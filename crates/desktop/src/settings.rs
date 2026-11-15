//! The app's settings: a JSON file in the profile's settings folder
//! ([`crate::paths`]). Nothing secret goes here. Whether the app starts at
//! login is the system's (its autostart entry), not a setting of ours — but
//! in the Flatpak, where only the portal reaches that entry, which can't be
//! asked about it ([`crate::flatpak`]). The Markdown folder's own state is
//! its CLI profile's ([`crate::folder`]).

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The server's address (`https://notes.example.com`), normalized like
    /// the CLI's. None until the person chose one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// The notes are only on this computer: the app runs the server inside
    /// itself ([`crate::local`]). `server` stays, for going back to it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub this_computer: bool,
    /// Closing the window hides it in the tray instead of quitting.
    #[serde(default = "yes")]
    pub close_to_tray: bool,
    /// Starting at login, as the Background portal last confirmed it: only
    /// in the Flatpak ([`crate::flatpak::request_autostart`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub autostart: bool,
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            server: None,
            this_computer: false,
            close_to_tray: true,
            autostart: false,
        }
    }
}

impl Settings {
    /// Where the notes are has been chosen: a server, or this computer.
    pub fn chosen(&self) -> bool {
        self.this_computer || self.server.is_some()
    }

    /// The server the notes are on, when they aren't only on this computer.
    pub fn remote(&self) -> Option<&str> {
        self.server.as_deref().filter(|_| !self.this_computer)
    }

    /// The settings in `path`; the defaults when there are none yet. A file
    /// that can't be read is an error, never quietly replaced.
    pub fn load(path: &Path) -> Result<Settings> {
        match std::fs::read(path) {
            Ok(b) => serde_json::from_slice(&b)
                .with_context(|| format!("reading the settings in {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let mut json = serde_json::to_vec_pretty(self)?;
        json.push(b'\n');
        notekpr_cli::fsutil::write_atomic(path, &json, false)
            .with_context(|| format!("writing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p").join("settings.json");
        let s = Settings::load(&path).unwrap();
        assert_eq!(s, Settings::default());
        assert!(s.close_to_tray);
        let s = Settings {
            server: Some("https://notes.example.com".into()),
            this_computer: true,
            close_to_tray: false,
            autostart: true,
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
        // Outside the Flatpak it is never set, and never written.
        Settings::default().save(&path).unwrap();
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("autostart")
        );
        // Settings added later keep their defaults in older files.
        std::fs::write(&path, br#"{"server":"https://n.example.com"}"#).unwrap();
        assert!(Settings::load(&path).unwrap().close_to_tray);
        std::fs::write(&path, b"{not json").unwrap();
        assert!(Settings::load(&path).is_err());
    }

    #[test]
    fn a_server_or_this_computer() {
        let mut s = Settings::default();
        assert!(!s.chosen() && s.remote().is_none());
        s.server = Some("https://notes.example.com".into());
        assert!(s.chosen());
        assert_eq!(s.remote(), Some("https://notes.example.com"));
        // On this computer, the server is kept to go back to, not used.
        s.this_computer = true;
        assert!(s.chosen());
        assert_eq!(s.remote(), None);
        s.server = None;
        assert!(s.chosen());
    }
}
