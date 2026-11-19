//! The notekpr desktop app (`notekpr-desktop`): the web client
//! (`crates/web`), its files inside the app, in a window of its own, with a
//! tray, sync in the background, start at login, and a Markdown folder kept
//! in sync by the CLI's library. docs/desktop.md says what it does,
//! docs/security.md what it keeps where.

mod app;
mod commands;
pub mod flatpak;
pub mod folder;
pub mod from_cli;
pub mod links;
pub mod local;
pub mod notify;
pub mod paths;
pub mod settings;
pub mod signin;
pub mod status;
#[cfg(all(test, target_os = "linux"))]
mod testbus;
pub mod update;

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(name = "notekpr-desktop", version = notekpr_core::VERSION, about = "notekpr, the desktop app")]
pub struct Args {
    /// Start in the tray without showing the window (how it starts at login).
    #[arg(long)]
    pub hidden: bool,
    /// Have the copy of the app running with this profile (and folders)
    /// quit, as Quit in its tray does, and return at once; it quits once
    /// its Markdown folder's sync stopped. Nothing happens when none runs.
    /// The Windows installer uses it before replacing or removing the app.
    #[arg(long, conflicts_with = "hidden")]
    pub quit: bool,
    /// Keep separate settings, sign-in and data under this name, so several
    /// people can be signed in side by side (as the CLI's --profile).
    #[arg(long, default_value = "default", value_parser = profile_name)]
    pub profile: String,
    /// Keep settings in this folder instead of the system's (as
    /// NOTEKPR_DESKTOP_CONFIG_DIR): a copy of the app with folders of its
    /// own, beside the installed one (development runs).
    #[arg(long)]
    pub config_dir: Option<PathBuf>,
    /// Keep data in this folder instead of the system's (as
    /// NOTEKPR_DESKTOP_DATA_DIR).
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
}

impl Args {
    /// Folders of its own, from `--config-dir` / `--data-dir` or the
    /// variables: made, made absolute, and set in the variables, which the
    /// CLI's library reads (`notekpr_cli::profile::desktop_roots`).
    ///
    /// # Safety
    ///
    /// Call it first thing in `main`, while the program has one thread:
    /// it sets environment variables.
    pub unsafe fn take_own_folders(&self) -> anyhow::Result<()> {
        use anyhow::Context;
        for (flag, var) in [
            (&self.config_dir, paths::CONFIG_VAR),
            (&self.data_dir, paths::DATA_VAR),
        ] {
            let given = flag.clone().or_else(|| {
                std::env::var_os(var)
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
            });
            if let Some(dir) = given {
                std::fs::create_dir_all(&dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
                let dir = dir.canonicalize()?;
                // SAFETY: the caller's: one thread so far.
                unsafe { std::env::set_var(var, &dir) };
            }
        }
        Ok(())
    }
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
