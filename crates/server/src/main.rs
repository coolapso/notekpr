use std::net::SocketAddr;
use std::path::Path;

use anyhow::{Context, bail};
use clap::Parser;
use coolpsonotes_server::config::{Cli, Command, Config, LogFormat};
use coolpsonotes_server::db;
use tracing_subscriber::EnvFilter;

fn init_logging(format: LogFormat) {
    let filter = EnvFilter::try_from_env("COOLPSONOTES_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn"));
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
                        "coolpsonotes-{:04}{:02}{:02}-{:02}{:02}{:02}.db",
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
                            .is_some_and(|n| n.starts_with("coolpsonotes-") && n.ends_with(".db"))
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
        Command::Healthcheck => healthcheck(cli.config.listen),
    }
}

async fn serve(config: Config) -> anyhow::Result<()> {
    if !config.oidc_enabled() && !config.dev_mode {
        bail!(
            "no login method configured: set --oidc-issuer and --oidc-client-id \
             (COOLPSONOTES_OIDC_ISSUER / COOLPSONOTES_OIDC_CLIENT_ID), or --dev-mode for local development"
        );
    }
    if config.dev_mode {
        tracing::warn!(
            "DEVELOPMENT MODE: anyone who can reach this server can sign in as any user name without a password"
        );
        if !config.listen.ip().is_loopback() {
            tracing::warn!(listen = %config.listen, "development mode is listening on a non-loopback address");
        }
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
    let (app, _state) = coolpsonotes_server::build(config, metrics).await?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    tracing::info!(%listen, %public_url, version = env!("CARGO_PKG_VERSION"), "coolpsonotes server ready");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
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
