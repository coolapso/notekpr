//! Where the app keeps things, per profile (`--profile`, `default` unless
//! given):
//!
//! * `<config>/notekpr-desktop/<profile>/settings.json`: the server and the
//!   app's settings ([`crate::settings`]). Nothing secret.
//! * `<data>/notekpr-desktop/<profile>/webview/<account>/`: an account's
//!   window storage, as a browser profile holds the web app's (the device
//!   key, IndexedDB copies); `<account>` is its id ([`crate::settings`]).
//! * `<data>/notekpr-desktop/<profile>/folders/<account>/`: an account's
//!   Markdown folder's CLI profile ([`crate::folder`]): its session, sealed
//!   keys, local copies (before accounts: `folder/`).
//! * `<data>/notekpr-desktop/<profile>/this-computer/`: notes only on this
//!   computer, the server's data ([`crate::local`]).
//!
//! `<config>` and `<data>` are the XDG base directories on Linux, `%APPDATA%`
//! and `%LOCALAPPDATA%` on Windows, `~/Library/Application Support` on
//! macOS. `NOTEKPR_DESKTOP_CONFIG_DIR` and `NOTEKPR_DESKTOP_DATA_DIR`
//! replace `<config>/notekpr-desktop` and `<data>/notekpr-desktop`.

use std::path::PathBuf;

use anyhow::Result;
use notekpr_cli::profile::Dirs;

/// The variables that give the app folders of its own.
pub const CONFIG_VAR: &str = "NOTEKPR_DESKTOP_CONFIG_DIR";
pub const DATA_VAR: &str = "NOTEKPR_DESKTOP_DATA_DIR";

/// A copy of the app with folders of its own (development runs, tests:
/// [`CONFIG_VAR`], [`DATA_VAR`], absolute by now): a short tag of them, so
/// its single-copy name and its autostart entry are apart from the
/// installed app's, and the arguments that start it with the same folders
/// (an autostart entry keeps arguments, not variables). None for the app
/// in the system's folders.
pub fn own_folders() -> Option<(String, Vec<String>)> {
    let given = |v: &str| std::env::var_os(v).filter(|v| !v.is_empty());
    own_folders_of(given(CONFIG_VAR), given(DATA_VAR))
}

fn own_folders_of(
    config: Option<std::ffi::OsString>,
    data: Option<std::ffi::OsString>,
) -> Option<(String, Vec<String>)> {
    use sha2::Digest;
    if config.is_none() && data.is_none() {
        return None;
    }
    let mut h = sha2::Sha256::new();
    let mut args = vec![];
    for (dir, flag) in [(&config, "--config-dir"), (&data, "--data-dir")] {
        h.update(
            dir.as_ref()
                .map(|d| d.as_encoded_bytes())
                .unwrap_or_default(),
        );
        h.update([0]);
        if let Some(d) = dir {
            args.extend([flag.to_string(), d.to_string_lossy().into_owned()]);
        }
    }
    let tag = data_encoding::HEXLOWER.encode(&h.finalize()[..4]);
    Some((tag, args))
}

/// An account id: a folder's name, nothing that leads elsewhere.
pub fn valid_account_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id != "none"
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// The host of `server` as a folder's name may hold it, for people looking
/// at the folders (nothing depends on it).
fn host_part(server: &str) -> String {
    url::Url::parse(server)
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
        .collect()
}

/// A new account's id on `server`: its host, then 128 random bits — no
/// two servers' storage can be made to meet.
pub fn new_account_id(server: &str) -> String {
    format!("{}-{}", host_part(server), notekpr_core::random_id())
}

/// The id of an account from before accounts: the name its window storage
/// had (its host and 48 bits of its address's hash), so it keeps it.
pub fn legacy_account_id(server: &str) -> String {
    use sha2::Digest;
    let hash = data_encoding::HEXLOWER.encode(&sha2::Sha256::digest(server.as_bytes()));
    format!("{}-{}", host_part(server), &hash[..12])
}

/// Whether [`legacy_account_id`] leaves `server` no other address but the
/// same host's on another port: its host in full (nothing replaced or cut)
/// and no path — 48 bits of hash aren't enough to tell addresses apart on
/// their own. Storage found under such a name is taken for an account
/// added later only when this holds; the account open at the upgrade has
/// its whole address ([`crate::settings::Settings::settle`]).
pub fn legacy_name_is_its_own(server: &str) -> bool {
    url::Url::parse(server)
        .is_ok_and(|u| u.path() == "/" && u.host_str().is_some_and(|h| h == host_part(server)))
}

#[derive(Clone, Debug)]
pub struct Paths {
    /// This profile's settings folder.
    pub config: PathBuf,
    /// This profile's data folder.
    pub data: PathBuf,
    /// The app profile's name.
    pub profile: String,
    data_root: PathBuf,
}

impl Paths {
    /// The roots are the CLI library's ([`notekpr_cli::profile::desktop_roots`]):
    /// the CLI looks there for the Markdown folder the app keeps.
    pub fn for_profile(profile: &str) -> Result<Paths> {
        let (config, data) = notekpr_cli::profile::desktop_roots()?;
        Ok(Paths {
            config: config.join(profile),
            data: data.join(profile),
            profile: profile.to_string(),
            data_root: data,
        })
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.json")
    }

    /// An account's window storage: one per account, so nothing one
    /// server's session left (its token, device key, copies) is ever sent
    /// to another. None: before an account was chosen.
    pub fn webview(&self, account: Option<&str>) -> PathBuf {
        self.data.join("webview").join(account.unwrap_or("none"))
    }

    /// An account's Markdown folder: its CLI profile lives under this root.
    pub fn folder(&self, account: &str) -> Dirs {
        notekpr_cli::profile::desktop_folder_root(&self.data_root, &self.profile, account)
    }

    /// The Markdown folder of before accounts (0.17), until it is moved to
    /// its account's ([`crate::folder::take_legacy`]).
    pub fn legacy_folder(&self) -> Dirs {
        notekpr_cli::profile::desktop_legacy_folder_root(&self.data_root, &self.profile)
    }

    /// Notes only on this computer: the database and attachments of the
    /// server inside the app.
    pub fn this_computer(&self) -> PathBuf {
        self.data.join("this-computer")
    }

    /// Make this profile's data folder readable by this account only
    /// (`0700`, as the CLI's), before anything is kept in it: it holds the
    /// window's storage (its session, this device's key) and the Markdown
    /// folder's keys. One made by an earlier version is tightened; a link
    /// someone put there is left as it is.
    pub fn private_data(&self) -> Result<()> {
        use anyhow::Context;
        notekpr_cli::fsutil::create_private_dir(&self.data)
            .with_context(|| format!("creating {}", self.data.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::symlink_metadata(&self.data)?;
            if meta.is_dir() && meta.permissions().mode() & 0o077 != 0 {
                std::fs::set_permissions(&self.data, std::fs::Permissions::from_mode(0o700))
                    .with_context(|| format!("making {} private", self.data.display()))?;
            }
        }
        Ok(())
    }
}

/// Profile names: what the CLI allows for its own (`--profile`).
pub fn valid_profile(name: &str) -> bool {
    notekpr_cli::profile::valid_profile_name(name)
}

/// The window storage at `dir` ([`Paths::webview`]) as macOS keeps it
/// (macOS 14 and newer): a data store of the app's, named by a UUID, as
/// WebKit there keeps one store unless told which — every account and
/// profile would share it. The same folder always gives the same store (a
/// UUID version 8 from its path's SHA-256), another folder another.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn webview_store(dir: &std::path::Path) -> [u8; 16] {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(dir.as_os_str().as_encoded_bytes());
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest[..16]);
    id[6] = (id[6] & 0x0f) | 0x80;
    id[8] = (id[8] & 0x3f) | 0x80;
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_store_per_window_storage() {
        let p = Paths {
            config: "/c".into(),
            data: "/d/default".into(),
            profile: "default".into(),
            data_root: "/d".into(),
        };
        let a = webview_store(&p.webview(Some("notes.example.com-9914ea42e685")));
        // The same every time, so the account finds its storage again.
        assert_eq!(
            a,
            webview_store(&p.webview(Some("notes.example.com-9914ea42e685")))
        );
        // Another account's, another profile's: another store.
        assert_ne!(a, webview_store(&p.webview(Some("other.example.com-1"))));
        assert_ne!(a, webview_store(&p.webview(None)));
        let bob = Paths {
            data: "/d/bob".into(),
            profile: "bob".into(),
            ..p.clone()
        };
        assert_ne!(
            a,
            webview_store(&bob.webview(Some("notes.example.com-9914ea42e685")))
        );
        // A UUID version 8 (RFC 9562), never all zeros.
        assert_eq!(a[6] >> 4, 8);
        assert_eq!(a[8] >> 6, 0b10);
    }

    #[test]
    fn a_window_storage_and_a_folder_per_account() {
        let p = Paths {
            config: "/c".into(),
            data: "/d".into(),
            profile: "default".into(),
            data_root: "/".into(),
        };
        // 0.17.0's names stay: an account from then keeps its storage.
        let a = legacy_account_id("https://notes.example.com");
        assert_eq!(a, "notes.example.com-9914ea42e685");
        assert_ne!(a, legacy_account_id("https://notes.example.com/other"));
        // Found again for another address only when the name pins it.
        for s in [
            "https://notes.example.com",
            "https://notes.example.com:8443",
            "http://127.0.0.1:8099",
            "http://localhost:8099",
            "https://my-notes.example.com",
        ] {
            assert!(legacy_name_is_its_own(s), "{s}");
        }
        for s in [
            "https://notes.example.com/other",
            "https://my_notes.example.com",
            "http://[::1]:8099",
            &format!("https://{}.example.com", "a".repeat(64)),
            "not an address",
        ] {
            assert!(!legacy_name_is_its_own(s), "{s}");
        }
        assert_eq!(
            p.webview(Some(&a)),
            PathBuf::from("/d/webview/notes.example.com-9914ea42e685")
        );
        assert_eq!(p.webview(None), PathBuf::from("/d/webview/none"));
        // New accounts: random, even on one server.
        let n = new_account_id("https://notes.example.com");
        assert!(n.starts_with("notes.example.com-"));
        assert_eq!(n.len(), "notes.example.com-".len() + 32);
        assert_ne!(n, new_account_id("https://notes.example.com"));
        for id in [&a, &n, crate::settings::THIS_COMPUTER] {
            assert!(valid_account_id(id), "{id}");
        }
        for id in ["", "none", ".", "..", "../x", "a/b", ".hidden"] {
            assert!(!valid_account_id(id), "{id}");
        }
        assert_eq!(
            p.folder("x").config,
            PathBuf::from("/default/folders/x/config")
        );
        assert_eq!(
            p.legacy_folder().config,
            PathBuf::from("/default/folder/config")
        );
        assert_eq!(p.this_computer(), PathBuf::from("/d/this-computer"));
    }

    #[test]
    fn a_copy_with_folders_of_its_own() {
        assert_eq!(own_folders_of(None, None), None);
        let (tag, args) = own_folders_of(Some("/r/c".into()), Some("/r/d".into())).unwrap();
        assert_eq!(tag.len(), 8);
        assert_eq!(args, ["--config-dir", "/r/c", "--data-dir", "/r/d"]);
        // Other folders, another copy; the same ones, the same copy.
        let (other, _) = own_folders_of(Some("/s/c".into()), Some("/r/d".into())).unwrap();
        assert_ne!(tag, other);
        assert_eq!(
            own_folders_of(Some("/r/c".into()), Some("/r/d".into()))
                .unwrap()
                .0,
            tag
        );
        // Only one given: the other is the system's.
        let (_, args) = own_folders_of(None, Some("/r/d".into())).unwrap();
        assert_eq!(args, ["--data-dir", "/r/d"]);
    }

    #[cfg(unix)]
    #[test]
    fn data_readable_by_this_account_only() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("notekpr-desktop");
        let paths = |profile: &str| Paths {
            config: tmp.path().join("config"),
            data: root.join(profile),
            profile: profile.into(),
            data_root: root.clone(),
        };

        // New: made private.
        let new = paths("default");
        new.private_data().unwrap();
        assert_eq!(mode(&new.data), 0o700);

        // Made by an earlier version, open to others: tightened, its
        // contents kept.
        let old = paths("work");
        std::fs::create_dir_all(old.data.join("webview")).unwrap();
        std::fs::write(old.data.join("webview/kept"), "x").unwrap();
        std::fs::set_permissions(&old.data, std::fs::Permissions::from_mode(0o755)).unwrap();
        old.private_data().unwrap();
        assert_eq!(mode(&old.data), 0o700);
        assert!(old.data.join("webview/kept").exists());

        // A link is left alone (and not followed).
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o755)).unwrap();
        let linked = paths("linked");
        std::os::unix::fs::symlink(&elsewhere, &linked.data).unwrap();
        linked.private_data().unwrap();
        assert_eq!(mode(&elsewhere), 0o755);
    }
}
