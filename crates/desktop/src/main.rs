//! `notekpr-desktop`: the notekpr desktop app.

// No console window next to the app on Windows (release builds).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use clap::Parser;
use tracing_subscriber::EnvFilter;

fn main() {
    let args = notekpr_desktop::Args::parse();
    // Logs go to stderr; NOTEKPR_LOG=debug for more.
    let filter = EnvFilter::try_from_env("NOTEKPR_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    if let Err(e) = notekpr_desktop::run(args) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
