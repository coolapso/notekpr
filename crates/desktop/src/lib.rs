//! The notekpr desktop app (`notekpr-desktop`): the web client
//! (`crates/web`), its files inside the app, in a window of its own, with a
//! tray, sync in the background, start at login, and a Markdown folder kept
//! in sync by the CLI's library. docs/desktop.md says what it does,
//! docs/security.md what it keeps where.

mod app;
mod commands;
pub mod folder;
pub mod links;
pub mod paths;
pub mod settings;
pub mod signin;
pub mod status;

use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(name = "notekpr-desktop", version = notekpr_core::VERSION, about = "notekpr, the desktop app")]
pub struct Args {
    /// Start in the tray without showing the window (how it starts at login).
    #[arg(long)]
    pub hidden: bool,
    /// Keep separate settings, sign-in and data under this name, so several
    /// people can be signed in side by side (as the CLI's --profile).
    #[arg(long, default_value = "default", value_parser = profile_name)]
    pub profile: String,
}

fn profile_name(s: &str) -> Result<String, String> {
    if paths::valid_profile(s) {
        Ok(s.to_string())
    } else {
        Err("letters, digits, - and _ only (at most 64)".into())
    }
}

pub fn run(args: Args) -> anyhow::Result<()> {
    app::run(args)
}
