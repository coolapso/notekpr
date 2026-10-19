//! `coolpsonotes`: desktop/CLI client for coolpsonotes.

use clap::Parser;
use tracing_subscriber::EnvFilter;

fn main() {
    let cli = coolpsonotes_client::cli::Cli::parse();
    // Logs go to stderr (stdout carries command output and the MCP protocol).
    // The default level only shows warnings; COOLPSONOTES_LOG=debug for more.
    let filter = EnvFilter::try_from_env("COOLPSONOTES_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).with_target(false).init();
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start the async runtime: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = rt.block_on(coolpsonotes_client::commands::run(cli)) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
