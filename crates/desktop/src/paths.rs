//! Where the app keeps things, per profile (`--profile`, `default` unless
//! given):
//!
//! * `<config>/notekpr-desktop/<profile>/settings.json`: the server and the
//!   app's settings ([`crate::settings`]). Nothing secret.
//! * `<data>/notekpr-desktop/<profile>/webview/`: the window's storage, as a
//!   browser profile holds the web app's (the device key, IndexedDB copies).
//! * `<data>/notekpr-desktop/<profile>/folder/`: the Markdown folder's CLI
//!   profile ([`crate::folder`]): its session, sealed keys, local copies.
//!
//! `<config>` and `<data>` are the XDG base directories on Linux, `%APPDATA%`
//! and `%LOCALAPPDATA%` on Windows, `~/Library/Application Support` on
//! macOS. `NOTEKPR_DESKTOP_CONFIG_DIR` and `NOTEKPR_DESKTOP_DATA_DIR`
//! replace `<config>/notekpr-desktop` and `<data>/notekpr-desktop`.

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use notekpr_cli::profile::Dirs;

const APP_DIR: &str = "notekpr-desktop";

#[derive(Clone, Debug)]
pub struct Paths {
    /// This profile's settings folder.
    pub config: PathBuf,
    /// This profile's data folder.
    pub data: PathBuf,
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home() -> Result<PathBuf> {
    env_path("HOME")
        .or_else(|| env_path("USERPROFILE"))
        .ok_or_else(|| anyhow!("cannot determine the home directory"))
}

fn base(xdg: &str, windows: &str, fallback: &[&str]) -> Result<PathBuf> {
    if let Some(p) = env_path(xdg) {
        return Ok(p);
    }
    if cfg!(windows)
        && let Some(p) = env_path(windows)
    {
        return Ok(p);
    }
    let mut p = home()?;
    if cfg!(target_os = "macos") {
        p.push("Library");
        p.push("Application Support");
    } else {
        p.extend(fallback);
    }
    Ok(p)
}

impl Paths {
    pub fn for_profile(profile: &str) -> Result<Paths> {
        let config = match env_path("NOTEKPR_DESKTOP_CONFIG_DIR") {
            Some(p) => p,
            None => base("XDG_CONFIG_HOME", "APPDATA", &[".config"])?.join(APP_DIR),
        };
        let data = match env_path("NOTEKPR_DESKTOP_DATA_DIR") {
            Some(p) => p,
            None => base("XDG_DATA_HOME", "LOCALAPPDATA", &[".local", "share"])?.join(APP_DIR),
        };
        Ok(Paths {
            config: config.join(profile),
            data: data.join(profile),
        })
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.json")
    }

    /// The window's storage for `server`: one per server, so nothing one
    /// server's session left (its token, device key, copies) is ever sent
    /// to another.
    pub fn webview(&self, server: Option<&str>) -> PathBuf {
        let name = match server {
            None => "none".to_string(),
            Some(s) => {
                use sha2::Digest;
                let host: String = url::Url::parse(s)
                    .ok()
                    .and_then(|u| u.host_str().map(str::to_string))
                    .unwrap_or_default()
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '.' {
                            c
                        } else {
                            '-'
                        }
                    })
                    .take(64)
                    .collect();
                let hash = data_encoding::HEXLOWER.encode(&sha2::Sha256::digest(s.as_bytes()));
                format!("{host}-{}", &hash[..12])
            }
        };
        self.data.join("webview").join(name)
    }

    /// The Markdown folder's CLI profile lives under this root.
    pub fn folder(&self) -> Dirs {
        Dirs::under(&self.data.join("folder"))
    }
}

/// Profile names: what the CLI allows for its own (`--profile`).
pub fn valid_profile(name: &str) -> bool {
    notekpr_cli::profile::valid_profile_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_storage_per_server() {
        let p = Paths {
            config: "/c".into(),
            data: "/d".into(),
        };
        let a = p.webview(Some("https://notes.example.com"));
        let b = p.webview(Some("https://notes.example.com/other"));
        let c = p.webview(Some("http://localhost:8080"));
        assert!(a.starts_with("/d/webview"));
        assert!(
            a.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("notes.example.com-")
        );
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_eq!(a, p.webview(Some("https://notes.example.com")));
        assert_eq!(p.webview(None), PathBuf::from("/d/webview/none"));
    }
}
