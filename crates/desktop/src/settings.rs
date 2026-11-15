//! The app's settings: a JSON file in the profile's settings folder
//! ([`crate::paths`]). Nothing secret goes here. Whether the app starts at
//! login is the system's (its autostart entry), not a setting of ours — but
//! in the Flatpak, where only the portal reaches that entry, which can't be
//! asked about it ([`crate::flatpak`]). The Markdown folder's own state is
//! its CLI profile's ([`crate::folder`]).
//!
//! The app's accounts (docs/desktop.md "Accounts"): one per server it was
//! added for, and *On this computer*, each with a random id that names its
//! window storage and Markdown folder ([`crate::paths`]). One is open; the
//! app starts again to open another. They are kept in `accounts.json`
//! beside `settings.json`, which 0.17.0 rewrites without them; `server` and
//! `this_computer` there say which is open as 0.17.0 read it (one server).

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The id of the account *On this computer* (its window storage had that
/// name before accounts).
pub const THIS_COMPUTER: &str = "this-computer";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Account {
    /// Names its window storage and Markdown folder: random for an account
    /// added since accounts, else the name its window storage had already.
    pub id: String,
    /// Its server, normalized like the CLI's; none: only on this computer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// The name its window last reported, shown beside the server: a label,
    /// nothing more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Removed from the list; what the app keeps for it stays.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// Its initial setup was shown (docs/desktop.md "Getting started"):
    /// false for accounts from before there was one, so they get it once.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub set_up: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The open account's server (`https://notes.example.com`), normalized
    /// like the CLI's; with notes only on this computer, the server last
    /// open. None until the person chose one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// The open account is *On this computer*: the app runs the server
    /// inside itself ([`crate::local`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub this_computer: bool,
    /// The open account's id (in `accounts.json`).
    #[serde(skip)]
    pub account: Option<String>,
    /// Every account, in the order added; removed ones too (hidden). In
    /// `accounts.json`.
    #[serde(skip)]
    pub accounts: Vec<Account>,
    /// Closing the window hides it in the tray instead of quitting.
    #[serde(default = "yes")]
    pub close_to_tray: bool,
    /// Starting at login, as the Background portal last confirmed it: only
    /// in the Flatpak ([`crate::flatpak::request_autostart`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub autostart: bool,
    /// The initial setup asked about the app's own settings (the tray,
    /// starting at login): asked once, not for each account.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub background_asked: bool,
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            server: None,
            this_computer: false,
            account: None,
            accounts: vec![],
            close_to_tray: true,
            autostart: false,
            background_asked: false,
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

    /// The open account, once the settings agree on it: an older file (no
    /// list) becomes a list of one, with the id its window storage had
    /// (`legacy_id`); `server`/`this_computer` win over `account` when they
    /// disagree (a file an older app wrote). None before one was chosen.
    pub fn settle(&mut self, legacy_id: impl Fn(&str) -> String) -> Option<Account> {
        self.accounts
            .retain(|a| crate::paths::valid_account_id(&a.id));
        let open = if self.this_computer {
            Some(None)
        } else {
            self.server.clone().map(Some)
        };
        let Some(server) = open else {
            self.account = None;
            return None;
        };
        let agrees = self.account.as_ref().and_then(|id| {
            self.accounts
                .iter()
                .find(|a| &a.id == id && a.server == server)
        });
        let id = match agrees.or_else(|| self.find(server.as_deref())) {
            Some(a) => a.id.clone(),
            None => {
                let mut id = match &server {
                    Some(s) => legacy_id(s),
                    None => THIS_COMPUTER.to_string(),
                };
                // Never another account's (its storage).
                if self.get(&id).is_some()
                    && let Some(s) = &server
                {
                    id = crate::paths::new_account_id(s);
                }
                self.accounts.push(Account {
                    id: id.clone(),
                    server,
                    name: None,
                    hidden: false,
                    set_up: false,
                });
                id
            }
        };
        self.open(&id);
        self.get(&id).cloned()
    }

    pub fn get(&self, id: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }

    /// The account on `server` (none: on this computer), hidden or not.
    pub fn find(&self, server: Option<&str>) -> Option<&Account> {
        self.accounts.iter().find(|a| a.server.as_deref() == server)
    }

    /// The accounts in the list.
    pub fn visible(&self) -> impl Iterator<Item = &Account> {
        self.accounts.iter().filter(|a| !a.hidden)
    }

    /// The account on `server` (none: on this computer), in the list again
    /// if it was removed, or a new one; its id.
    pub fn add(&mut self, server: Option<String>, new_id: impl FnOnce() -> String) -> String {
        if let Some(a) = self.accounts.iter_mut().find(|a| a.server == server) {
            a.hidden = false;
            return a.id.clone();
        }
        let id = match &server {
            Some(_) => new_id(),
            None => THIS_COMPUTER.to_string(),
        };
        self.accounts.push(Account {
            id: id.clone(),
            server,
            name: None,
            hidden: false,
            set_up: false,
        });
        id
    }

    /// Make `id` the open account (in the list); false if there is none.
    pub fn open(&mut self, id: &str) -> bool {
        let Some(a) = self.accounts.iter_mut().find(|a| a.id == id) else {
            return false;
        };
        a.hidden = false;
        match a.server.clone() {
            Some(s) => {
                self.server = Some(s);
                self.this_computer = false;
            }
            None => self.this_computer = true,
        }
        self.account = Some(id.to_string());
        true
    }

    /// The initial setup was shown for account `id`, and with it the
    /// question about the app's own settings.
    pub fn set_up(&mut self, id: &str) {
        if let Some(a) = self.accounts.iter_mut().find(|a| a.id == id) {
            a.set_up = true;
        }
        self.background_asked = true;
    }

    /// Take `id` off the list: never the open one, nor this computer's.
    pub fn hide(&mut self, id: &str) -> bool {
        if self.account.as_deref() == Some(id) || id == THIS_COMPUTER {
            return false;
        }
        match self.accounts.iter_mut().find(|a| a.id == id) {
            Some(a) => {
                a.hidden = true;
                true
            }
            None => false,
        }
    }

    /// The settings in `path`; the defaults when there are none yet. A file
    /// that can't be read is an error, never quietly replaced.
    pub fn load(path: &Path) -> Result<Settings> {
        let mut s: Settings = read(path)?.unwrap_or_default();
        if let Some(a) = read::<Accounts>(&accounts_file(path))? {
            s.account = a.account;
            s.accounts = a.accounts;
        }
        Ok(s)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        write(
            &accounts_file(path),
            &Accounts {
                account: self.account.clone(),
                accounts: self.accounts.clone(),
            },
        )?;
        write(path, self)
    }
}

/// `accounts.json`: the accounts, apart from the settings 0.17.0 rewrites.
#[derive(Serialize, Deserialize, Default)]
struct Accounts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account: Option<String>,
    #[serde(default)]
    accounts: Vec<Account>,
}

fn accounts_file(settings: &Path) -> std::path::PathBuf {
    settings.with_file_name("accounts.json")
}

/// A file's JSON; none when there is no file. One that can't be read is an
/// error, never quietly replaced.
fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .with_context(|| format!("reading the settings in {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut json = serde_json::to_vec_pretty(value)?;
    json.push(b'\n');
    notekpr_cli::fsutil::write_atomic(path, &json, false)
        .with_context(|| format!("writing {}", path.display()))
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
            account: Some(THIS_COMPUTER.into()),
            accounts: vec![Account {
                id: THIS_COMPUTER.into(),
                server: None,
                name: Some("ana".into()),
                hidden: false,
                set_up: true,
            }],
            close_to_tray: false,
            autostart: true,
            background_asked: true,
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

    fn legacy(s: &str) -> String {
        format!(
            "legacy-{}",
            s.trim_start_matches("https://").replace('.', "-")
        )
    }

    #[test]
    fn an_older_file_becomes_a_list_of_one() {
        // 0.17.0's: one server.
        let mut s: Settings =
            serde_json::from_str(r#"{"server":"https://a.example.com"}"#).unwrap();
        let open = s.settle(legacy).unwrap();
        // Its window storage keeps the name it had.
        assert_eq!(open.id, legacy("https://a.example.com"));
        assert_eq!(open.server.as_deref(), Some("https://a.example.com"));
        assert_eq!(s.account.as_deref(), Some(open.id.as_str()));
        assert_eq!(s.visible().count(), 1);
        // Settled again: the same.
        let again = s.clone().settle(legacy).unwrap();
        assert_eq!(again, open);
        // Nothing chosen yet: no account.
        assert_eq!(Settings::default().settle(legacy), None);
        // Only on this computer (#61's file).
        let mut s: Settings =
            serde_json::from_str(r#"{"server":"https://a.example.com","this_computer":true}"#)
                .unwrap();
        assert_eq!(s.settle(legacy).unwrap().id, THIS_COMPUTER);
    }

    #[test]
    fn the_accounts_outlive_0_17_rewriting_its_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut s = Settings::default();
        let a = s.add(Some("https://a.example.com".into()), || "a-random".into());
        let b = s.add(Some("https://b.example.com".into()), || "b-random".into());
        s.open(&a);
        s.save(&path).unwrap();
        // 0.17.0 knows its fields only: it writes them alone, here after
        // the person chose b's server in it.
        std::fs::write(
            &path,
            br#"{"server":"https://b.example.com","close_to_tray":false}"#,
        )
        .unwrap();
        let mut back = Settings::load(&path).unwrap();
        let open = back.settle(legacy).unwrap();
        // b, with its own id (its storage), not one made up again.
        assert_eq!(open.id, b);
        assert_eq!(back.visible().count(), 2);
        assert!(!back.close_to_tray);
    }

    #[test]
    fn accounts_added_switched_and_removed() {
        let mut s: Settings =
            serde_json::from_str(r#"{"server":"https://a.example.com"}"#).unwrap();
        let a = s.settle(legacy).unwrap().id;
        let b = s.add(Some("https://b.example.com".into()), || "rnd-b".into());
        assert_eq!(b, "rnd-b");
        // The same server again: the same account.
        assert_eq!(
            s.add(Some("https://b.example.com".into()), || unreachable!()),
            b
        );
        let here = s.add(None, || unreachable!());
        assert_eq!(here, THIS_COMPUTER);
        assert!(s.open(&b));
        assert_eq!(
            (s.server.as_deref(), s.this_computer),
            (Some("https://b.example.com"), false)
        );
        assert!(s.open(&here));
        // On this computer, the server last open stays (0.17.0 reads it).
        assert_eq!(
            (s.server.as_deref(), s.this_computer),
            (Some("https://b.example.com"), true)
        );
        assert!(!s.open("nobody"));
        // Removing: never the open one or this computer's; it only hides.
        assert!(!s.hide(&here));
        assert!(s.hide(&a));
        assert_eq!(s.visible().count(), 2);
        assert!(s.get(&a).unwrap().hidden);
        // Added again: found, with its id (and its storage).
        assert_eq!(
            s.add(Some("https://a.example.com".into()), || unreachable!()),
            a
        );
        assert_eq!(s.visible().count(), 3);
        // An older app wrote the file (no `account`, another server): it
        // wins, and gets an account.
        let mut older = s.clone();
        older.server = Some("https://c.example.com".into());
        older.this_computer = false;
        let c = older.settle(legacy).unwrap();
        assert_eq!(c.server.as_deref(), Some("https://c.example.com"));
        assert_eq!(older.visible().count(), 4);
        // An id taken already (its storage) is never given to another.
        let mut taken = s.clone();
        taken.server = Some("https://d.example.com".into());
        taken.this_computer = false;
        let d = taken.settle(|_| a.clone()).unwrap();
        assert_ne!(d.id, a);
        assert_eq!(
            taken.get(&a).unwrap().server.as_deref(),
            Some("https://a.example.com")
        );
        // An id that would name a folder elsewhere is dropped.
        let mut bad = s.clone();
        bad.accounts.push(Account {
            id: "../x".into(),
            server: Some("https://x".into()),
            name: None,
            hidden: false,
            set_up: false,
        });
        bad.settle(legacy);
        assert!(bad.get("../x").is_none());
    }

    #[test]
    fn each_account_is_set_up_once_and_the_app_asked_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // An account from before there was a setup: not set up yet.
        std::fs::write(&path, br#"{"server":"https://a.example.com"}"#).unwrap();
        std::fs::write(
            accounts_file(&path),
            br#"{"account":"a1","accounts":[{"id":"a1","server":"https://a.example.com"}]}"#,
        )
        .unwrap();
        let mut s = Settings::load(&path).unwrap();
        s.settle(legacy);
        assert!(!s.get("a1").unwrap().set_up && !s.background_asked);
        let b = s.add(Some("https://b.example.com".into()), || "b1".into());
        assert!(!s.get(&b).unwrap().set_up);
        s.set_up("a1");
        s.save(&path).unwrap();
        let s = Settings::load(&path).unwrap();
        assert!(s.get("a1").unwrap().set_up);
        // The app's own question is asked once; b's setup is still to come.
        assert!(s.background_asked);
        assert!(!s.get(&b).unwrap().set_up);
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
