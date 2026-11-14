//! Profiles: one signed-in identity (person or agent) on one server.
//!
//! * `$XDG_CONFIG_HOME/notekpr/profiles/<name>.json` (mode 0600) holds the
//!   server URL, session token, identity, TOFU key pins and sync settings.
//! * `$XDG_DATA_HOME/notekpr/<name>/` holds the local key, the encrypted
//!   account/device secrets and the encrypted vault replicas.
//!
//! `NOTEKPR_CONFIG_DIR` / `NOTEKPR_DATA_DIR` override both roots.
//! Several profiles can be used side by side (`--profile alice`, `--profile bob`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use notekpr_core::protocol::PrincipalKind;
use serde::{Deserialize, Serialize};

use crate::fsutil;

pub const DEFAULT_PROFILE: &str = "default";

/// Root directories for configuration and data.
#[derive(Clone, Debug)]
pub struct Dirs {
    pub config: PathBuf,
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
        .ok_or_else(|| {
            anyhow!(
                "cannot determine the home directory; set NOTEKPR_CONFIG_DIR and NOTEKPR_DATA_DIR"
            )
        })
}

impl Dirs {
    /// Resolve from the environment (XDG base directories with overrides).
    pub fn from_env() -> Result<Dirs> {
        let config = match env_path("NOTEKPR_CONFIG_DIR") {
            Some(p) => p,
            None => match env_path("XDG_CONFIG_HOME").or_else(|| env_path("APPDATA")) {
                Some(p) => p.join("notekpr"),
                None => home()?.join(".config").join("notekpr"),
            },
        };
        let data = match env_path("NOTEKPR_DATA_DIR") {
            Some(p) => p,
            None => match env_path("XDG_DATA_HOME").or_else(|| env_path("LOCALAPPDATA")) {
                Some(p) => p.join("notekpr"),
                None => home()?.join(".local").join("share").join("notekpr"),
            },
        };
        Ok(Dirs { config, data })
    }

    pub fn under(root: &Path) -> Dirs {
        Dirs {
            config: root.join("config"),
            data: root.join("data"),
        }
    }

    pub fn profiles_dir(&self) -> PathBuf {
        self.config.join("profiles")
    }

    pub fn profile_path(&self, name: &str) -> PathBuf {
        self.profiles_dir().join(format!("{name}.json"))
    }

    pub fn profile_data(&self, name: &str) -> PathBuf {
        self.data.join(name)
    }

    /// Names of all existing profiles.
    pub fn profiles(&self) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(self.profiles_dir()) else {
            return vec![];
        };
        let mut v: Vec<String> = rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                e.file_name()
                    .to_str()
                    .and_then(|n| n.strip_suffix(".json"))
                    .map(str::to_string)
            })
            .filter(|n| valid_profile_name(n))
            .collect();
        v.sort();
        v
    }
}

/// Where the desktop app (`notekpr-desktop`, docs/desktop.md) keeps its
/// settings and its data: `(config, data)`, each holding a folder per app
/// profile. `NOTEKPR_DESKTOP_CONFIG_DIR` / `NOTEKPR_DESKTOP_DATA_DIR`, or
/// `notekpr-desktop` in the system's folders for them (XDG on Linux,
/// `%APPDATA%` / `%LOCALAPPDATA%` on Windows, `~/Library/Application
/// Support` on macOS). Kept here so the app and the CLI agree: the app keeps
/// its Markdown folder's CLI profile in `<data>/<app profile>/folder`
/// ([`desktop_folder_dirs`]), where the CLI looks for the folders it keeps.
pub fn desktop_roots() -> Result<(PathBuf, PathBuf)> {
    fn system_dir(xdg: &str, windows: &str, fallback: &[&str]) -> Result<PathBuf> {
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
    const APP_DIR: &str = "notekpr-desktop";
    let config = match env_path("NOTEKPR_DESKTOP_CONFIG_DIR") {
        Some(p) => p,
        None => system_dir("XDG_CONFIG_HOME", "APPDATA", &[".config"])?.join(APP_DIR),
    };
    let data = match env_path("NOTEKPR_DESKTOP_DATA_DIR") {
        Some(p) => p,
        None => system_dir("XDG_DATA_HOME", "LOCALAPPDATA", &[".local", "share"])?.join(APP_DIR),
    };
    Ok((config, data))
}

/// The desktop app's id as a Flatpak (and its identifier everywhere).
pub const DESKTOP_APP_ID: &str = "md.notekpr.Notekpr";

/// Whether this runs in a Flatpak's sandbox (the desktop app's Flatpak):
/// its folders are then its own (`~/.var/app/<id>`), not the system's usual ones.
pub fn inside_flatpak() -> bool {
    Path::new("/.flatpak-info").exists()
}

/// The folders the desktop app keeps its settings and data in when installed
/// the other way: from outside a Flatpak, the Flatpak's own (`~/.var/app/…`);
/// from inside one, the system's (`~/.config`, `~/.local/share`, or what
/// `XDG_CONFIG_HOME` and `XDG_DATA_HOME` say outside, which the sandbox
/// passes in as `HOST_XDG_…`). The CLI and the app look there too, so that
/// each sees the folders the other keeps, however the app was installed.
/// `env` looks the environment up.
pub fn other_install_homes(
    home: &Path,
    inside_flatpak: bool,
    env: &dyn Fn(&str) -> Option<PathBuf>,
) -> (PathBuf, PathBuf) {
    if inside_flatpak {
        // Relative ones are to be ignored (the XDG base directory spec).
        let host = |var: &str, usual: &[&str]| {
            env(var)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| usual.iter().fold(home.to_path_buf(), |p, d| p.join(d)))
        };
        (
            host("HOST_XDG_CONFIG_HOME", &[".config"]),
            host("HOST_XDG_DATA_HOME", &[".local", "share"]),
        )
    } else {
        let app = home.join(".var").join("app").join(DESKTOP_APP_ID);
        (app.join("config"), app.join("data"))
    }
}

/// [`other_install_homes`] here, on Linux.
fn other_install() -> Option<(PathBuf, PathBuf)> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    other_install_from(&env_path, inside_flatpak())
}

/// [`other_install`], with the environment looked up through `env`. None
/// where the desktop app's folders were chosen
/// (`NOTEKPR_DESKTOP_CONFIG_DIR`, `NOTEKPR_DESKTOP_DATA_DIR`: tests, several
/// people side by side): the app here isn't an installed one then. The CLI's
/// own (`NOTEKPR_CONFIG_DIR`, `NOTEKPR_DATA_DIR`) move only its folders: the
/// app's installs are where they are, and looked for.
fn other_install_from(
    env: &dyn Fn(&str) -> Option<PathBuf>,
    inside_flatpak: bool,
) -> Option<(PathBuf, PathBuf)> {
    let app_chosen = ["NOTEKPR_DESKTOP_CONFIG_DIR", "NOTEKPR_DESKTOP_DATA_DIR"];
    if app_chosen.iter().any(|v| env(v).is_some()) {
        return None;
    }
    Some(other_install_homes(&env("HOME")?, inside_flatpak, env))
}

/// Every place the desktop app may keep its settings and data on this
/// computer: [`desktop_roots`], and the other install's.
pub fn desktop_roots_all() -> Vec<(PathBuf, PathBuf)> {
    let mut v: Vec<_> = desktop_roots().into_iter().collect();
    if let Some((config, data)) = other_install() {
        v.push((config.join("notekpr-desktop"), data.join("notekpr-desktop")));
    }
    v
}

/// The CLI's folders as the system has them, seen from inside the desktop
/// app's Flatpak (whose own `~/.config` is elsewhere): None outside one.
pub fn cli_dirs_outside_sandbox() -> Option<Dirs> {
    if !inside_flatpak() {
        return None;
    }
    let (config, data) = other_install()?;
    Some(Dirs {
        config: config.join("notekpr"),
        data: data.join("notekpr"),
    })
}

/// The desktop app's own CLI profile root for an app profile: where its
/// Markdown folder's sync keeps its session, keys and local copies.
pub fn desktop_folder_root(data: &Path, app_profile: &str) -> Dirs {
    Dirs::under(&data.join(app_profile).join("folder"))
}

/// The desktop app's Markdown folders' profile roots on this computer, with
/// the app profile each belongs to — however the app was installed
/// ([`desktop_roots_all`]).
pub fn desktop_folder_dirs() -> Vec<(String, Dirs)> {
    let mut v: Vec<(String, Dirs)> = vec![];
    for (_, data) in desktop_roots_all() {
        let Ok(rd) = std::fs::read_dir(&data) else {
            continue;
        };
        v.extend(
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .filter(|n| valid_profile_name(n))
                .map(|n| {
                    let d = desktop_folder_root(&data, &n);
                    (n, d)
                })
                .filter(|(_, d)| d.profiles_dir().exists()),
        );
    }
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

pub fn valid_profile_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AgentCredentials {
    pub client_id: String,
    /// The client secret is never copied into the profile; it is read from
    /// this file whenever a new access token is needed.
    pub client_secret_file: PathBuf,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SyncSettings {
    /// Older profiles: the user accepted decrypted files for the remembered
    /// `mirror_dir` (it used to apply to any directory). Migrated to
    /// `plaintext_dirs` on the next confirmation.
    #[serde(default)]
    pub plaintext_ok: bool,
    /// Directories the user explicitly accepted decrypted files in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plaintext_dirs: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror_dir: Option<PathBuf>,
}

impl SyncSettings {
    /// Whether writing decrypted files into `dir` was confirmed.
    pub fn plaintext_confirmed(&self, dir: &Path) -> bool {
        self.plaintext_dirs.iter().any(|d| d == dir)
            || (self.plaintext_ok && self.mirror_dir.as_deref() == Some(dir))
    }

    pub fn confirm_plaintext(&mut self, dir: &Path) {
        let mut dirs = vec![dir.to_path_buf()];
        if self.plaintext_ok {
            dirs.extend(self.mirror_dir.clone());
            self.plaintext_ok = false;
        }
        for d in dirs {
            if !self.plaintext_dirs.contains(&d) {
                self.plaintext_dirs.push(d);
            }
        }
    }
}

/// Size limits the server enforces, remembered so they are known offline.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerLimits {
    pub max_attachment_bytes: u64,
    pub max_update_bytes: u64,
    /// 0: not known (older servers; the sync crate applies their rule).
    #[serde(default)]
    pub max_snapshot_bytes: u64,
}

impl Default for ServerLimits {
    /// The server's defaults (`max_attachment_mb` 25, `max_update_kb` 2048).
    fn default() -> Self {
        ServerLimits {
            max_attachment_bytes: 25 * 1024 * 1024,
            max_update_bytes: 2048 * 1024,
            max_snapshot_bytes: 0,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct GatewaySettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Profile {
    /// Server origin, e.g. `https://notes.example.com` (no `/api/v1`).
    pub server: String,
    #[serde(default)]
    pub insecure_http: bool,
    pub kind: PrincipalKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    /// Device registered but not yet approved (login can be resumed).
    #[serde(default)]
    pub device_pending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentCredentials>,
    #[serde(default)]
    pub sync: SyncSettings,
    #[serde(default)]
    pub gateway: GatewaySettings,
    /// Last limits the server announced (`/info`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_limits: Option<ServerLimits>,
    /// Trust-on-first-use account key fingerprints: user id -> fingerprint.
    #[serde(default)]
    pub pins: BTreeMap<String, String>,
    /// For vaults whose notes are shared with us: who started the vault's
    /// membership log, as first shown (vault id -> user id). Keys of those
    /// notes are taken only from owners the server shows we can trace back
    /// to them (`notekpr_core::memberlog::check_owner_proof`).
    #[serde(default)]
    pub share_roots: BTreeMap<String, String>,
    /// What the person's synced settings held, as far as this app has seen
    /// them: whom their apps trust and the newest counter. The server can't
    /// make up a copy, but it can hand out an older one: this app never
    /// saves one again without these (`crate::prefs`).
    #[serde(default)]
    pub synced_trust: SyncedTrust,
}

/// See [`Profile::synced_trust`]: the synced settings' fields of that name.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct SyncedTrust {
    #[serde(default)]
    pub counter: u64,
    #[serde(default)]
    pub pins: BTreeMap<String, String>,
    #[serde(default)]
    pub share_roots: BTreeMap<String, String>,
    #[serde(default)]
    pub log_heads: notekpr_sync::members::LogHeads,
}

impl Profile {
    pub fn new(server: &str, kind: PrincipalKind, insecure_http: bool) -> Self {
        Profile {
            server: server.to_string(),
            insecure_http,
            kind,
            user_id: None,
            display_name: None,
            device_id: None,
            device_name: None,
            device_pending: false,
            session_token: None,
            session_expires_at: None,
            agent: None,
            sync: SyncSettings::default(),
            gateway: GatewaySettings::default(),
            server_limits: None,
            pins: BTreeMap::new(),
            share_roots: BTreeMap::new(),
            synced_trust: SyncedTrust::default(),
        }
    }
}

/// Access to one profile's files.
#[derive(Clone, Debug)]
pub struct ProfileStore {
    pub dirs: Dirs,
    pub name: String,
}

impl ProfileStore {
    pub fn new(dirs: &Dirs, name: &str) -> Result<Self> {
        if !valid_profile_name(name) {
            bail!("invalid profile name '{name}' (use letters, digits, '-' and '_')");
        }
        Ok(ProfileStore {
            dirs: dirs.clone(),
            name: name.to_string(),
        })
    }

    pub fn path(&self) -> PathBuf {
        self.dirs.profile_path(&self.name)
    }

    pub fn data_dir(&self) -> PathBuf {
        self.dirs.profile_data(&self.name)
    }

    pub fn load(&self) -> Result<Option<Profile>> {
        let Some(bytes) = fsutil::read_optional(&self.path())? else {
            return Ok(None);
        };
        let p = serde_json::from_slice(&bytes)
            .with_context(|| format!("reading profile '{}'", self.name))?;
        Ok(Some(p))
    }

    pub fn require(&self) -> Result<Profile> {
        self.load()?.ok_or_else(|| {
            anyhow!(
                "profile '{}' is not set up; run `notekpr --profile {} login --server URL` \
                 (or `agent setup` for an agent)",
                self.name,
                self.name
            )
        })
    }

    pub fn save(&self, p: &Profile) -> Result<()> {
        fsutil::create_private_dir(&self.dirs.profiles_dir())?;
        let json = serde_json::to_vec_pretty(p)?;
        fsutil::write_atomic(&self.path(), &json, true)
            .with_context(|| format!("saving profile '{}'", self.name))
    }

    /// Held (briefly) around every read-modify-write of the profile file, so
    /// concurrent processes (a running sync daemon pinning a key, a login
    /// renewing the session) never drop each other's changes.
    fn edit_lock(&self) -> Result<std::fs::File> {
        fsutil::create_private_dir(&self.dirs.profiles_dir())?;
        let f = lock_file(
            &self
                .dirs
                .profiles_dir()
                .join(format!(".{}.json.lock", self.name)),
        )?;
        f.lock().context("locking the profile file")?;
        Ok(f)
    }

    /// Read-modify-write under the profile's edit lock, re-reading the file
    /// first.
    pub fn update<R>(&self, f: impl FnOnce(&mut Profile) -> R) -> Result<R> {
        let _lock = self.edit_lock()?;
        let mut p = self.require()?;
        let r = f(&mut p);
        self.save(&p)?;
        Ok(r)
    }

    /// [`ProfileStore::update`], creating the profile with `new` if needed.
    pub fn upsert<R>(
        &self,
        new: impl FnOnce() -> Profile,
        f: impl FnOnce(&mut Profile) -> R,
    ) -> Result<R> {
        let _lock = self.edit_lock()?;
        let mut p = self.load()?.unwrap_or_else(new);
        let r = f(&mut p);
        self.save(&p)?;
        Ok(r)
    }

    pub fn delete(&self) -> Result<()> {
        match std::fs::remove_file(self.path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

fn lock_file(path: &Path) -> Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
        .with_context(|| format!("opening {}", path.display()))
}

/// Exclusive use of a profile's local data (replicas, mirror bookkeeping)
/// by one process. Two processes each holding a replica in memory would
/// overwrite each other's files — a queued edit or a received update could
/// be lost. The lock is released when the value is dropped or the process
/// ends (also on a crash).
pub struct ProfileLock {
    _file: std::fs::File,
}

impl ProfileLock {
    fn path(dirs: &Dirs, profile: &str) -> PathBuf {
        dirs.profiles_dir().join(format!(".{profile}.lock"))
    }

    /// Take the lock if no other process holds it.
    pub fn try_acquire(dirs: &Dirs, profile: &str) -> Result<Option<ProfileLock>> {
        if !valid_profile_name(profile) {
            bail!("invalid profile name '{profile}'");
        }
        fsutil::create_private_dir(&dirs.profiles_dir())?;
        let f = lock_file(&Self::path(dirs, profile))?;
        match f.try_lock() {
            Ok(()) => Ok(Some(ProfileLock { _file: f })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => {
                Err(e).context("locking the profile's local data")
            }
        }
    }

    /// Take the lock, waiting up to `wait` for another process to release it.
    pub async fn acquire_within(
        dirs: &Dirs,
        profile: &str,
        wait: std::time::Duration,
    ) -> Result<Option<ProfileLock>> {
        let deadline = std::time::Instant::now() + wait;
        loop {
            if let Some(l) = Self::try_acquire(dirs, profile)? {
                return Ok(Some(l));
            }
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
}

/// Validate and normalise a server URL to its origin (plus an optional path
/// prefix). Plain `http://` is only accepted for loopback hosts unless
/// `insecure_http` is set.
pub fn normalize_server_url(input: &str, insecure_http: bool) -> Result<String> {
    let input = input.trim();
    let with_scheme = if input.contains("://") {
        input.to_string()
    } else {
        format!("https://{input}")
    };
    let url =
        url::Url::parse(&with_scheme).with_context(|| format!("invalid server URL '{input}'"))?;
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("server URL '{input}' has no host"))?;
    match url.scheme() {
        "https" => {}
        "http" => {
            if !is_loopback_host(host) && !insecure_http {
                bail!(
                    "refusing to use plain http:// with {host}: credentials and encrypted data would travel \
                     without TLS. Use https://, or pass --insecure-http if you really mean it"
                );
            }
        }
        s => bail!("unsupported URL scheme '{s}' (use https://)"),
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("the server URL must not contain a query or fragment");
    }
    let mut out = format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default());
    if host.contains(':') && !out.ends_with(']') {
        // IPv6 literal: host_str() omits brackets.
        out = format!("{}://[{}]", url.scheme(), host);
    }
    if let Some(port) = url.port() {
        out.push_str(&format!(":{port}"));
    }
    let path = url.path().trim_end_matches('/');
    let path = path.strip_suffix("/api/v1").unwrap_or(path);
    out.push_str(path);
    Ok(out)
}

pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost")
        || h.parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_urls() {
        assert_eq!(
            normalize_server_url("notes.example.com", false).unwrap(),
            "https://notes.example.com"
        );
        assert_eq!(
            normalize_server_url("https://n.example.com/api/v1/", false).unwrap(),
            "https://n.example.com"
        );
        assert_eq!(
            normalize_server_url("https://n.example.com/sub/", false).unwrap(),
            "https://n.example.com/sub"
        );
        assert_eq!(
            normalize_server_url("http://localhost:8080", false).unwrap(),
            "http://localhost:8080"
        );
        assert_eq!(
            normalize_server_url("http://127.0.0.1:1234", false).unwrap(),
            "http://127.0.0.1:1234"
        );
        assert_eq!(
            normalize_server_url("http://[::1]:9", false).unwrap(),
            "http://[::1]:9"
        );
        assert!(normalize_server_url("http://notes.example.com", false).is_err());
        assert!(normalize_server_url("http://notes.example.com", true).is_ok());
        assert!(normalize_server_url("ftp://x", true).is_err());
    }

    #[test]
    fn profile_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(d.path());
        let ps = ProfileStore::new(&dirs, "alice").unwrap();
        assert!(ps.load().unwrap().is_none());
        ps.save(&Profile::new("https://x", PrincipalKind::Human, false))
            .unwrap();
        ps.update(|p| p.pins.insert("u".into(), "F".into()))
            .unwrap();
        assert_eq!(ps.require().unwrap().pins["u"], "F");
        assert_eq!(dirs.profiles(), vec!["alice".to_string()]);
        assert!(!crate::fsutil::is_shared_readable(&ps.path()));
        assert!(ProfileStore::new(&dirs, "../x").is_err());
    }

    #[test]
    fn one_process_at_a_time_owns_a_profile() {
        let d = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(d.path());
        let first = ProfileLock::try_acquire(&dirs, "alice").unwrap();
        assert!(first.is_some());
        // (flock conflicts between separate opens, also within one process.)
        assert!(ProfileLock::try_acquire(&dirs, "alice").unwrap().is_none());
        assert!(ProfileLock::try_acquire(&dirs, "bob").unwrap().is_some());
        drop(first);
        assert!(ProfileLock::try_acquire(&dirs, "alice").unwrap().is_some());
    }

    #[test]
    fn concurrent_profile_edits_are_not_lost() {
        let d = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(d.path());
        let ps = ProfileStore::new(&dirs, "alice").unwrap();
        ps.save(&Profile::new("https://x", PrincipalKind::Human, false))
            .unwrap();
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let ps = ps.clone();
                std::thread::spawn(move || {
                    for i in 0..20 {
                        ps.update(|p| p.pins.insert(format!("{t}-{i}"), "F".into()))
                            .unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(ps.require().unwrap().pins.len(), 160);
    }

    #[test]
    fn plaintext_confirmation_is_per_directory() {
        let a = Path::new("/home/u/Notes");
        let b = Path::new("/mnt/usb/Notes");
        // A profile from before: the flag covers its remembered directory only.
        let mut s = SyncSettings {
            plaintext_ok: true,
            plaintext_dirs: vec![],
            mirror_dir: Some(a.into()),
        };
        assert!(s.plaintext_confirmed(a));
        assert!(!s.plaintext_confirmed(b));
        s.confirm_plaintext(b);
        s.mirror_dir = Some(b.into());
        assert!(!s.plaintext_ok);
        assert!(s.plaintext_confirmed(a) && s.plaintext_confirmed(b));
        s.confirm_plaintext(b);
        assert_eq!(s.plaintext_dirs.len(), 2);
    }

    #[test]
    fn the_app_installed_the_other_way_is_looked_for_where_it_keeps_things() {
        fn vars<'a>(v: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<PathBuf> + 'a {
            move |name| {
                v.iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, p)| PathBuf::from(p))
            }
        }
        let pair = |c: &str, d: &str| Some((PathBuf::from(c), PathBuf::from(d)));
        let home = [("HOME", "/home/ana")];
        // From outside a Flatpak: the Flatpak's own folders.
        assert_eq!(
            other_install_from(&vars(&home), false),
            pair(
                "/home/ana/.var/app/md.notekpr.Notekpr/config",
                "/home/ana/.var/app/md.notekpr.Notekpr/data"
            )
        );
        // From inside it: the system's usual ones…
        assert_eq!(
            other_install_from(&vars(&home), true),
            pair("/home/ana/.config", "/home/ana/.local/share")
        );
        // …or those XDG_CONFIG_HOME and XDG_DATA_HOME say outside (relative
        // ones don't count).
        let moved = [
            ("HOME", "/home/ana"),
            ("HOST_XDG_CONFIG_HOME", "/home/ana/settings"),
            ("HOST_XDG_DATA_HOME", "data"),
        ];
        assert_eq!(
            other_install_from(&vars(&moved), true),
            pair("/home/ana/settings", "/home/ana/.local/share")
        );
        // The CLI's folders chosen (even where they usually are): the app's
        // installs are still looked for.
        let cli = [
            ("HOME", "/home/ana"),
            ("NOTEKPR_DATA_DIR", "/home/ana/.local/share/notekpr"),
        ];
        assert_eq!(
            other_install_from(&vars(&cli), false),
            other_install_from(&vars(&home), false)
        );
        // The app's chosen (tests, several people side by side): only those.
        let app = [
            ("HOME", "/home/ana"),
            ("NOTEKPR_DESKTOP_DATA_DIR", "/tmp/t"),
        ];
        assert_eq!(other_install_from(&vars(&app), false), None);
    }
}
