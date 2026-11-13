//! The app's settings: a JSON file in the profile's settings folder
//! ([`crate::paths`]). Nothing secret goes here. Whether the app starts at
//! login is the system's (its autostart entry), not a setting of ours; the
//! Markdown folder's own state is its CLI profile's ([`crate::folder`]).

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The server's address (`https://notes.example.com`), normalized like
    /// the CLI's. None until the person chose one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Closing the window hides it in the tray instead of quitting.
    #[serde(default = "yes")]
    pub close_to_tray: bool,
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            server: None,
            close_to_tray: true,
        }
    }
}

impl Settings {
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
            close_to_tray: false,
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
        // Settings added later keep their defaults in older files.
        std::fs::write(&path, br#"{"server":"https://n.example.com"}"#).unwrap();
        assert!(Settings::load(&path).unwrap().close_to_tray);
        std::fs::write(&path, b"{not json").unwrap();
        assert!(Settings::load(&path).is_err());
    }
}
