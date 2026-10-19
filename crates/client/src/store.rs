//! Encrypted on-disk replica of one vault.
//!
//! ```text
//! <data>/<profile>/vaults/<vault-id>/
//!   state.sealed        keys, metadata, seq, max_head_seen, counters, flags
//!   pending.sealed      unacknowledged local updates (retried with the same id)
//!   quarantine.sealed   local updates too large for the server (kept, not sent)
//!   objects/<id>.sealed full CRDT state of the index and of every note
//!   blobs/<id>          attachment ciphertext exactly as stored on the server
//!   mirror.sealed       file-mirror bookkeeping (see `mirror.rs`)
//! ```
//!
//! Everything except blobs (already encrypted with the vault key) is sealed
//! with the profile's local key. Files are replaced atomically. Saves write
//! `pending` first, then objects, then `state`; on load, pending updates are
//! re-applied to the documents. A crash at any point therefore loses nothing:
//! a pending edit is either in the journal or not yet acknowledged to the user,
//! and a `seq` that lags behind the objects only causes an idempotent re-fetch.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use coolpsonotes_core::b64;
use coolpsonotes_core::engine::{Pending, VaultMeta};
use coolpsonotes_core::protocol::Role;
use serde::{Deserialize, Serialize};

use crate::fsutil;
use crate::secrets::LocalKey;

const FORMAT: u32 = 1;

/// Ids we use as file names: the same alphabet the server accepts.
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StoredState {
    pub format: u32,
    pub vault_id: String,
    pub role: Role,
    pub owner_id: String,
    #[serde(default)]
    pub archived: bool,
    pub meta: VaultMeta,
    /// Vault key per epoch (base64). Needed to read the replica offline.
    pub keys: BTreeMap<u32, String>,
    /// Document keys of individually shared notes: object id → epoch → key (base64).
    #[serde(default)]
    pub doc_keys: BTreeMap<String, BTreeMap<u32, String>>,
    /// Received content waiting for a key (already encrypted).
    #[serde(default)]
    pub deferred: Vec<coolpsonotes_core::engine::Deferred>,
    pub seq: i64,
    pub max_head_seen: i64,
    #[serde(default)]
    pub since_snapshot: BTreeMap<String, u32>,
    #[serde(default)]
    pub purged: BTreeSet<String>,
    /// Attachments encrypted locally but not uploaded yet.
    #[serde(default)]
    pub pending_blobs: BTreeSet<String>,
    /// Set when access was revoked or the vault deleted: the local copy stays
    /// readable but is never written to or pushed again.
    #[serde(default)]
    pub read_only: Option<String>,
    #[serde(default)]
    pub last_sync: Option<i64>,
}

#[derive(Serialize, Deserialize, Default)]
struct StoredPending {
    updates: Vec<StoredPendingUpdate>,
}

#[derive(Serialize, Deserialize)]
struct StoredPendingUpdate {
    client_update_id: String,
    object_id: String,
    #[serde(with = "b64")]
    update: Vec<u8>,
}

#[derive(Clone)]
pub struct VaultStore {
    dir: PathBuf,
    key: Arc<LocalKey>,
    vault_id: String,
}

impl VaultStore {
    pub fn new(vaults_dir: PathBuf, key: Arc<LocalKey>, vault_id: &str) -> Result<VaultStore> {
        if !valid_id(vault_id) {
            return Err(anyhow!("invalid vault id"));
        }
        Ok(VaultStore {
            dir: vaults_dir.join(vault_id),
            key,
            vault_id: vault_id.to_string(),
        })
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    pub fn exists(&self) -> bool {
        self.dir.join("state.sealed").exists()
    }

    fn ctx(&self, what: &str) -> String {
        format!("vault/{}/{what}", self.vault_id)
    }

    pub fn load_state(&self) -> Result<Option<StoredState>> {
        let Some(pt) = self
            .key
            .read_sealed(&self.dir.join("state.sealed"), &self.ctx("state"))?
        else {
            return Ok(None);
        };
        let s: StoredState = serde_json::from_slice(&pt).context("decoding local vault state")?;
        if s.vault_id != self.vault_id {
            return Err(anyhow!("local vault state belongs to a different vault"));
        }
        if s.format > FORMAT {
            return Err(anyhow!("local data was written by a newer client version"));
        }
        Ok(Some(s))
    }

    pub fn save_state(&self, s: &StoredState) -> Result<()> {
        fsutil::create_private_dir(&self.dir)?;
        let mut s = s.clone();
        s.format = FORMAT;
        self.key.write_sealed(
            &self.dir.join("state.sealed"),
            &self.ctx("state"),
            &serde_json::to_vec(&s)?,
        )
    }

    pub fn load_pending(&self) -> Result<Vec<Pending>> {
        self.load_updates("pending")
    }

    pub fn save_pending(&self, pending: &[Pending]) -> Result<()> {
        self.save_updates("pending", pending)
    }

    /// Local updates the server refused as too large: kept, not uploaded.
    pub fn load_quarantine(&self) -> Result<Vec<Pending>> {
        self.load_updates("quarantine")
    }

    pub fn save_quarantine(&self, updates: &[Pending]) -> Result<()> {
        self.save_updates("quarantine", updates)
    }

    fn load_updates(&self, name: &str) -> Result<Vec<Pending>> {
        let Some(pt) = self
            .key
            .read_sealed(&self.dir.join(format!("{name}.sealed")), &self.ctx(name))?
        else {
            return Ok(vec![]);
        };
        let p: StoredPending = serde_json::from_slice(&pt).context("decoding queued updates")?;
        Ok(p.updates
            .into_iter()
            .map(|u| Pending {
                client_update_id: u.client_update_id,
                object_id: u.object_id,
                update: u.update,
            })
            .collect())
    }

    fn save_updates(&self, name: &str, updates: &[Pending]) -> Result<()> {
        fsutil::create_private_dir(&self.dir)?;
        let p = StoredPending {
            updates: updates
                .iter()
                .map(|p| StoredPendingUpdate {
                    client_update_id: p.client_update_id.clone(),
                    object_id: p.object_id.clone(),
                    update: p.update.clone(),
                })
                .collect(),
        };
        self.key.write_sealed(
            &self.dir.join(format!("{name}.sealed")),
            &self.ctx(name),
            &serde_json::to_vec(&p)?,
        )
    }

    fn object_path(&self, object_id: &str) -> Result<PathBuf> {
        if !valid_id(object_id) {
            return Err(anyhow!("invalid object id"));
        }
        Ok(self.dir.join("objects").join(format!("{object_id}.sealed")))
    }

    pub fn save_object(&self, object_id: &str, state: &[u8]) -> Result<()> {
        let p = self.object_path(object_id)?;
        fsutil::create_private_dir(&self.dir.join("objects"))?;
        self.key
            .write_sealed(&p, &self.ctx(&format!("object/{object_id}")), state)
    }

    pub fn delete_object(&self, object_id: &str) -> Result<()> {
        let p = self.object_path(object_id)?;
        match std::fs::remove_file(p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// All stored objects as (id, full state).
    pub fn load_objects(&self) -> Result<Vec<(String, Vec<u8>)>> {
        let dir = self.dir.join("objects");
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return Ok(vec![]);
        };
        let mut out = Vec::new();
        for e in rd {
            let e = e?;
            let name = e.file_name();
            let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".sealed")) else {
                continue;
            };
            if !valid_id(id) {
                continue;
            }
            let ct = std::fs::read(e.path())?;
            let pt = self.key.open(&self.ctx(&format!("object/{id}")), &ct)?;
            out.push((id.to_string(), pt.to_vec()));
        }
        Ok(out)
    }

    fn blob_path(&self, id: &str) -> Result<PathBuf> {
        if !valid_id(id) {
            return Err(anyhow!("invalid attachment id"));
        }
        Ok(self.dir.join("blobs").join(id))
    }

    /// Cache an attachment ciphertext (encrypted with the vault key).
    pub fn save_blob(&self, id: &str, ciphertext: &[u8]) -> Result<()> {
        let p = self.blob_path(id)?;
        fsutil::create_private_dir(&self.dir.join("blobs"))?;
        fsutil::write_atomic(&p, ciphertext, true)?;
        Ok(())
    }

    pub fn load_blob(&self, id: &str) -> Result<Option<Vec<u8>>> {
        Ok(fsutil::read_optional(&self.blob_path(id)?)?)
    }

    pub fn delete_blob(&self, id: &str) -> Result<()> {
        match std::fs::remove_file(self.blob_path(id)?) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// Auxiliary sealed documents (mirror bookkeeping).
    pub fn save_aux(&self, name: &str, data: &[u8]) -> Result<()> {
        fsutil::create_private_dir(&self.dir)?;
        self.key.write_sealed(
            &self.dir.join(format!("{name}.sealed")),
            &self.ctx(name),
            data,
        )
    }

    pub fn load_aux(&self, name: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .key
            .read_sealed(&self.dir.join(format!("{name}.sealed")), &self.ctx(name))?
            .map(|z| z.to_vec()))
    }

    pub fn wipe(&self) -> Result<()> {
        if self.dir.exists() {
            std::fs::remove_dir_all(&self.dir)?;
        }
        Ok(())
    }
}
