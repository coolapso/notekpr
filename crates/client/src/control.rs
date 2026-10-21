//! Changing commands while a daemon owns the profile.
//!
//! A running `sync` or `gateway` owns the profile's local copies, so a
//! one-shot command that changes notes can't open them itself. Instead it
//! hands the command to that process: over a loopback connection whose port
//! and random token are in `control.json` (readable by the user only), like
//! the gateway's. The daemon runs the command on its own copies and sends
//! back what it printed; the command's process prints that, as if it had run
//! the command itself.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::cli::{Cli, Command, QuickArgs};
use crate::fsutil;
use crate::hub::Hub;
use crate::profile::Dirs;
use crate::ui;

/// Largest request (a command with what it reads from stdin, e.g. a file
/// written as an attachment).
const MAX_REQUEST: u64 = 256 << 20;

#[derive(Serialize, Deserialize)]
struct Endpoint {
    port: u16,
    token: String,
}

#[derive(Serialize, Deserialize)]
struct Request {
    token: String,
    cli: Cli,
    #[serde(default, with = "coolpsonotes_core::b64::opt")]
    stdin: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize, Default)]
struct Reply {
    stdout: String,
    stderr: String,
    /// The command failed with this error.
    error: Option<String>,
}

fn endpoint_path(dirs: &Dirs, profile: &str) -> PathBuf {
    dirs.profile_data(profile).join("control.json")
}

/// Commands a daemon runs for others: those that change the profile's local
/// copies — but never `sync` itself, nor signing out.
pub fn forwardable(c: &Command) -> bool {
    !matches!(c, Command::Sync(_) | Command::Logout { .. })
        && crate::commands::changes_local_copies(c)
}

/// The control endpoint of a running daemon; gone when dropped.
pub struct Control {
    path: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Control {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Run changing commands for others on `hub`, the daemon's own copies.
pub async fn serve(dirs: &Dirs, profile: &str, hub: Arc<Hub>) -> Result<Control> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("opening the local control endpoint")?;
    let port = listener.local_addr()?.port();
    let token = crate::gateway::new_token();
    let path = endpoint_path(dirs, profile);
    let endpoint = serde_json::to_vec(&Endpoint {
        port,
        token: token.clone(),
    })?;
    fsutil::write_atomic(&path, &endpoint, true)
        .with_context(|| format!("writing {}", path.display()))?;
    let (dirs, profile) = (dirs.clone(), profile.to_string());
    let task = tokio::spawn(async move {
        while let Ok((conn, _)) = listener.accept().await {
            let (dirs, profile, hub, token) =
                (dirs.clone(), profile.clone(), hub.clone(), token.clone());
            tokio::spawn(async move {
                if let Err(e) = answer(conn, &dirs, &profile, hub, &token).await {
                    tracing::debug!("control: {e:#}");
                }
            });
        }
    });
    Ok(Control { path, task })
}

async fn answer(
    mut conn: TcpStream,
    dirs: &Dirs,
    profile: &str,
    hub: Arc<Hub>,
    token: &str,
) -> Result<()> {
    let mut raw = Vec::new();
    (&mut conn).take(MAX_REQUEST).read_to_end(&mut raw).await?;
    let req: Request = serde_json::from_slice(&raw).context("reading a control request")?;
    if !bool::from(req.token.as_bytes().ct_eq(token.as_bytes())) {
        return Err(anyhow!("a control request with a wrong token"));
    }
    let reply = if req.cli.profile != profile || !forwardable(&req.cli.command) {
        Reply {
            error: Some("this process doesn't run that command".into()),
            ..Default::default()
        }
    } else if hub.read_only {
        // Everything handed over changes something.
        Reply {
            error: Some("the running gateway is read-only: it makes no changes".into()),
            ..Default::default()
        }
    } else {
        let (result, out) =
            crate::commands::run_for_other(req.cli, dirs.clone(), hub, req.stdin).await;
        Reply {
            stdout: out.stdout,
            stderr: out.stderr,
            error: result.err().map(|e| format!("{e:#}")),
        }
    };
    conn.write_all(&serde_json::to_vec(&reply)?).await?;
    conn.shutdown().await?;
    Ok(())
}

/// Hand `cli` to the daemon that owns its profile, and print what it
/// printed. `Ok(false)`: no daemon takes commands (none running, or an older
/// one) — the caller says the profile is in use, as before.
pub async fn forward(dirs: &Dirs, mut cli: Cli) -> Result<bool> {
    if !forwardable(&cli.command) {
        return Ok(false);
    }
    let Some(raw) = fsutil::read_optional(&endpoint_path(dirs, &cli.profile))? else {
        return Ok(false);
    };
    let Ok(endpoint) = serde_json::from_slice::<Endpoint>(&raw) else {
        return Ok(false);
    };
    let connect = TcpStream::connect(("127.0.0.1", endpoint.port));
    let Ok(Ok(mut conn)) = tokio::time::timeout(Duration::from_secs(2), connect).await else {
        // A daemon that ended without cleaning up.
        return Ok(false);
    };
    // What the command would read here: stdin, and paths relative to here.
    let stdin = match &cli.command {
        Command::Write { .. } => Some(ui::read_stdin().context("reading stdin")?),
        Command::Quick(QuickArgs { text, list, .. }) if text.is_empty() && !list => {
            if ui::stdin_is_tty() {
                eprintln!("Type your quick note, then press Ctrl+D:");
            }
            Some(ui::read_stdin().context("reading stdin")?)
        }
        _ => None,
    };
    if let Command::Import { source, .. } = &mut cli.command {
        *source = absolute(source)?;
    }
    let quiet = cli.quiet;
    let req = Request {
        token: endpoint.token,
        cli,
        stdin,
    };
    conn.write_all(&serde_json::to_vec(&req)?).await?;
    conn.shutdown().await?;
    let mut raw = Vec::new();
    conn.read_to_end(&mut raw).await?;
    let reply: Reply = serde_json::from_slice(&raw)
        .context("the running coolpsonotes process didn't answer properly")?;
    if !quiet {
        eprintln!("(run by the coolpsonotes process that keeps this profile in sync)");
    }
    eprint!("{}", reply.stderr);
    print!("{}", reply.stdout);
    match reply.error {
        Some(e) => Err(anyhow!(e)),
        None => Ok(true),
    }
}

fn absolute(p: &Path) -> Result<PathBuf> {
    Ok(if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()?.join(p)
    })
}
