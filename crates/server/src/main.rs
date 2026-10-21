use std::net::SocketAddr;
use std::path::Path;

use anyhow::{Context, bail};
use clap::Parser;
use notekpr_server::config::{Cli, Command, Config, LogFormat};
use notekpr_server::db;
use tracing_subscriber::EnvFilter;

fn init_logging(format: LogFormat) {
    let filter =
        EnvFilter::try_from_env("NOTEKPR_LOG").unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false);
    match format {
        LogFormat::Json => builder.json().flatten_event(true).init(),
        LogFormat::Pretty => builder.init(),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut cli = Cli::parse();
    cli.config.normalize();
    init_logging(cli.config.log_format);
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => serve(cli.config).await,
        Command::Migrate => {
            let path = cli.config.db_path();
            let pool = db::connect(&path).await?;
            db::migrate(&pool, &path).await?;
            println!("database at {} is up to date", path.display());
            Ok(())
        }
        Command::Backup { out, out_dir, keep } => {
            let path = cli.config.db_path();
            anyhow::ensure!(path.exists(), "no database at {}", path.display());
            let out = match (out, &out_dir) {
                (Some(o), _) => o,
                (None, Some(dir)) => {
                    std::fs::create_dir_all(dir)?;
                    let ts = time::OffsetDateTime::now_utc();
                    dir.join(format!(
                        "notekpr-{:04}{:02}{:02}-{:02}{:02}{:02}.db",
                        ts.year(),
                        ts.month() as u8,
                        ts.day(),
                        ts.hour(),
                        ts.minute(),
                        ts.second()
                    ))
                }
                (None, None) => bail!("pass --out FILE or --out-dir DIR"),
            };
            let pool = db::connect(&path).await?;
            db::backup_to(&pool, &out).await?;
            println!("backup written to {}", out.display());
            if let (Some(dir), Some(keep)) = (out_dir, keep) {
                let mut files: Vec<_> = std::fs::read_dir(&dir)?
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with("notekpr-") && n.ends_with(".db"))
                    })
                    .collect();
                files.sort();
                while files.len() > keep.max(1) {
                    let old = files.remove(0);
                    std::fs::remove_file(&old)?;
                    println!("removed old backup {}", old.display());
                }
            }
            Ok(())
        }
        Command::Restore { from } => restore(&cli.config, &from).await,
        Command::ExportUser { user, backup, out } => {
            export_user(&cli.config, &user, backup.as_deref(), &out).await
        }
        Command::Healthcheck => healthcheck(cli.config.listen),
    }
}

/// Export one person's encrypted data from the live database or a backup.
async fn export_user(
    config: &Config,
    user: &str,
    backup: Option<&std::path::Path>,
    out: &std::path::Path,
) -> anyhow::Result<()> {
    anyhow::ensure!(!out.exists(), "{} already exists", out.display());
    // A backup may be from an older version: work on a migrated copy so the
    // backup itself is never modified.
    let scratch = std::env::temp_dir().join(format!("notekpr-export-{}", db::new_id()));
    let path = match backup {
        Some(b) => {
            std::fs::create_dir_all(&scratch)?;
            let copy = scratch.join("notekpr.db");
            std::fs::copy(b, &copy).with_context(|| format!("reading {}", b.display()))?;
            copy
        }
        None => config.db_path(),
    };
    anyhow::ensure!(path.exists(), "no database at {}", path.display());
    let result = async {
        let pool = db::connect(&path).await?;
        if backup.is_some() {
            db::migrate(&pool, &path).await?;
        }
        let id = find_user(&pool, user).await?;
        let export = notekpr_server::export::user_export(
            &pool,
            &notekpr_server::blobs::Blobs::Sqlite(pool.clone()),
            &id,
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let json = serde_json::to_vec(&export)?;
        write_private(out, &json)?;
        println!(
            "wrote {} ({} vaults, {} shared notes) for user {id}",
            out.display(),
            export.vaults.len(),
            export.shared_notes.len()
        );
        anyhow::Ok(())
    }
    .await;
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

/// A user by id, exact e-mail or exact display name (case-insensitive).
async fn find_user(pool: &db::Db, user: &str) -> anyhow::Result<String> {
    let ids = notekpr_server::repo::users::find(pool, user).await?;
    match ids.as_slice() {
        [one] => Ok(one.clone()),
        [] => bail!("no user {user:?} in this database"),
        _ => bail!("{user:?} matches several users; pass the user id"),
    }
}

fn write_private(path: &std::path::Path, data: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    f.write_all(data)?;
    f.sync_all()?;
    Ok(())
}

async fn serve(config: Config) -> anyhow::Result<()> {
    if !config.oidc_enabled() && !config.dev_mode {
        bail!(
            "no login method configured: set --oidc-issuer and --oidc-client-id \
             (NOTEKPR_OIDC_ISSUER / NOTEKPR_OIDC_CLIENT_ID), or --dev-mode for local development"
        );
    }
    if config.demo_resets_at.is_some() && !config.dev_mode {
        bail!("--demo-resets-at is for demos: it needs --dev-mode");
    }
    if config.dev_mode {
        // Anyone who can reach a dev-mode server can sign in as anyone:
        // refuse setups that look like they are meant to be reachable.
        let risky = [
            (
                !config.listen.ip().is_loopback(),
                "it listens on a non-loopback address",
            ),
            (
                config.public_url.starts_with("https://"),
                "its public URL is https",
            ),
            (config.oidc_enabled(), "OIDC sign-in is configured too"),
        ];
        let reasons: Vec<&str> = risky
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, r)| *r)
            .collect();
        if !reasons.is_empty() && !config.dev_mode_anywhere {
            bail!(
                "refusing to start in development mode because {}. Development mode lets anyone sign in as anyone. If this server really is reachable only by you (e.g. a container published on 127.0.0.1), add --dev-mode-anywhere (NOTEKPR_DEV_MODE_ANYWHERE=true).",
                reasons.join(" and ")
            );
        }
        tracing::warn!(
            "DEVELOPMENT MODE: anyone who can reach this server can sign in as any user name without a password"
        );
        for r in reasons {
            tracing::warn!("development mode allowed although {r}");
        }
    }
    match notekpr_server::process::raise_open_files_limit() {
        // Each connected person needs about three: a WebSocket and HTTP
        // connections.
        Some(n) if n < 8192 => tracing::warn!(
            open_files = n,
            "the open-files limit allows only about {} people connected at once; raise it (ulimit -n, or the container's nofile ulimit)",
            n / 3
        ),
        Some(n) => tracing::info!(open_files = n, "open-files limit"),
        None => {}
    }
    if !config.public_url.starts_with("https://") && !is_local_url(&config.public_url) {
        tracing::warn!(public_url = %config.public_url, "public URL is not HTTPS; terminate TLS at a reverse proxy");
    }
    let metrics = if config.metrics {
        Some(
            metrics_exporter_prometheus::PrometheusBuilder::new()
                .install_recorder()
                .context("installing metrics recorder")?,
        )
    } else {
        None
    };
    let listen = config.listen;
    let public_url = config.public_url();
    let (app, state) = notekpr_server::build(config, metrics).await?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    tracing::info!(%listen, %public_url, version = env!("CARGO_PKG_VERSION"), "notekpr server ready");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown({
        let state = state.clone();
        async move {
            shutdown_signal().await;
            tracing::info!("shutting down: live connections are told it's a restart");
            state.shutting_down.send_replace(true);
        }
    })
    .await?;
    // No new connections and every request answered; WebSockets aren't
    // among what the server waits for, so give their closes a moment.
    state
        .close_connections(std::time::Duration::from_secs(3))
        .await;
    tracing::info!("shut down cleanly");
    Ok(())
}

/// Minimal HTTP probe so distroless images need no curl or shell.
fn healthcheck(listen: SocketAddr) -> anyhow::Result<()> {
    use std::io::{Read, Write};
    let mut addr = listen;
    if addr.ip().is_unspecified() {
        addr.set_ip(std::net::Ipv4Addr::LOCALHOST.into());
    }
    let mut s = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(3))?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(3)))?;
    write!(
        s,
        "GET /readyz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )?;
    let mut buf = String::new();
    s.read_to_string(&mut buf)?;
    if buf.starts_with("HTTP/1.1 200") {
        Ok(())
    } else {
        bail!("not ready: {}", buf.lines().next().unwrap_or(""))
    }
}

fn is_local_url(u: &str) -> bool {
    ["http://localhost", "http://127.", "http://[::1]"]
        .iter()
        .any(|p| u.starts_with(p))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
}

/// Replace the live database with a verified backup, keeping the old one.
async fn restore(config: &Config, from: &Path) -> anyhow::Result<()> {
    let target = config.db_path();
    if let Some(aside) = db::restore_from(&target, from).await? {
        println!("previous database moved to {}", aside.display());
    }
    println!("restored {} -> {}", from.display(), target.display());
    Ok(())
}
