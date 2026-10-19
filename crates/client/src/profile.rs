//! Profiles: one signed-in identity (person or agent) on one server.
//!
//! * `$XDG_CONFIG_HOME/coolpsonotes/profiles/<name>.json` (mode 0600) holds the
//!   server URL, session token, identity, TOFU key pins and sync settings.
//! * `$XDG_DATA_HOME/coolpsonotes/<name>/` holds the local key, the encrypted
//!   account/device secrets and the encrypted vault replicas.
//!
//! `COOLPSONOTES_CONFIG_DIR` / `COOLPSONOTES_DATA_DIR` override both roots.
//! Several profiles can be used side by side (`--profile alice`, `--profile bob`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use coolpsonotes_core::protocol::PrincipalKind;
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
    env_path("HOME").or_else(|| env_path("USERPROFILE")).ok_or_else(|| {
        anyhow!("cannot determine the home directory; set COOLPSONOTES_CONFIG_DIR and COOLPSONOTES_DATA_DIR")
    })
}

impl Dirs {
    /// Resolve from the environment (XDG base directories with overrides).
    pub fn from_env() -> Result<Dirs> {
        let config = match env_path("COOLPSONOTES_CONFIG_DIR") {
            Some(p) => p,
            None => match env_path("XDG_CONFIG_HOME").or_else(|| env_path("APPDATA")) {
                Some(p) => p.join("coolpsonotes"),
                None => home()?.join(".config").join("coolpsonotes"),
            },
        };
        let data = match env_path("COOLPSONOTES_DATA_DIR") {
            Some(p) => p,
            None => match env_path("XDG_DATA_HOME").or_else(|| env_path("LOCALAPPDATA")) {
                Some(p) => p.join("coolpsonotes"),
                None => home()?.join(".local").join("share").join("coolpsonotes"),
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
    /// The user explicitly accepted that `sync --dir` writes decrypted files.
    #[serde(default)]
    pub plaintext_ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror_dir: Option<PathBuf>,
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
    /// Trust-on-first-use account key fingerprints: user id -> fingerprint.
    #[serde(default)]
    pub pins: BTreeMap<String, String>,
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
            pins: BTreeMap::new(),
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
                "profile '{}' is not set up; run `coolpsonotes --profile {} login --server URL` \
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

    /// Read-modify-write, re-reading the file first so concurrent processes
    /// (a running sync daemon and a one-off command) do not drop each other's changes.
    pub fn update<R>(&self, f: impl FnOnce(&mut Profile) -> R) -> Result<R> {
        let mut p = self.require()?;
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
}
