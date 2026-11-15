//! Where the app keeps things, per profile (`--profile`, `default` unless
//! given):
//!
//! * `<config>/notekpr-desktop/<profile>/settings.json`: the server and the
//!   app's settings ([`crate::settings`]). Nothing secret.
//! * `<data>/notekpr-desktop/<profile>/webview/`: the window's storage, as a
//!   browser profile holds the web app's (the device key, IndexedDB copies).
//! * `<data>/notekpr-desktop/<profile>/folder/`: the Markdown folder's CLI
//!   profile ([`crate::folder`]): its session, sealed keys, local copies.
//! * `<data>/notekpr-desktop/<profile>/this-computer/`: notes only on this
//!   computer, the server's data ([`crate::local`]); its window's storage
//!   is `webview/this-computer`.
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

    /// The window's storage for notes only on this computer (no server's
    /// name has this form: theirs end in a hash).
    pub fn webview_here(&self) -> PathBuf {
        self.data.join("webview").join("this-computer")
    }

    /// Notes only on this computer: the database and attachments of the
    /// server inside the app.
    pub fn this_computer(&self) -> PathBuf {
        self.data.join("this-computer")
    }

    /// The Markdown folder's CLI profile lives under this root.
    pub fn folder(&self) -> Dirs {
        notekpr_cli::profile::desktop_folder_root(&self.data_root, &self.profile)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_storage_per_server() {
        let p = Paths {
            config: "/c".into(),
            data: "/d".into(),
            profile: "default".into(),
            data_root: "/".into(),
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
        // Notes only on this computer: a window's storage of their own,
        // whatever server the app was on before.
        let here = p.webview_here();
        assert_eq!(here, PathBuf::from("/d/webview/this-computer"));
        for s in [
            "http://this-computer",
            "http://127.0.0.1:8080",
            "https://this-computer/",
        ] {
            assert_ne!(p.webview(Some(s)), here);
        }
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
