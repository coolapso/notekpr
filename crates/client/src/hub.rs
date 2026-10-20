//! The runtime shared by every front end: the set of open vaults of one
//! session, change notifications, and pushing local edits.
//!
//! One-shot CLI commands use a hub for a single catch-up + edit + push. The
//! sync daemon and gateway keep one alive, with
//! [`crate::livesync`] streaming remote changes into it over the WebSocket.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use anyhow::{Result, anyhow, bail};
use coolpsonotes_core::engine::Applied;
use coolpsonotes_core::protocol::{INDEX_OBJECT_ID, Role, VaultInfo};
use serde::Serialize;
use tokio::sync::{Mutex, broadcast};

use crate::api::ApiError;
use crate::profile::ServerLimits;
use crate::session::{PushOutcome, Session, SharedVault};
use crate::ui;
use crate::vault::Vault;

#[derive(Clone, Debug)]
pub enum HubEvent {
    /// Documents or the index changed (`local` = made on this machine).
    Changed {
        vault_id: String,
        objects: Vec<String>,
        index_changed: bool,
        purged: Vec<String>,
        local: bool,
    },
    /// A vault became available (opened) on this hub.
    VaultAdded { vault_id: String },
    /// Access was revoked or the vault was deleted; the local copy is read-only.
    VaultClosed { vault_id: String, reason: String },
    /// Live connection state changed.
    Connection { online: bool },
}

#[derive(Clone, Debug, Default)]
pub struct HubOptions {
    /// Restrict to these vaults (ids or unique names). Empty = all.
    pub vaults: Vec<String>,
    /// Refuse every write (gateway `--read-only`).
    pub read_only: bool,
    /// Fall back to the local replicas when the server is unreachable.
    pub offline_ok: bool,
    /// Catch up over REST and push pending changes while starting.
    pub catch_up: bool,
    /// Another process owns this profile's local data (a running sync
    /// daemon): only read the local copies — no network sync, no writes.
    pub detached: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct VaultSummary {
    pub id: String,
    pub name: String,
    /// Its icon (an emoji), if it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub role: Role,
    pub archived: bool,
    pub documents: usize,
    pub pending_changes: usize,
    /// Local changes too large for the server: kept here, not uploaded.
    #[serde(skip_serializing_if = "is_zero")]
    pub too_large_changes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only: Option<String>,
    pub seq: i64,
    pub last_sync: Option<i64>,
}

pub struct Hub {
    pub session: Arc<Session>,
    vaults: RwLock<BTreeMap<String, SharedVault>>,
    /// Allow-list as given by the user (ids or names).
    allow: Vec<String>,
    /// Ids admitted by the allow-list (None = no restriction).
    allowed: RwLock<Option<BTreeSet<String>>>,
    pub read_only: bool,
    events: broadcast::Sender<HubEvent>,
    push_locks: std::sync::Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
    online: AtomicBool,
    /// When true, local edits are pushed immediately (no live loop, or connected).
    push_now: AtomicBool,
    /// Notes edited on this machine and when, for "editing session ended"
    /// revisions (see [`Hub::snapshot_idle`]).
    edited: std::sync::Mutex<BTreeMap<(String, String), std::time::Instant>>,
    waiting_for_keys: RwLock<BTreeSet<String>>,
}

fn lower(s: &str) -> String {
    s.to_lowercase()
}

impl Hub {
    pub async fn start(session: Arc<Session>, opts: HubOptions) -> Result<Arc<Hub>> {
        let (events, _) = broadcast::channel(1024);
        let hub = Arc::new(Hub {
            session,
            vaults: RwLock::new(BTreeMap::new()),
            allow: opts.vaults.clone(),
            allowed: RwLock::new(None),
            read_only: opts.read_only || opts.detached,
            events,
            push_locks: Default::default(),
            online: AtomicBool::new(false),
            push_now: AtomicBool::new(true),
            waiting_for_keys: Default::default(),
            edited: Default::default(),
        });
        hub.load(&opts).await?;
        if opts.catch_up && hub.is_online() {
            for v in hub.vault_ids() {
                if let Err(e) = hub.sync_vault(&v).await {
                    ui::warn(&format!("syncing vault {v}: {e:#}"));
                }
            }
        }
        Ok(hub)
    }

    async fn load(&self, opts: &HubOptions) -> Result<()> {
        if opts.detached {
            for id in self.session.local_vault_ids() {
                match self.session.load_local(&id) {
                    Ok(Some(mut v)) => {
                        v.detach();
                        self.vaults
                            .write()
                            .unwrap()
                            .insert(id, Arc::new(Mutex::new(v)));
                    }
                    Ok(None) => {}
                    Err(e) => ui::warn(&format!("{e:#}")),
                }
            }
            return self.apply_allow_list();
        }
        match self.session.api.vaults().await {
            Ok(list) => {
                self.online.store(true, Ordering::Relaxed);
                // Learn (and remember) the size limits while online.
                let _ = self.limits().await;
                self.preresolve_allow_list(&list);
                self.open_listed(&list).await?;
            }
            Err(e)
                if (e.is_offline() || e.is_unauthorized() || matches!(e, ApiError::Auth(_)))
                    && opts.offline_ok =>
            {
                if e.is_network() {
                    ui::warn("the server is unreachable; working offline with the local copies");
                } else if e.is_offline() {
                    ui::warn(&format!(
                        "the server is not available right now ({e}); working offline with the local copies"
                    ));
                } else {
                    ui::warn(&format!(
                        "{e}; showing local copies only (run `coolpsonotes login`)"
                    ));
                }
                for id in self.session.local_vault_ids() {
                    match self.session.load_local(&id) {
                        Ok(Some(v)) => {
                            self.vaults
                                .write()
                                .unwrap()
                                .insert(id, Arc::new(Mutex::new(v)));
                        }
                        Ok(None) => {}
                        Err(e) => ui::warn(&format!("{e:#}")),
                    }
                }
            }
            Err(e) => return Err(anyhow!(e).context("listing vaults")),
        }
        self.apply_allow_list()
    }

    /// Open every listed vault not open yet; vaults that disappeared from the
    /// list become read-only local copies. Returns newly opened ids.
    pub async fn open_listed(&self, list: &[VaultInfo]) -> Result<Vec<String>> {
        let mut added = Vec::new();
        let listed: BTreeSet<&str> = list.iter().map(|i| i.id.as_str()).collect();
        for info in list {
            if !self.id_may_be_allowed(&info.id) {
                continue;
            }
            if let Some(existing) = self.get(&info.id) {
                let mut v = existing.lock().await;
                if let Err(e) = self.session.apply_info(&mut v, info).await {
                    tracing::warn!(vault = %info.id, "refreshing vault: {e:#}");
                }
                continue;
            }
            match self.session.open_vault(info).await {
                Ok(v) => {
                    self.waiting_for_keys.write().unwrap().remove(&info.id);
                    self.vaults
                        .write()
                        .unwrap()
                        .insert(info.id.clone(), Arc::new(Mutex::new(v)));
                    added.push(info.id.clone());
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    if msg.contains("no key has been shared") {
                        if self
                            .waiting_for_keys
                            .write()
                            .unwrap()
                            .insert(info.id.clone())
                        {
                            ui::status(&format!(
                                "vault {} was shared with you but its key has not arrived yet; it will open once the \
                                 owner's client grants it",
                                info.id
                            ));
                        }
                    } else {
                        ui::warn(&format!("cannot open vault {}: {msg}", info.id));
                    }
                }
            }
        }
        // Local replicas the server no longer lists: keep them read-only.
        for id in self.session.local_vault_ids() {
            if listed.contains(id.as_str()) || !self.id_may_be_allowed(&id) {
                continue;
            }
            let shared = match self.get(&id) {
                Some(s) => s,
                None => match self.session.load_local(&id) {
                    Ok(Some(v)) => {
                        let s = Arc::new(Mutex::new(v));
                        self.vaults.write().unwrap().insert(id.clone(), s.clone());
                        s
                    }
                    _ => continue,
                },
            };
            let mut v = shared.lock().await;
            if v.read_only.is_none() {
                v.read_only = Some(
                    "you no longer have access to this vault (local copy kept read-only)".into(),
                );
                v.mark_state_dirty();
                let _ = v.save();
            }
        }
        for id in &added {
            self.emit(HubEvent::VaultAdded {
                vault_id: id.clone(),
            });
        }
        Ok(added)
    }

    /// Resolve the allow-list from vault ids and the names cached in local
    /// replicas, so a command about one vault does not open all of them. Falls
    /// back to opening everything (names need decryption) when unsure.
    fn preresolve_allow_list(&self, list: &[VaultInfo]) {
        if self.allow.is_empty() {
            return;
        }
        let names: Vec<(String, Option<String>)> = list
            .iter()
            .map(|i| {
                let name = self
                    .session
                    .vault_store(&i.id)
                    .ok()
                    .and_then(|s| s.load_state().ok().flatten())
                    .map(|s| s.meta.name);
                (i.id.clone(), name)
            })
            .collect();
        let mut ids = BTreeSet::new();
        for r in &self.allow {
            if names.iter().any(|(id, _)| id == r) {
                ids.insert(r.clone());
                continue;
            }
            let matching: Vec<&String> = names
                .iter()
                .filter(|(_, n)| n.as_deref().is_some_and(|n| lower(n) == lower(r)))
                .map(|(id, _)| id)
                .collect();
            let unknown = names.iter().any(|(_, n)| n.is_none());
            match matching.as_slice() {
                [one] if !unknown => {
                    ids.insert((*one).clone());
                }
                _ => return,
            }
        }
        *self.allowed.write().unwrap() = Some(ids);
    }

    fn id_may_be_allowed(&self, id: &str) -> bool {
        match &*self.allowed.read().unwrap() {
            Some(set) => set.contains(id),
            None => true,
        }
    }

    /// Resolve the allow-list (names need decrypted metadata) and close every
    /// vault it does not admit.
    fn apply_allow_list(&self) -> Result<()> {
        if self.allow.is_empty() {
            return Ok(());
        }
        let mut ids = BTreeSet::new();
        let open: Vec<(String, String)> = self
            .vaults
            .read()
            .unwrap()
            .iter()
            .map(|(id, v)| {
                (
                    id.clone(),
                    v.try_lock()
                        .map(|g| g.name().to_string())
                        .unwrap_or_default(),
                )
            })
            .collect();
        for r in &self.allow {
            let by_id = open.iter().find(|(id, _)| id == r);
            let by_name: Vec<&(String, String)> =
                open.iter().filter(|(_, n)| lower(n) == lower(r)).collect();
            match (by_id, by_name.as_slice()) {
                (Some((id, _)), _) => {
                    ids.insert(id.clone());
                }
                (None, [one]) => {
                    ids.insert(one.0.clone());
                }
                (None, []) => bail!("no vault '{r}' is accessible to this profile"),
                (None, _) => bail!("vault name '{r}' is ambiguous; use the vault id"),
            }
        }
        self.vaults
            .write()
            .unwrap()
            .retain(|id, _| ids.contains(id));
        *self.allowed.write().unwrap() = Some(ids);
        Ok(())
    }

    pub fn is_online(&self) -> bool {
        self.online.load(Ordering::Relaxed)
    }

    /// The server's size limits: fetched once while online, otherwise the
    /// remembered ones (never "unlimited"). Call it before taking a vault lock.
    pub async fn limits(&self) -> ServerLimits {
        if self.is_online()
            && let Err(e) = self.session.server_info().await
        {
            tracing::debug!("server info: {e}");
        }
        self.session.limits()
    }

    pub fn set_online(&self, online: bool) {
        if self.online.swap(online, Ordering::Relaxed) != online {
            self.emit(HubEvent::Connection { online });
        }
    }

    /// Whether local edits should be uploaded right away (false while the
    /// live loop is disconnected; it pushes after reconnecting).
    pub fn set_push_now(&self, v: bool) {
        self.push_now.store(v, Ordering::Relaxed);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<HubEvent> {
        self.events.subscribe()
    }

    pub fn emit(&self, ev: HubEvent) {
        let _ = self.events.send(ev);
    }

    pub fn emit_applied(&self, vault_id: &str, a: &Applied, local: bool) {
        if a.is_empty() {
            return;
        }
        self.emit(HubEvent::Changed {
            vault_id: vault_id.to_string(),
            objects: a.objects.iter().cloned().collect(),
            index_changed: a.index_changed,
            purged: a.purged.iter().cloned().collect(),
            local,
        });
    }

    pub fn vault_ids(&self) -> Vec<String> {
        self.vaults.read().unwrap().keys().cloned().collect()
    }

    pub fn get(&self, id: &str) -> Option<SharedVault> {
        self.vaults.read().unwrap().get(id).cloned()
    }

    pub fn vaults(&self) -> Vec<SharedVault> {
        self.vaults.read().unwrap().values().cloned().collect()
    }

    /// Mark a vault read-only (access revoked, deleted).
    pub async fn close_vault(&self, id: &str, reason: &str) {
        if let Some(v) = self.get(id) {
            let mut g = v.lock().await;
            if g.read_only.is_none() {
                ui::warn(&format!(
                    "vault '{}': {reason}; the local copy is kept read-only",
                    g.name()
                ));
            }
            g.read_only = Some(reason.to_string());
            g.mark_state_dirty();
            let _ = g.save();
        }
        self.emit(HubEvent::VaultClosed {
            vault_id: id.to_string(),
            reason: reason.to_string(),
        });
    }

    /// Find an open vault by id or (unique, case-insensitive) name.
    pub async fn resolve(&self, reference: &str) -> Result<SharedVault> {
        if let Some(v) = self.get(reference) {
            return Ok(v);
        }
        let mut matches = Vec::new();
        for v in self.vaults() {
            let name = v.lock().await.name().to_string();
            if name == reference {
                return Ok(v);
            }
            if lower(&name) == lower(reference) {
                matches.push(v);
            }
        }
        match matches.len() {
            1 => Ok(matches.remove(0)),
            0 => bail!("no vault '{reference}' (see `coolpsonotes vault list`)"),
            _ => bail!("vault name '{reference}' is ambiguous; use the vault id"),
        }
    }

    pub async fn summaries(&self) -> Vec<VaultSummary> {
        let mut out = Vec::new();
        for v in self.vaults() {
            out.push(summary(&*v.lock().await));
        }
        out.sort_by(|a, b| lower(&a.name).cmp(&lower(&b.name)).then(a.id.cmp(&b.id)));
        out
    }

    fn push_lock(&self, vault_id: &str) -> Arc<Mutex<()>> {
        self.push_locks
            .lock()
            .unwrap()
            .entry(vault_id.to_string())
            .or_default()
            .clone()
    }

    /// Upload a vault's pending changes (serialised per vault).
    pub async fn push(&self, vault_id: &str) -> Result<PushOutcome> {
        let Some(v) = self.get(vault_id) else {
            bail!("vault {vault_id} is not open")
        };
        let lock = self.push_lock(vault_id);
        let _g = lock.lock().await;
        let out = self.session.push(&v).await?;
        if out.offline {
            self.online.store(false, Ordering::Relaxed);
        }
        Ok(out)
    }

    /// Catch up over REST, push and compact one vault.
    pub async fn sync_vault(&self, vault_id: &str) -> Result<PushOutcome> {
        let Some(v) = self.get(vault_id) else {
            bail!("vault {vault_id} is not open")
        };
        let applied = self.session.catch_up(&v).await?;
        self.emit_applied(vault_id, &applied, false);
        let out = self.push(vault_id).await?;
        if !out.offline {
            if let Err(e) = self.session.compact(&v, None).await {
                tracing::warn!("compaction failed: {e:#}");
            }
            if let Err(e) = self.session.grant_missing_keys(&v).await {
                tracing::warn!("granting keys failed: {e:#}");
            }
        }
        Ok(out)
    }

    /// Call after a local edit was applied and saved: notifies listeners and
    /// uploads it unless the live loop is disconnected.
    pub async fn local_changed(
        &self,
        vault_id: &str,
        objects: Vec<String>,
        index_changed: bool,
    ) -> PushOutcome {
        {
            let now = std::time::Instant::now();
            let mut edited = self.edited.lock().unwrap();
            for o in objects.iter().filter(|o| o.as_str() != INDEX_OBJECT_ID) {
                edited.insert((vault_id.to_string(), o.clone()), now);
            }
        }
        self.emit(HubEvent::Changed {
            vault_id: vault_id.to_string(),
            objects,
            index_changed,
            purged: vec![],
            local: true,
        });
        if !self.push_now.load(Ordering::Relaxed) {
            let pending = match self.get(vault_id) {
                Some(v) => v.lock().await.replica.pending.len(),
                None => 0,
            };
            return PushOutcome {
                remaining: pending,
                offline: true,
                ..Default::default()
            };
        }
        match self.push(vault_id).await {
            Ok(o) => o,
            Err(e) => {
                ui::warn(&format!(
                    "upload failed (changes are kept and retried): {e:#}"
                ));
                PushOutcome {
                    offline: true,
                    ..Default::default()
                }
            }
        }
    }

    /// Store a revision (snapshot) of every note edited here that has been
    /// idle for `idle` and gathered at least `min_updates` updates since its
    /// last snapshot. This is what makes revision history useful between the
    /// much rarer compactions. Returns the number of revisions stored.
    pub async fn snapshot_idle(&self, idle: std::time::Duration, min_updates: u32) -> usize {
        let due: Vec<(String, String)> = {
            let edited = self.edited.lock().unwrap();
            edited
                .iter()
                .filter(|(_, t)| t.elapsed() >= idle)
                .map(|(k, _)| k.clone())
                .collect()
        };
        let mut n = 0;
        for key in due {
            let (vid, oid) = &key;
            let Some(v) = self.get(vid) else {
                self.edited.lock().unwrap().remove(&key);
                continue;
            };
            let (count, pending) = {
                let g = v.lock().await;
                (
                    g.replica.since_snapshot.get(oid).copied().unwrap_or(0),
                    g.replica.pending.iter().any(|p| &p.object_id == oid),
                )
            };
            if pending || !self.is_online() {
                continue;
            }
            self.edited.lock().unwrap().remove(&key);
            if count < min_updates || g_read_only(&v).await {
                continue;
            }
            match self.session.snapshot_object(vid, &v, oid).await {
                Ok(true) => n += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!("storing a revision failed: {e:#}"),
            }
        }
        n
    }

    /// Save every vault with unsaved changes.
    pub async fn save_all(&self) {
        for v in self.vaults() {
            if let Err(e) = v.lock().await.save() {
                ui::warn(&format!("saving the local copy failed: {e:#}"));
            }
        }
    }

    pub fn is_allowed(&self, id: &str) -> bool {
        self.id_may_be_allowed(id) && self.get(id).is_some()
    }

    pub fn waiting_for_keys(&self) -> Vec<String> {
        self.waiting_for_keys
            .read()
            .unwrap()
            .iter()
            .cloned()
            .collect()
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

async fn g_read_only(v: &SharedVault) -> bool {
    let g = v.lock().await;
    g.read_only.is_some() || !g.replica.can_write()
}

pub fn summary(v: &Vault) -> VaultSummary {
    VaultSummary {
        id: v.id().to_string(),
        name: v.name().to_string(),
        icon: v.replica.meta.icon.clone(),
        role: v.role(),
        archived: v.archived,
        documents: v.notes().len(),
        pending_changes: v.replica.pending.len(),
        too_large_changes: v.outbox.held_count(),
        read_only: v.read_only.clone(),
        seq: v.replica.seq,
        last_sync: v.last_sync,
    }
}
