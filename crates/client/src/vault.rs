//! One vault's local replica plus the document operations every front end
//! (CLI, file mirror, gateway) shares.
//!
//! [`Vault`] wraps the core [`VaultReplica`] with persistence (dirty tracking
//! for [`VaultStore`]) and path-level operations. Nothing here touches the
//! network: local edits become pending CRDT updates that the sync layer pushes.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, anyhow, bail};
use coolpsonotes_core::b64;
use coolpsonotes_core::doc::{self, Entry, EntryKind, NoteDoc};
use coolpsonotes_core::engine::{Applied, Pending, VaultMeta, VaultReplica};
use coolpsonotes_core::keys::{VaultKey, VaultKeyring};
use coolpsonotes_core::protocol::{
    ChangesResponse, INDEX_OBJECT_ID, PushResult, REJECT_PURGED, RejectedUpdate, Role, Snapshot,
    Update,
};
use coolpsonotes_core::textdiff::{self, Edit};
use coolpsonotes_core::{markdown, now_ms, paths, random_id, search};
use coolpsonotes_sync::members::MemberLog;
use coolpsonotes_sync::{Limits, Outbox, Plan, Reaction};

use crate::store::{StoredState, VaultStore};

/// Result of creating or updating a note or attachment.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WriteResult {
    pub id: String,
    pub path: String,
    pub created: bool,
    pub changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ListKind {
    Folder,
    Note,
    Attachment,
}

#[derive(Debug, Clone)]
pub struct Listing {
    /// Display path (unique within the vault).
    pub path: String,
    pub kind: ListKind,
    pub entry: Option<Entry>,
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub path: String,
    pub entry: Entry,
    pub hit: search::Hit,
}

/// Merge two `Applied` summaries (the core keeps its merge private).
pub fn merge_applied(into: &mut Applied, o: Applied) {
    into.objects.extend(o.objects);
    into.index_changed |= o.index_changed;
    into.purged.extend(o.purged);
}

/// Content hash used for change detection and HTTP `ETag`s.
pub fn content_hash(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

pub struct Vault {
    pub replica: VaultReplica,
    pub owner_id: String,
    pub archived: bool,
    /// Why the vault became read-only locally (access revoked, deleted).
    pub read_only: Option<String>,
    pub pending_blobs: BTreeSet<String>,
    pub last_sync: Option<i64>,
    /// What to upload next (`coolpsonotes_sync`). Its `held` updates — too
    /// large for the server — are kept (and applied locally), not uploaded,
    /// so they never block the rest of the queue; stored as `quarantine`.
    pub outbox: Outbox,
    /// The vault's verified membership log (loaded from the server per
    /// session; `coolpsonotes_sync::members`).
    pub log: MemberLog,
    /// The last log entry verified here, `(seq, hash)`: kept across sessions.
    pub log_seen: Option<(u64, Vec<u8>)>,
    /// The server's limits as far as known (sizes edits and pushes).
    limits: Limits,
    store: VaultStore,
    dirty_objects: BTreeSet<String>,
    dirty_state: bool,
    dirty_pending: bool,
    dirty_quarantine: bool,
    detached: bool,
    /// Notes made from unsent edits of notes deleted for good elsewhere
    /// ([`Vault::rescue`]), still to be uploaded.
    rescued: Vec<String>,
}

/// A vault's keys as the server gives them to this principal.
pub struct Keys {
    pub ring: VaultKeyring,
    /// Document keyrings of its notes (any note may have one).
    pub docs: std::collections::HashMap<String, VaultKeyring>,
    /// Notes shared with someone individually.
    pub shared: BTreeSet<String>,
    /// The server keeps a key per note (R2).
    pub note_keys: bool,
}

/// Apply text edits to a note as one or more updates within `budget`
/// bytes each (see `coolpsonotes_sync::edits`).
pub fn apply_edits_chunked(doc: &NoteDoc, edits: &[Edit], budget: usize) -> Vec<Vec<u8>> {
    coolpsonotes_sync::edits::apply_chunked(doc, edits, budget)
}

impl Vault {
    pub fn new(store: VaultStore, replica: VaultReplica, owner_id: &str, archived: bool) -> Vault {
        Vault {
            replica,
            owner_id: owner_id.to_string(),
            archived,
            read_only: None,
            pending_blobs: BTreeSet::new(),
            last_sync: None,
            outbox: Outbox::default(),
            log: MemberLog::default(),
            log_seen: None,
            limits: Limits::default(),
            store,
            dirty_objects: BTreeSet::new(),
            dirty_state: true,
            dirty_pending: true,
            dirty_quarantine: false,
            detached: false,
            rescued: Vec::new(),
        }
    }

    /// Size local edits, pushes and carrying snapshots for the server's limits.
    pub fn set_limits(&mut self, l: &crate::profile::ServerLimits) {
        let size = |n: u64| n.min(usize::MAX as u64) as usize;
        self.limits =
            Limits::new(size(l.max_update_bytes)).with_snapshot(size(l.max_snapshot_bytes));
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Largest change one local edit update may carry (larger text edits are split).
    pub fn update_budget(&self) -> usize {
        self.limits.edit_budget()
    }

    /// Run an outbox operation, noting what has to be saved.
    fn with_outbox<T>(
        &mut self,
        f: impl FnOnce(&mut Outbox, &mut VaultReplica, &Limits) -> T,
    ) -> T {
        let held: Vec<String> = self
            .outbox
            .held
            .iter()
            .map(|p| p.client_update_id.clone())
            .collect();
        let pending = self.replica.pending.len();
        let limits = self.limits;
        let out = f(&mut self.outbox, &mut self.replica, &limits);
        if self.outbox.held.len() != held.len()
            || self
                .outbox
                .held
                .iter()
                .any(|p| !held.contains(&p.client_update_id))
        {
            self.dirty_quarantine = true;
        }
        // Merging or holding changes the queue even at the same length.
        if self.replica.pending.len() != pending || !held.is_empty() || pending > 0 {
            self.dirty_pending = true;
        }
        out
    }

    /// The server stored a snapshot of `object_id` carrying these held
    /// updates: they are done.
    pub fn carried(&mut self, object_id: &str, held: &[String]) {
        self.outbox.carried(held);
        self.replica.snapshot_created(object_id);
        self.dirty_quarantine = true;
        self.dirty_state = true;
    }

    /// The pushes to send now (merged, size-bounded, oversized ones held).
    pub fn plan_push(&mut self) -> Plan {
        self.with_outbox(|o, r, l| o.plan(r, l))
    }

    /// The server answered a REST push: stored `results`, refused `rejected`.
    pub fn push_answered(
        &mut self,
        results: &[PushResult],
        rejected: &[RejectedUpdate],
    ) -> Reaction {
        self.rescue_refused(rejected);
        self.dirty_state = true;
        self.with_outbox(|o, r, l| {
            o.on_ack(r, results, false);
            o.on_rejected(r, rejected, l)
        })
    }

    /// An older server refused a whole batch (`too_large`: 413, else 409).
    pub fn push_refused(&mut self, ids: &[String], too_large: bool) -> Reaction {
        self.with_outbox(|o, r, l| o.on_batch_refused(r, ids, too_large, l))
    }

    /// These updates were not delivered (offline, error): plan them again later.
    pub fn not_sent(&mut self, ids: &[String]) {
        self.outbox.not_sent(ids);
    }

    /// A new upload session: everything unacknowledged may be sent again.
    pub fn new_push_session(&mut self) {
        self.outbox.connection_reset();
    }

    /// Load the local replica, re-applying unacknowledged updates.
    pub fn load(store: VaultStore) -> Result<Option<Vault>> {
        let Some(s) = store.load_state()? else {
            return Ok(None);
        };
        let mut ring = VaultKeyring::new(&s.vault_id);
        for (epoch, k) in &s.keys {
            let key = b64::decode32(k).ok_or_else(|| anyhow!("corrupt local vault key"))?;
            ring.insert(*epoch, VaultKey(key));
        }
        let mut r = VaultReplica::new(&s.vault_id, s.role, ring, s.meta.clone());
        for (oid, epochs) in &s.doc_keys {
            let mut dr = VaultKeyring::new(&s.vault_id);
            for (epoch, k) in epochs {
                let key = b64::decode32(k).ok_or_else(|| anyhow!("corrupt local document key"))?;
                dr.insert(*epoch, VaultKey(key));
            }
            r.doc_keys.insert(oid.clone(), dr);
        }
        r.shared = s
            .shared
            .clone()
            .unwrap_or_else(|| r.doc_keys.keys().cloned().collect());
        r.seq = s.seq;
        r.max_head_seen = s.max_head_seen;
        r.since_snapshot = s.since_snapshot.into_iter().collect();
        r.purged = s.purged;
        for (id, state) in store.load_objects()? {
            if id == INDEX_OBJECT_ID {
                r.index.apply_update(&state)?;
            } else if !r.purged.contains(&id) {
                r.notes.entry(id).or_default().apply_update(&state)?;
            }
        }
        r.deferred = s.deferred.clone();
        r.pending = store.load_pending()?;
        let quarantined: Vec<Pending> = store.load_quarantine()?;
        // (A crash while moving one into quarantine can leave it in both.)
        r.pending.retain(|p| {
            !quarantined
                .iter()
                .any(|q| q.client_update_id == p.client_update_id)
        });
        for p in r.pending.iter().chain(&quarantined) {
            let res = if p.object_id == INDEX_OBJECT_ID {
                r.index.apply_update(&p.update)
            } else {
                r.notes
                    .entry(p.object_id.clone())
                    .or_default()
                    .apply_update(&p.update)
            };
            if let Err(e) = res {
                tracing::warn!("a queued local change could not be re-applied: {e}");
            }
        }
        let mut outbox = Outbox::with_held(quarantined);
        outbox.new_keys = s.new_keys;
        outbox.note_keys = s.note_keys;
        outbox.restore_new_keys(&mut r);
        Ok(Some(Vault {
            replica: r,
            owner_id: s.owner_id,
            archived: s.archived,
            read_only: s.read_only,
            pending_blobs: s.pending_blobs,
            last_sync: s.last_sync,
            outbox,
            log: MemberLog::default(),
            log_seen: s
                .log_seen
                .and_then(|(seq, h)| Some((seq, b64::decode(&h).ok()?))),
            limits: Limits::default(),
            store,
            dirty_objects: BTreeSet::new(),
            dirty_state: false,
            dirty_pending: false,
            dirty_quarantine: false,
            detached: false,
            rescued: Vec::new(),
        }))
    }

    pub fn store(&self) -> &VaultStore {
        &self.store
    }

    pub fn id(&self) -> &str {
        &self.replica.id
    }

    pub fn name(&self) -> &str {
        &self.replica.meta.name
    }

    pub fn role(&self) -> Role {
        self.replica.role
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty_state
            || self.dirty_pending
            || self.dirty_quarantine
            || !self.dirty_objects.is_empty()
    }

    pub fn mark_state_dirty(&mut self) {
        self.dirty_state = true;
    }

    /// Never write this copy back: another process owns the profile's files
    /// (see `profile::ProfileLock`); this one only reads them.
    pub fn detach(&mut self) {
        self.detached = true;
    }

    pub fn is_detached(&self) -> bool {
        self.detached
    }

    /// Persist everything that changed (pending first, then objects, then state).
    pub fn save(&mut self) -> Result<()> {
        if !self.is_dirty() || self.detached {
            return Ok(());
        }
        // Quarantine first: an update moved there is never in neither file.
        if self.dirty_quarantine {
            self.store.save_quarantine(&self.outbox.held)?;
            self.dirty_quarantine = false;
        }
        if self.dirty_pending {
            self.store.save_pending(&self.replica.pending)?;
        }
        // An object leaves the dirty set only once it is on disk: after a
        // failed save (disk full, …) the rest is still saved next time.
        for oid in self.dirty_objects.clone() {
            if oid == INDEX_OBJECT_ID {
                self.store
                    .save_object(&oid, &self.replica.index.encode_state())?;
            } else if let Some(n) = self.replica.notes.get(&oid) {
                self.store.save_object(&oid, &n.encode_state())?;
            } else {
                self.store.delete_object(&oid)?;
            }
            self.dirty_objects.remove(&oid);
        }
        let keys = self
            .replica
            .keyring
            .epochs()
            .filter_map(|e| self.replica.keyring.get(e).map(|k| (e, b64::encode(&k.0))))
            .collect();
        let doc_keys = self
            .replica
            .doc_keys
            .iter()
            .map(|(oid, ring)| {
                (
                    oid.clone(),
                    ring.epochs()
                        .filter_map(|e| ring.get(e).map(|k| (e, b64::encode(&k.0))))
                        .collect(),
                )
            })
            .collect();
        self.store.save_state(&StoredState {
            format: 1,
            vault_id: self.replica.id.clone(),
            role: self.replica.role,
            owner_id: self.owner_id.clone(),
            archived: self.archived,
            meta: self.replica.meta.clone(),
            keys,
            doc_keys,
            shared: Some(self.replica.shared.clone()),
            new_keys: self.outbox.new_keys.clone(),
            note_keys: self.outbox.note_keys,
            log_seen: self
                .log_seen
                .as_ref()
                .map(|(seq, h)| (*seq, b64::encode(h))),
            deferred: self.replica.deferred.clone(),
            seq: self.replica.seq,
            max_head_seen: self.replica.max_head_seen,
            since_snapshot: self
                .replica
                .since_snapshot
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            purged: self.replica.purged.clone(),
            pending_blobs: self.pending_blobs.clone(),
            read_only: self.read_only.clone(),
            last_sync: self.last_sync,
        })?;
        self.dirty_state = false;
        self.dirty_pending = false;
        Ok(())
    }

    // ------------------------------------------------------------ remote input

    fn mark_applied(&mut self, a: &Applied) {
        self.dirty_objects.extend(a.objects.iter().cloned());
        if a.index_changed {
            self.dirty_objects.insert(INDEX_OBJECT_ID.to_string());
        }
        if !a.purged.is_empty() {
            self.dirty_objects.extend(a.purged.iter().cloned());
            self.dirty_pending = true;
        }
        self.dirty_state = true;
    }

    pub fn apply_changes(&mut self, ch: &ChangesResponse) -> Applied {
        for p in &ch.purged {
            self.rescue(&p.object_id);
        }
        let (a, errors) = self.replica.apply_changes(ch);
        for e in errors {
            tracing::warn!(vault = %self.replica.id, "skipped a change that could not be decrypted: {e}");
        }
        self.mark_applied(&a);
        a
    }

    pub fn apply_update(&mut self, u: &Update) -> Result<Applied> {
        let a = self.replica.apply_update(u)?;
        self.mark_applied(&a);
        Ok(a)
    }

    pub fn apply_snapshot(&mut self, s: &Snapshot) -> Result<Applied> {
        let a = self.replica.apply_snapshot(s)?;
        self.mark_applied(&a);
        Ok(a)
    }

    /// Check content against `authors` (from the vault's log) from now on,
    /// and what waited for a fresh log once more (refused if it still fails).
    pub fn set_authors(
        &mut self,
        authors: Option<coolpsonotes_core::memberlog::Authors>,
    ) -> Applied {
        let (a, errors) = self.replica.set_authors(authors);
        for e in &errors {
            if matches!(e, coolpsonotes_core::engine::EngineError::Author(_)) {
                tracing::warn!(vault = %self.replica.id, "refused a change: {e}");
            }
        }
        self.mark_applied(&a);
        self.dirty_state = true;
        a
    }

    /// Replay content that arrived before we had its key.
    pub fn retry_deferred(&mut self) -> Applied {
        if !self.replica.needs_keys() {
            return Applied::default();
        }
        let (a, errors) = self.replica.retry_deferred();
        if !errors.is_empty() {
            tracing::debug!(vault = %self.replica.id, "{} item(s) still cannot be decrypted", errors.len());
        }
        self.mark_applied(&a);
        a
    }

    pub fn apply_purge(&mut self, object_id: &str, seq: i64) -> Applied {
        self.rescue(object_id);
        let before = self.outbox.held.len();
        self.outbox.drop_object(&mut self.replica, object_id);
        self.dirty_quarantine |= self.outbox.held.len() != before;
        let a = self.replica.apply_purge(object_id, seq);
        self.mark_applied(&a);
        a
    }

    pub fn synced(&mut self, head: i64) {
        self.replica.synced(head);
        self.dirty_state = true;
    }

    /// WebSocket acknowledgements (in log order) and refusals.
    pub fn ack(
        &mut self,
        results: &[PushResult],
        rejected: &[RejectedUpdate],
        in_order: bool,
    ) -> Reaction {
        self.rescue_refused(rejected);
        self.dirty_state = true;
        self.with_outbox(|o, r, l| {
            o.on_ack(r, results, in_order);
            o.on_rejected(r, rejected, l)
        })
    }

    pub fn set_keyring(&mut self, ring: VaultKeyring) {
        self.replica.keyring = ring;
        self.dirty_state = true;
    }

    /// Replace the vault keyring and the notes' document keyrings with what
    /// the server has.
    pub fn set_keys(&mut self, keys: Keys) {
        self.replica.keyring = keys.ring;
        self.replica.doc_keys = keys.docs;
        self.replica.shared = keys.shared;
        self.outbox.keys_reloaded(&mut self.replica, keys.note_keys);
        self.dirty_state = true;
        self.retry_deferred();
    }

    /// One note's keys, reloaded alone (see `Outbox::note_keys_reloaded`).
    pub fn set_note_keys(&mut self, object_id: &str, ring: VaultKeyring, shared: bool) {
        self.outbox
            .note_keys_reloaded(&mut self.replica, object_id, ring, shared);
        self.dirty_state = true;
        self.retry_deferred();
    }

    /// The server stored a new note's key.
    pub fn key_registered(&mut self, object_id: &str) {
        self.outbox.key_registered(object_id);
        self.dirty_state = true;
    }

    /// The server refused a new note's key (see `Outbox::key_refused`).
    pub fn key_refused(&mut self, object_id: &str) {
        self.outbox.key_refused(&mut self.replica, object_id);
        self.dirty_state = true;
    }

    /// The server accepted `ring` as a note's (new) keyring.
    pub fn set_note_keyring(&mut self, object_id: &str, ring: VaultKeyring) {
        self.replica.doc_keys.insert(object_id.to_string(), ring);
        self.dirty_state = true;
    }

    pub fn set_meta(&mut self, meta: VaultMeta) {
        self.replica.meta = meta;
        self.dirty_state = true;
    }

    /// Detect a server whose log went backwards (restored from an older
    /// backup, or a malicious rollback). Resets the cursor to the server's
    /// head and re-queues the full local state of every object; CRDT merging
    /// then restores whatever the server lost. Returns true on rollback.
    pub fn check_rollback(&mut self, head: i64) -> bool {
        if head >= self.replica.max_head_seen {
            return false;
        }
        tracing::warn!(
            vault = %self.replica.id,
            "the server's history went backwards (head {head} < {} seen before): it may have been restored \
             from a backup. Re-uploading the local state so nothing is lost",
            self.replica.max_head_seen
        );
        crate::ui::warn(&format!(
            "vault '{}': the server's history went backwards (possible restore or rollback); re-uploading local state",
            self.name()
        ));
        if self.read_only.is_some() {
            self.replica.seq = self.replica.seq.min(head);
            self.replica.max_head_seen = head;
        } else {
            let queued = coolpsonotes_sync::recover_from_rollback(&mut self.replica, head);
            if queued > 0 {
                self.dirty_pending = true;
                self.dirty_objects.extend(
                    std::iter::once(INDEX_OBJECT_ID.to_string())
                        .chain(self.replica.notes.keys().cloned()),
                );
            }
        }
        self.dirty_state = true;
        true
    }

    // ------------------------------------------------------------ local edits

    pub fn require_writable(&self) -> Result<()> {
        if let Some(r) = &self.read_only {
            bail!("vault '{}' is read-only on this device: {r}", self.name());
        }
        if !self.replica.can_write() {
            bail!("you have viewer access to vault '{}'", self.name());
        }
        Ok(())
    }

    /// Queue a local CRDT update for an object (already applied locally).
    pub fn record(&mut self, object_id: &str, update: Vec<u8>) -> Result<()> {
        self.replica.local_change(object_id, update)?;
        self.dirty_objects.insert(object_id.to_string());
        self.dirty_pending = true;
        Ok(())
    }

    fn record_index(&mut self, update: Option<Vec<u8>>) -> Result<()> {
        if let Some(u) = update {
            self.record(INDEX_OBJECT_ID, u)?;
        }
        Ok(())
    }

    /// Several index updates as one pending update (bulk operations).
    pub fn record_index_batch(&mut self, updates: Vec<Vec<u8>>) -> Result<()> {
        match updates.len() {
            0 => Ok(()),
            1 => self.record(
                INDEX_OBJECT_ID,
                updates.into_iter().next().unwrap_or_default(),
            ),
            _ => {
                let merged = doc::merge_updates(&updates)?;
                self.record(INDEX_OBJECT_ID, merged)
            }
        }
    }

    // ------------------------------------------------------------ queries

    /// Live entries by unique display path.
    pub fn paths(&self) -> BTreeMap<String, Entry> {
        self.replica.index.resolved_paths()
    }

    pub fn entry(&self, id: &str) -> Option<Entry> {
        self.replica.index.get(id)
    }

    /// Display path of a live entry.
    pub fn display_path(&self, id: &str) -> Option<String> {
        self.paths()
            .into_iter()
            .find(|(_, e)| e.id == id)
            .map(|(p, _)| p)
    }

    /// Find a live entry by display path, normalised path, path without
    /// `.md`, case-insensitive path, or entry id.
    pub fn find(&self, reference: &str) -> Option<(String, Entry)> {
        let paths = self.paths();
        if let Some(e) = paths.get(reference) {
            return Some((reference.to_string(), e.clone()));
        }
        if let Ok(p) = paths::normalize(reference) {
            for cand in [p.clone(), format!("{p}.md")] {
                if let Some(e) = paths.get(&cand) {
                    return Some((cand, e.clone()));
                }
            }
            let lower = p.to_lowercase();
            let lower_md = format!("{lower}.md");
            let mut ci = paths
                .iter()
                .filter(|(k, _)| k.to_lowercase() == lower || k.to_lowercase() == lower_md);
            if let (Some((k, e)), None) = (ci.next(), ci.next()) {
                return Some((k.clone(), e.clone()));
            }
        }
        paths.into_iter().find(|(_, e)| e.id == reference)
    }

    pub fn find_note(&self, reference: &str) -> Result<(String, Entry)> {
        match self.find(reference) {
            Some((p, e)) if e.kind == EntryKind::Note => Ok((p, e)),
            Some((p, _)) => bail!("'{p}' is not a note"),
            None => bail!("no document '{reference}' in vault '{}'", self.name()),
        }
    }

    pub fn is_folder(&self, path: &str) -> bool {
        let Ok(p) = paths::normalize(path) else {
            return path.trim_matches('/').is_empty();
        };
        self.replica.index.folders().contains(&p)
    }

    pub fn note_text(&self, id: &str) -> String {
        self.replica.note_text(id)
    }

    /// Live notes by display path.
    pub fn notes(&self) -> Vec<(String, Entry)> {
        self.paths()
            .into_iter()
            .filter(|(_, e)| e.kind == EntryKind::Note)
            .collect()
    }

    /// Entries directly inside `folder` ("" = root), or everything below it.
    pub fn list(&self, folder: &str, recursive: bool) -> Result<Vec<Listing>> {
        let folder = if folder.trim_matches('/').is_empty() {
            String::new()
        } else {
            paths::normalize(folder)?
        };
        if !folder.is_empty() && !self.is_folder(&folder) {
            bail!("no folder '{folder}' in vault '{}'", self.name());
        }
        let mut folders: BTreeSet<String> = BTreeSet::new();
        let mut files = Vec::new();
        for f in self.replica.index.folders() {
            if paths::is_within(&f, &folder) && f != folder {
                let rel = if folder.is_empty() {
                    f.as_str()
                } else {
                    &f[folder.len() + 1..]
                };
                if recursive || !rel.contains('/') {
                    folders.insert(f.clone());
                }
            }
        }
        for (p, e) in self.paths() {
            if e.kind == EntryKind::Folder || !paths::is_within(&p, &folder) {
                continue;
            }
            let rel = if folder.is_empty() {
                p.as_str()
            } else {
                &p[folder.len() + 1..]
            };
            if recursive || !rel.contains('/') {
                let kind = if e.kind == EntryKind::Note {
                    ListKind::Note
                } else {
                    ListKind::Attachment
                };
                files.push(Listing {
                    path: p.clone(),
                    kind,
                    entry: Some(e),
                });
            }
        }
        let mut out: Vec<Listing> = folders
            .into_iter()
            .map(|f| {
                let entry = self
                    .paths()
                    .get(&f)
                    .filter(|e| e.kind == EntryKind::Folder)
                    .cloned();
                Listing {
                    path: f,
                    kind: ListKind::Folder,
                    entry,
                }
            })
            .collect();
        out.extend(files);
        Ok(out)
    }

    /// `(id, path)` of all live entries, for link resolution.
    pub fn link_candidates(&self) -> Vec<(String, String)> {
        self.paths()
            .into_iter()
            .filter(|(_, e)| e.kind != EntryKind::Folder)
            .map(|(p, e)| (e.id, p))
            .collect()
    }

    /// Notes linking to the entry `target_id`.
    pub fn backlinks(&self, target_id: &str) -> Vec<(String, Entry)> {
        let cands = self.link_candidates();
        self.notes()
            .into_iter()
            .filter(|(p, e)| {
                e.id != target_id
                    && markdown::analyze(&self.note_text(&e.id))
                        .links
                        .iter()
                        .any(|l| markdown::resolve(l, p, &cands) == Some(target_id))
            })
            .collect()
    }

    pub fn tags(&self, id: &str) -> Vec<String> {
        markdown::analyze(&self.note_text(id)).tags
    }

    pub fn search(&self, q: &search::Query) -> Vec<SearchHit> {
        let mut hits: Vec<SearchHit> = self
            .notes()
            .into_iter()
            .filter_map(|(p, e)| {
                let text = self.note_text(&e.id);
                let tags = markdown::analyze(&text).tags;
                search::score(q, &p, &text, &tags).map(|hit| SearchHit {
                    path: p,
                    entry: e,
                    hit,
                })
            })
            .collect();
        hits.sort_by(|a, b| {
            b.hit
                .score
                .total_cmp(&a.hit.score)
                .then(a.path.cmp(&b.path))
        });
        hits
    }

    pub fn trashed(&self) -> Vec<Entry> {
        let mut v: Vec<Entry> = self
            .replica
            .index
            .entries()
            .into_iter()
            .filter(|e| e.trashed.is_some())
            .collect();
        v.sort_by(|a, b| b.trashed.cmp(&a.trashed).then(a.path.cmp(&b.path)));
        v
    }

    /// A note we have unsent edits of was deleted for good elsewhere: keep
    /// this copy's text as a new note, "<title> (recovered)" at the top of
    /// the vault (as the browser does), before the purge drops it. Nothing
    /// for notes without unsent edits.
    fn rescue(&mut self, object_id: &str) {
        let unsent = self
            .replica
            .pending
            .iter()
            .any(|p| p.object_id == object_id)
            || self.outbox.held.iter().any(|p| p.object_id == object_id);
        if !unsent || self.replica.purged.contains(object_id) || self.require_writable().is_err() {
            return;
        }
        let text = self.note_text(object_id);
        if text.is_empty() {
            return;
        }
        let title = self
            .replica
            .index
            .get(object_id)
            .map(|e| e.title().to_string())
            .unwrap_or_else(|| "Untitled".into());
        for n in 1..100 {
            let name = match n {
                1 => format!("{title} (recovered)"),
                n => format!("{title} (recovered {n})"),
            };
            if let Ok(w) = self.create_note(&format!("{name}.md"), &text) {
                crate::ui::warn(&format!(
                    "“{title}” was deleted by someone else; your unsent changes were saved as “{name}”"
                ));
                self.rescued.push(w.id);
                return;
            }
        }
    }

    /// Pushes refused because their note is gone for good.
    fn rescue_refused(&mut self, rejected: &[RejectedUpdate]) {
        let gone: BTreeSet<String> = rejected
            .iter()
            .filter(|r| r.code == REJECT_PURGED)
            .map(|r| r.object_id.clone())
            .collect();
        for o in gone {
            self.rescue(&o);
        }
    }

    /// Notes [rescued](Vault::rescue) since the last call, to upload.
    pub fn take_rescued(&mut self) -> Vec<String> {
        std::mem::take(&mut self.rescued)
    }

    // ------------------------------------------------------------ mutations

    /// Create a note, or diff-update it if it exists. Edits are merged as
    /// fine-grained CRDT operations, so concurrent edits elsewhere survive.
    pub fn write_note(&mut self, path: &str, text: &str) -> Result<WriteResult> {
        self.require_writable()?;
        if let Some((p, e)) = self.find(path) {
            if e.kind != EntryKind::Note {
                bail!("'{p}' exists and is not a note");
            }
            let changed = self.set_note_text(&e.id, text)?;
            return Ok(WriteResult {
                id: e.id,
                path: p,
                created: false,
                changed,
            });
        }
        self.create_note(path, text)
    }

    /// Create a new note; fails if the path is taken.
    pub fn create_note(&mut self, path: &str, text: &str) -> Result<WriteResult> {
        self.require_writable()?;
        let p = paths::normalize_note(path)?;
        if self.paths().contains_key(&p) {
            bail!("'{p}' already exists");
        }
        let id = random_id();
        let budget = self.update_budget();
        let note = self.replica.notes.entry(id.clone()).or_default();
        let updates = apply_edits_chunked(note, &textdiff::diff("", text), budget);
        if updates.is_empty() {
            self.dirty_objects.insert(id.clone());
        }
        for u in updates {
            self.record(&id, u)?;
        }
        let u = self.replica.index.create_with(
            &id,
            &p,
            EntryKind::Note,
            now_ms(),
            Some(text.len() as u64),
            None,
        );
        self.record_index(u)?;
        // Its own key, registered before any of its content is uploaded.
        if self
            .outbox
            .new_document_key(&mut self.replica, &id, paths::title(&p))
        {
            self.dirty_state = true;
        }
        Ok(WriteResult {
            id,
            path: p,
            created: true,
            changed: true,
        })
    }

    /// Replace a note's text via a diff. Returns false if nothing changed.
    pub fn set_note_text(&mut self, id: &str, text: &str) -> Result<bool> {
        self.require_writable()?;
        let budget = self.update_budget();
        let note = self.replica.notes.entry(id.to_string()).or_default();
        let old = note.text();
        if old == text {
            return Ok(false);
        }
        let updates = apply_edits_chunked(note, &textdiff::diff(&old, text), budget);
        if updates.is_empty() {
            return Ok(false);
        }
        for u in updates {
            self.record(id, u)?;
        }
        self.touch(id, text.len() as u64)?;
        Ok(true)
    }

    /// Merge an update produced elsewhere (e.g. a forked document in the file
    /// mirror) into a note and queue it.
    pub fn merge_note_update(&mut self, id: &str, update: Vec<u8>) -> Result<()> {
        self.merge_note_updates(id, vec![update])
    }

    /// [`Vault::merge_note_update`] for several updates (a split edit).
    pub fn merge_note_updates(&mut self, id: &str, updates: Vec<Vec<u8>>) -> Result<()> {
        self.require_writable()?;
        if updates.is_empty() {
            return Ok(());
        }
        for u in updates {
            self.replica
                .notes
                .entry(id.to_string())
                .or_default()
                .apply_update(&u)?;
            self.record(id, u)?;
        }
        let len = self.replica.notes.get(id).map_or(0, |n| n.len() as u64);
        self.touch(id, len)
    }

    fn touch(&mut self, id: &str, size: u64) -> Result<()> {
        let u = self.replica.index.touch(id, now_ms(), Some(size));
        self.record_index(u)
    }

    pub fn append_note(&mut self, path: &str, text: &str) -> Result<WriteResult> {
        self.require_writable()?;
        match self.find(path) {
            Some((p, e)) if e.kind == EntryKind::Note => {
                let budget = self.update_budget();
                let note = self.replica.notes.entry(e.id.clone()).or_default();
                let len = note.len();
                let insert = Edit::Insert {
                    at: len,
                    text: text.to_string(),
                };
                let updates = apply_edits_chunked(note, &[insert], budget);
                if updates.is_empty() {
                    return Ok(WriteResult {
                        id: e.id,
                        path: p,
                        created: false,
                        changed: false,
                    });
                }
                let new_len = note.len() as u64;
                for u in updates {
                    self.record(&e.id, u)?;
                }
                self.touch(&e.id, new_len)?;
                Ok(WriteResult {
                    id: e.id,
                    path: p,
                    created: false,
                    changed: true,
                })
            }
            Some((p, _)) => bail!("'{p}' is not a note"),
            None => self.create_note(path, text),
        }
    }

    pub fn create_folder(&mut self, path: &str) -> Result<String> {
        self.require_writable()?;
        let p = paths::normalize(path)?;
        if let Some(e) = self.paths().get(&p) {
            if e.kind == EntryKind::Folder {
                return Ok(e.id.clone());
            }
            bail!("'{p}' already exists");
        }
        let id = random_id();
        let u = self
            .replica
            .index
            .create(&id, &p, EntryKind::Folder, now_ms());
        self.record_index(u)?;
        Ok(id)
    }

    /// Set a note's, attachment's or folder's icon (an emoji), or remove it
    /// with `None`. A folder that only exists as the parent of others gets an
    /// entry for it. Returns the entry's id.
    pub fn set_icon(&mut self, path: &str, icon: Option<&str>) -> Result<String> {
        self.require_writable()?;
        let icon = match icon {
            Some(i) => doc::clean_icon(i).map_err(|e| anyhow!(e))?,
            None => None,
        };
        let id = match self.find(path) {
            Some((_, e)) => e.id,
            None if self.is_folder(path) => self.create_folder(path)?,
            None => bail!("no document or folder '{path}'"),
        };
        let u = self.replica.index.set_icon(&id, icon.as_deref());
        self.record_index(u)?;
        Ok(id)
    }

    /// Move or rename a note, attachment or folder. A target ending in `/`
    /// (or naming an existing folder) keeps the file name. Optionally rewrites
    /// wiki links in other notes that pointed at a moved note.
    pub fn move_path(&mut self, from: &str, to: &str, update_links: bool) -> Result<String> {
        self.require_writable()?;
        let into_folder = to.ends_with('/') || self.is_folder(to);
        if self.find(from).is_none() && self.is_folder(from) {
            let old = paths::normalize(from)?;
            let new = if into_folder {
                paths::join(&paths::normalize(to)?, paths::file_name(&old))
            } else {
                paths::normalize(to)?
            };
            if new == old || paths::is_within(&new, &old) {
                bail!("cannot move a folder into itself");
            }
            let u = self.replica.index.rename_folder(&old, &new, now_ms());
            self.record_index(u)?;
            return Ok(new);
        }
        let (old_path, entry) = self
            .find(from)
            .ok_or_else(|| anyhow!("no document or folder '{from}'"))?;
        let target = if into_folder {
            let base = if to.trim_matches('/').is_empty() {
                String::new()
            } else {
                paths::normalize(to)?
            };
            paths::join(&base, paths::file_name(&old_path))
        } else {
            to.to_string()
        };
        let new_path = match entry.kind {
            EntryKind::Note => paths::normalize_note(&target)?,
            _ => paths::normalize(&target)?,
        };
        if new_path == old_path {
            return Ok(new_path);
        }
        if let Some(other) = self.paths().get(&new_path)
            && other.id != entry.id
        {
            bail!("'{new_path}' already exists");
        }
        let before = self.link_candidates();
        let u = self.replica.index.rename(&entry.id, &new_path, now_ms());
        self.record_index(u)?;
        if update_links && entry.kind == EntryKind::Note {
            self.rewrite_links_to(&entry.id, &before, &new_path)?;
        }
        Ok(new_path)
    }

    /// Rewrite `[[links]]` that resolved to `target_id` (using the paths from
    /// before the rename) so they keep pointing at it.
    fn rewrite_links_to(
        &mut self,
        target_id: &str,
        before: &[(String, String)],
        new_path: &str,
    ) -> Result<usize> {
        let title = paths::title(new_path).to_string();
        let ambiguous = self
            .notes()
            .iter()
            .filter(|(p, e)| e.id != target_id && paths::title(p).eq_ignore_ascii_case(&title))
            .count()
            > 0;
        let new_target = if ambiguous {
            new_path.trim_end_matches(".md").to_string()
        } else {
            title
        };
        let mut changed = 0;
        for (p, e) in self.notes() {
            if e.id == target_id {
                continue;
            }
            let from_path = before
                .iter()
                .find(|(id, _)| *id == e.id)
                .map(|(_, p)| p.clone())
                .unwrap_or(p);
            let text = self.note_text(&e.id);
            let rewritten = markdown::rewrite_links(&text, &new_target, |l| {
                markdown::resolve(l, &from_path, before) == Some(target_id)
            });
            if let Some(t) = rewritten
                && self.set_note_text(&e.id, &t)?
            {
                changed += 1;
            }
        }
        Ok(changed)
    }

    /// Move an entry (or a folder and everything in it) to the trash.
    pub fn trash_path(&mut self, path: &str) -> Result<Vec<String>> {
        self.require_writable()?;
        if let Some((_, e)) = self.find(path)
            && e.kind != EntryKind::Folder
        {
            let u = self.replica.index.trash(&e.id, now_ms());
            self.record_index(u)?;
            return Ok(vec![e.id]);
        }
        if self.is_folder(path) {
            let folder = paths::normalize(path)?;
            let ids: Vec<String> = self
                .replica
                .index
                .entries()
                .into_iter()
                .filter(|e| e.is_live() && (e.path == folder || paths::is_within(&e.path, &folder)))
                .map(|e| e.id)
                .collect();
            let u = self.replica.index.trash_folder(&folder, now_ms());
            self.record_index(u)?;
            return Ok(ids);
        }
        bail!("no document or folder '{path}' in vault '{}'", self.name())
    }

    pub fn trash_id(&mut self, id: &str) -> Result<()> {
        self.require_writable()?;
        let u = self.replica.index.trash(id, now_ms());
        self.record_index(u)
    }

    /// Restore a trashed entry by id or by its (former) path.
    pub fn restore(&mut self, reference: &str) -> Result<Entry> {
        self.require_writable()?;
        let trashed = self.trashed();
        let e = trashed
            .iter()
            .find(|e| e.id == reference)
            .or_else(|| {
                let p = paths::normalize(reference).ok()?;
                trashed
                    .iter()
                    .find(|e| e.path == p || e.path == format!("{p}.md"))
            })
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "nothing named '{reference}' in the trash of vault '{}'",
                    self.name()
                )
            })?;
        let u = self.replica.index.restore(&e.id);
        self.record_index(u)?;
        Ok(e)
    }

    /// Drop a purged entry from the index.
    pub fn remove_entry(&mut self, id: &str) -> Result<()> {
        let u = self.replica.index.remove(id);
        self.record_index(u)
    }

    /// Encrypt an attachment, cache the ciphertext for upload and create its
    /// index entry. An existing attachment at the same path is moved to the
    /// trash (attachment blobs are immutable, so a new version is a new entry).
    pub fn add_attachment(
        &mut self,
        path: &str,
        data: &[u8],
        mime: Option<&str>,
    ) -> Result<WriteResult> {
        self.require_writable()?;
        let p = paths::normalize(path)?;
        let mut created = true;
        if let Some((existing, e)) = self.find(&p).filter(|(dp, _)| *dp == p) {
            if e.kind != EntryKind::Attachment {
                bail!("'{existing}' exists and is not an attachment");
            }
            if self.attachment_hash(&e.id).as_deref() == Some(content_hash(data).as_str()) {
                return Ok(WriteResult {
                    id: e.id,
                    path: existing,
                    created: false,
                    changed: false,
                });
            }
            let u = self.replica.index.trash(&e.id, now_ms());
            self.record_index(u)?;
            created = false;
        }
        let id = random_id();
        let ct = self.replica.encrypt_attachment(&id, data)?;
        self.store.save_blob(&id, &ct)?;
        self.pending_blobs.insert(id.clone());
        self.dirty_state = true;
        let mime = mime.map(str::to_string).or_else(|| {
            mime_guess::from_path(&p)
                .first()
                .map(|m| m.essence_str().to_string())
        });
        let u = self.replica.index.create_with(
            &id,
            &p,
            EntryKind::Attachment,
            now_ms(),
            Some(data.len() as u64),
            mime.as_deref(),
        );
        self.record_index(u)?;
        Ok(WriteResult {
            id,
            path: p,
            created,
            changed: true,
        })
    }

    /// Decrypt a cached attachment (None if it is not cached locally).
    pub fn cached_attachment(&self, id: &str) -> Result<Option<Vec<u8>>> {
        match self.store.load_blob(id)? {
            Some(ct) => Ok(Some(self.replica.decrypt_attachment(id, &ct)?)),
            None => Ok(None),
        }
    }

    fn attachment_hash(&self, id: &str) -> Option<String> {
        self.cached_attachment(id)
            .ok()
            .flatten()
            .map(|d| content_hash(&d))
    }
}

/// A fresh, independent copy of a note state (new random client id), used to
/// apply an external edit relative to the version the editor started from.
pub fn fork_note(state: &[u8]) -> Result<NoteDoc> {
    Ok(NoteDoc::from_updates([state])?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use coolpsonotes_core::keys::AccountSecret;

    fn vault(dir: &std::path::Path) -> Vault {
        let account = AccountSecret::generate();
        let (req, ring) =
            coolpsonotes_core::engine::new_vault(&account, "u1", &VaultMeta::named("T"), 1)
                .unwrap();
        let key = std::sync::Arc::new(crate::secrets::LocalKey::load_or_create(dir).unwrap());
        let store = VaultStore::new(dir.join("vaults"), key, &req.id).unwrap();
        Vault::new(
            store,
            VaultReplica::new(&req.id, Role::Owner, ring, VaultMeta::named("T")),
            "u1",
            false,
        )
    }

    #[test]
    fn rollback_resets_cursor_and_requeues_full_state() {
        let d = tempfile::tempdir().unwrap();
        let mut v = vault(d.path());
        let w = v.write_note("A.md", "hello").unwrap();
        v.replica.pending.clear();
        v.replica.seq = 40;
        v.replica.max_head_seen = 40;
        assert!(!v.check_rollback(40));
        assert!(v.check_rollback(12));
        assert_eq!(v.replica.seq, 12);
        assert_eq!(v.replica.max_head_seen, 12);
        let objects: BTreeSet<&str> = v
            .replica
            .pending
            .iter()
            .map(|p| p.object_id.as_str())
            .collect();
        assert!(objects.contains(INDEX_OBJECT_ID) && objects.contains(w.id.as_str()));
        // The re-queued state reproduces the document on a fresh replica.
        let p = v
            .replica
            .pending
            .iter()
            .find(|p| p.object_id == w.id)
            .unwrap();
        assert_eq!(
            NoteDoc::from_updates([p.update.as_slice()]).unwrap().text(),
            "hello"
        );
    }

    #[test]
    fn large_edits_are_split_into_updates_under_the_budget() {
        let doc = NoteDoc::new();
        let old = "start\n".repeat(10);
        doc.set_text(&old);
        // A huge paste of multibyte text plus many small edits.
        let new = format!(
            "{}{}{}",
            "start (edited)\n".repeat(10),
            "größer ✓ ".repeat(20_000),
            "end\n"
        );
        let budget = 16 * 1024;
        let initial = doc.encode_state();
        let updates = apply_edits_chunked(&doc, &textdiff::diff(&old, &new), budget);
        assert!(updates.len() > 10, "{}", updates.len());
        assert!(updates.iter().all(|u| u.len() <= budget + 1024));
        assert_eq!(doc.text(), new);
        // Another replica applying the updates in order gets the same text.
        let replay = NoteDoc::from_updates([initial.as_slice()]).unwrap();
        for u in &updates {
            replay.apply_update(u).unwrap();
        }
        assert_eq!(replay.text(), new);
        // Small edits stay one update.
        assert_eq!(
            apply_edits_chunked(&doc, &textdiff::diff(&new, "short"), budget).len(),
            1
        );
    }

    #[test]
    fn a_failed_save_keeps_everything_dirty_for_the_next_one() {
        let d = tempfile::tempdir().unwrap();
        let mut v = vault(d.path());
        v.save().unwrap();
        // Remote updates for several objects arrive; the objects directory
        // cannot be written (here: a file is in its way).
        let mut other = vault(d.path());
        for (i, name) in ["A.md", "B.md", "C.md"].iter().enumerate() {
            other.write_note(name, &format!("text {i}")).unwrap();
        }
        for p in other.replica.pending.clone() {
            let doc_update = p.update.clone();
            if p.object_id == INDEX_OBJECT_ID {
                v.replica.index.apply_update(&doc_update).unwrap();
            } else {
                v.replica
                    .notes
                    .entry(p.object_id.clone())
                    .or_default()
                    .apply_update(&doc_update)
                    .unwrap();
            }
            v.dirty_objects.insert(p.object_id.clone());
        }
        v.dirty_state = true;
        let objects = v.store.dir().join("objects");
        std::fs::remove_dir_all(&objects).ok();
        std::fs::write(&objects, "in the way").unwrap();
        assert!(v.save().is_err());
        assert_eq!(v.dirty_objects.len(), 4, "nothing may be forgotten");
        // The disk recovers: the next save writes all of them.
        std::fs::remove_file(&objects).unwrap();
        v.save().unwrap();
        assert!(!v.is_dirty());
        let store = v.store.clone();
        drop(v);
        let back = Vault::load(store).unwrap().unwrap();
        assert_eq!(back.notes().len(), 3);
        let (_, e) = back.find("B.md").unwrap();
        assert_eq!(back.note_text(&e.id), "text 1");
    }

    #[test]
    fn local_store_survives_reload_with_pending_updates() {
        let d = tempfile::tempdir().unwrap();
        let mut v = vault(d.path());
        v.write_note("Folder/Note.md", "one").unwrap();
        v.save().unwrap();
        // A pending edit is journalled; reload re-applies it.
        v.write_note("Folder/Note.md", "one two").unwrap();
        v.store.save_pending(&v.replica.pending).unwrap();
        let store = v.store.clone();
        drop(v);
        let back = Vault::load(store).unwrap().unwrap();
        let (_, e) = back.find("Folder/Note").unwrap();
        assert_eq!(back.note_text(&e.id), "one two");
        assert_eq!(back.replica.pending.len(), 4);
        // Nothing on disk is plaintext.
        for f in walkdir::WalkDir::new(d.path())
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let raw = std::fs::read(f.path()).unwrap();
            assert!(
                !raw.windows(7).any(|w| w == b"one two"),
                "{}",
                f.path().display()
            );
        }
    }
}
