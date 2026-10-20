//! What to upload, and what to do with the server's answers.
//!
//! A replica's `pending` queue holds every local update the server hasn't
//! confirmed. The [`Outbox`] decides what goes out next: it merges queued
//! updates of the same object that were never sent (an offline session
//! stays one update per note instead of thousands), encrypts them with the
//! current keys, holds back those the server can never store, and splits
//! the rest into pushes the server accepts. It then turns acknowledgements
//! and refusals into the right follow-up: drop, hold, reload keys, re-send.

use std::collections::BTreeSet;

use coolpsonotes_core::crypto::Purpose;
use coolpsonotes_core::doc;
use coolpsonotes_core::engine::{Pending, VaultReplica};
use coolpsonotes_core::protocol::{
    ObjectKeyUpdate, PushResult, PushUpdate, REJECT_PURGED, REJECT_STALE_KEY, REJECT_TOO_LARGE,
    RejectedUpdate,
};
use coolpsonotes_core::random_id;
use serde::{Deserialize, Serialize};

/// The server's limits, as far as the client knows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Largest encrypted update the server stores (`/info` `max_update_bytes`).
    pub max_update_bytes: usize,
    /// Most updates in one push.
    pub max_push_count: usize,
}

impl Limits {
    /// Used until the server told us its own.
    pub const DEFAULT_MAX_UPDATE: usize = 2 << 20;
    pub const MAX_PUSH_COUNT: usize = 200;

    pub fn new(max_update_bytes: usize) -> Limits {
        Limits {
            max_update_bytes: max_update_bytes.max(1024),
            max_push_count: Self::MAX_PUSH_COUNT,
        }
    }

    /// Bytes one local change may cost (see `edits`) so that its encrypted
    /// update stays well within the limit (CRDT encoding and the envelope add
    /// to the text).
    pub fn edit_budget(&self) -> usize {
        (self.max_update_bytes / 2).max(1024)
    }

    /// Payload bytes per push: base64 in JSON keeps a full push well below
    /// the server's request and frame limits.
    pub fn push_bytes(&self) -> usize {
        self.max_update_bytes
    }
}

impl Default for Limits {
    fn default() -> Limits {
        Limits::new(Self::DEFAULT_MAX_UPDATE)
    }
}

/// An update kept back from uploading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub client_update_id: String,
    pub object_id: String,
    /// Encrypted size, if known.
    pub bytes: usize,
    pub reason: HoldReason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HoldReason {
    /// Larger than the server accepts; goes out again once it fits.
    TooLarge,
    /// The server refused it for another reason (bad request).
    Refused(String),
}

/// Upload bookkeeping kept next to a replica. `held` must be persisted with
/// the pending queue (it is local work the server doesn't have); the rest is
/// per connection.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Outbox {
    /// Updates kept back: the server can't store them (yet). They stay
    /// applied locally and are released when they fit.
    #[serde(default)]
    pub held: Vec<Pending>,
    /// Document keys of new notes the server doesn't know yet (R2). A note's
    /// updates wait until its key is registered, so nothing of it is ever
    /// encrypted with the vault key. Persisted with the queue: the key itself
    /// is in there only wrapped under the vault key.
    #[serde(default)]
    pub new_keys: Vec<NewKey>,
    /// The server keeps a key per note (it said which notes are shared,
    /// `VaultKeysResponse::shared`); older servers would treat every keyed
    /// note as shared, so new notes get no key there.
    #[serde(default)]
    pub note_keys: bool,
    /// Handed to the transport at least once: never merged again, so a retry
    /// carries the same id and the server stores it only once.
    #[serde(skip)]
    sent: BTreeSet<String>,
    /// Sent on the current connection and not acknowledged yet.
    #[serde(skip)]
    in_flight: BTreeSet<String>,
    /// Pushed one at a time to find which one an older server refuses.
    #[serde(skip)]
    alone: BTreeSet<String>,
    /// Held updates the user was already told about.
    #[serde(skip)]
    reported: BTreeSet<String>,
    /// The size limit each held update was held under: it is released only
    /// once the known limit grows past that (a server refusing an update we
    /// thought fits must not make us resend it forever).
    #[serde(skip)]
    held_under: std::collections::HashMap<String, usize>,
}

/// A new note's document key waiting to be registered
/// (`PUT /vaults/{v}/objects/{o}/key`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewKey {
    pub object_id: String,
    pub update: ObjectKeyUpdate,
}

/// The pushes to send now, and what changed in the queue.
#[derive(Debug, Default)]
pub struct Plan {
    /// Register these document keys first; the notes' updates follow in a
    /// later plan, after [`Outbox::key_registered`].
    pub register: Vec<NewKey>,
    pub pushes: Vec<Vec<PushUpdate>>,
    /// Newly held back (tell the user once).
    pub held: Vec<Held>,
    /// Queued updates that can't be encrypted right now (their key is being
    /// reloaded); they stay queued.
    pub waiting_for_keys: usize,
    /// Held updates that fit again and were queued.
    pub released: usize,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.pushes.is_empty() && self.register.is_empty()
    }

    pub fn ids(&self) -> Vec<String> {
        self.pushes
            .iter()
            .flatten()
            .map(|u| u.client_update_id.clone())
            .collect()
    }
}

/// What a client has to do after the server's answer.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reaction {
    /// Some updates need a newer key: reload keys, then plan again.
    pub reload_keys: bool,
    /// Objects permanently deleted on the server whose local changes were
    /// dropped (the text is still in the replica if the client wants to keep
    /// a copy).
    pub purged: Vec<String>,
    /// Newly held back.
    pub held: Vec<Held>,
    /// Something can be sent again right away.
    pub resend: bool,
}

impl Outbox {
    /// An outbox with updates held back by an earlier session (loaded from
    /// local storage).
    pub fn with_held(held: Vec<Pending>) -> Outbox {
        Outbox {
            held,
            ..Default::default()
        }
    }

    /// Give a note created here its own document key (R2), to be registered
    /// before its content goes out. Nothing happens if it has a key already,
    /// or for replicas that can't write or aren't the vault's members.
    pub fn new_document_key(&mut self, r: &mut VaultReplica, object_id: &str, title: &str) -> bool {
        if !self.note_keys || !r.member || !r.can_write() || r.doc_keys.contains_key(object_id) {
            return false;
        }
        let Ok((ring, update)) =
            coolpsonotes_core::engine::new_document_key(&r.keyring, object_id, title)
        else {
            return false;
        };
        r.doc_keys.insert(object_id.to_string(), ring);
        self.new_keys.push(NewKey {
            object_id: object_id.to_string(),
            update,
        });
        true
    }

    /// The server stored a new note's key: its updates may go out now.
    pub fn key_registered(&mut self, object_id: &str) {
        self.new_keys.retain(|k| k.object_id != object_id);
    }

    /// The server refused a new note's key. If it was wrapped under a vault
    /// key that is no longer the current one (the vault's key rotated
    /// meanwhile; reload keys first), a new key under the current vault key
    /// replaces it: nothing of the note left this device under the old one.
    /// Otherwise (e.g. the note had content already) the note stays under
    /// the vault key.
    pub fn key_refused(&mut self, r: &mut VaultReplica, object_id: &str) {
        let Some(pos) = self.new_keys.iter().position(|k| k.object_id == object_id) else {
            return;
        };
        let k = self.new_keys.remove(pos);
        r.doc_keys.remove(object_id);
        let wrapped_under = coolpsonotes_core::crypto::sym_epoch(&k.update.vault_envelope).ok();
        if wrapped_under < r.keyring.current_epoch() {
            let title = r
                .index
                .get(object_id)
                .map(|e| e.title().to_string())
                .unwrap_or_default();
            self.new_document_key(r, object_id, &title);
        }
    }

    /// The client replaced `r.doc_keys` with the keys the server has: new
    /// notes whose key is among them are registered (an answer that got lost),
    /// the others get their key back.
    pub fn keys_reloaded(&mut self, r: &mut VaultReplica, note_keys: bool) {
        self.note_keys = note_keys;
        self.new_keys
            .retain(|k| !r.doc_keys.contains_key(&k.object_id));
        self.restore_new_keys(r);
    }

    /// After loading the queue from storage: put the keys of new notes not
    /// registered yet back into the replica (unwrapped with the vault key),
    /// so their queued updates can be decrypted and sent.
    pub fn restore_new_keys(&mut self, r: &mut VaultReplica) {
        let vault_id = r.keyring.vault_id().to_string();
        self.new_keys.retain(|k| {
            if r.doc_keys.contains_key(&k.object_id) {
                return true;
            }
            match coolpsonotes_core::keys::unwrap_doc_key_for_vault(
                &r.keyring,
                &k.object_id,
                k.update.epoch,
                &k.update.vault_envelope,
            ) {
                Ok(dk) => {
                    let mut ring = coolpsonotes_core::keys::VaultKeyring::new(&vault_id);
                    ring.insert(k.update.epoch, dk);
                    r.doc_keys.insert(k.object_id.clone(), ring);
                    true
                }
                // The vault key it was wrapped under is gone: nothing to do.
                Err(_) => false,
            }
        });
    }

    /// Whether a new note's key is still to be registered.
    pub fn waits_for_key(&self, object_id: &str) -> bool {
        self.new_keys.iter().any(|k| k.object_id == object_id)
    }

    /// Number of updates kept back.
    pub fn held_count(&self) -> usize {
        self.held.len()
    }

    /// Everything this replica still has to upload (queued and held).
    pub fn unsent(&self, r: &VaultReplica) -> usize {
        r.pending.len() + self.held.len()
    }

    /// Plaintext bytes of queued updates not yet handed to the transport on
    /// this connection (e.g. to pace a bulk import).
    pub fn unsent_bytes(&self, r: &VaultReplica) -> usize {
        r.pending
            .iter()
            .filter(|p| !self.in_flight.contains(&p.client_update_id))
            .map(|p| p.update.len())
            .sum()
    }

    /// Whether an object has local changes the server doesn't have yet
    /// (queued or held back). A local copy of its state must not be treated
    /// as confirmed.
    pub fn unconfirmed(&self, r: &VaultReplica, object_id: &str) -> bool {
        self.waits_for_key(object_id)
            || r.pending
                .iter()
                .chain(self.held.iter())
                .any(|p| p.object_id == object_id)
    }

    /// The connection was lost or replaced: everything not acknowledged is
    /// sent again on the next plan (with the same ids).
    pub fn connection_reset(&mut self) {
        self.in_flight.clear();
    }

    /// These updates could not be handed to the transport (e.g. the socket
    /// is closed): plan them again later. Still mergeable when never sent.
    pub fn not_sent(&mut self, ids: &[String]) {
        for id in ids {
            self.in_flight.remove(id);
            self.sent.remove(id);
        }
    }

    /// Merge queued updates of the same object that were never sent [W-M9],
    /// as long as a merged update stays within `limits.edit_budget()` (a big
    /// paste was split on purpose). Applying a merged update equals applying
    /// the originals.
    pub fn merge_unsent(&mut self, r: &mut VaultReplica, limits: &Limits) {
        let budget = limits.edit_budget();
        let mut out: Vec<Pending> = Vec::with_capacity(r.pending.len());
        // Open groups by object: (object, member indices, bytes).
        let mut groups: Vec<(String, Vec<usize>, usize)> = Vec::new();
        let mut open: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for (i, p) in r.pending.iter().enumerate() {
            if self.sent.contains(&p.client_update_id) {
                continue;
            }
            let n = p.update.len();
            match open.get(&p.object_id) {
                Some(&g) if groups[g].2 + n <= budget => {
                    groups[g].1.push(i);
                    groups[g].2 += n;
                }
                _ => {
                    open.insert(p.object_id.clone(), groups.len());
                    groups.push((p.object_id.clone(), vec![i], n));
                }
            }
        }
        let groups: Vec<(String, Vec<usize>)> =
            groups.into_iter().map(|(o, v, _)| (o, v)).collect();
        let mut replace: std::collections::HashMap<usize, Option<Pending>> =
            std::collections::HashMap::new();
        for (object_id, idx) in groups.into_iter().filter(|(_, v)| v.len() > 1) {
            let updates: Vec<Vec<u8>> = idx.iter().map(|&i| r.pending[i].update.clone()).collect();
            let Ok(merged) = doc::merge_updates(&updates) else {
                continue;
            };
            replace.insert(
                idx[0],
                Some(Pending {
                    client_update_id: random_id(),
                    object_id,
                    update: merged,
                }),
            );
            for &i in &idx[1..] {
                replace.insert(i, None);
            }
        }
        if replace.is_empty() {
            return;
        }
        for (i, p) in std::mem::take(&mut r.pending).into_iter().enumerate() {
            match replace.remove(&i) {
                Some(Some(m)) => out.push(m),
                Some(None) => {}
                None => out.push(p),
            }
        }
        r.pending = out;
    }

    /// The pushes to send now. Updates in flight on this connection are not
    /// sent twice; call [`Outbox::connection_reset`] to send everything again.
    /// Read-only replicas send nothing.
    pub fn plan(&mut self, r: &mut VaultReplica, limits: &Limits) -> Plan {
        let mut plan = Plan::default();
        if !r.can_write() {
            return plan;
        }
        plan.released = self.release_fitting(r, limits);
        self.merge_unsent(r, limits);
        plan.register = self.new_keys.clone();
        let max = limits.max_update_bytes;
        let mut ready: Vec<PushUpdate> = Vec::new();
        let mut hold_ids = Vec::new();
        for p in &r.pending {
            if self.in_flight.contains(&p.client_update_id) || self.waits_for_key(&p.object_id) {
                continue;
            }
            match r.encrypt_object(Purpose::Update, &p.object_id, &p.update) {
                Ok(payload) if payload.len() > max => {
                    hold_ids.push(p.client_update_id.clone());
                    plan.held.push(Held {
                        client_update_id: p.client_update_id.clone(),
                        object_id: p.object_id.clone(),
                        bytes: payload.len(),
                        reason: HoldReason::TooLarge,
                    });
                }
                Ok(payload) => ready.push(PushUpdate {
                    object_id: p.object_id.clone(),
                    client_update_id: p.client_update_id.clone(),
                    payload,
                    signature: None,
                }),
                Err(_) => plan.waiting_for_keys += 1,
            }
        }
        self.hold(r, &hold_ids, max);
        plan.held
            .retain(|h| self.reported.insert(h.client_update_id.clone()));
        // Single pushes first for updates an older server refused as part of
        // a batch, then size- and count-bounded batches.
        let (alone, rest): (Vec<PushUpdate>, Vec<PushUpdate>) = ready
            .into_iter()
            .partition(|u| self.alone.contains(&u.client_update_id));
        plan.pushes.extend(alone.into_iter().map(|u| vec![u]));
        let mut cur: Vec<PushUpdate> = Vec::new();
        let mut bytes = 0;
        for u in rest {
            let n = u.payload.len();
            if !cur.is_empty()
                && (bytes + n > limits.push_bytes() || cur.len() >= limits.max_push_count)
            {
                plan.pushes.push(std::mem::take(&mut cur));
                bytes = 0;
            }
            bytes += n;
            cur.push(u);
        }
        if !cur.is_empty() {
            plan.pushes.push(cur);
        }
        for id in plan.ids() {
            self.sent.insert(id.clone());
            self.in_flight.insert(id);
        }
        plan
    }

    /// The server stored these updates. `in_order` is true for WebSocket
    /// acknowledgements, which arrive in log order and also move the replica's
    /// cursor; REST answers can overtake other clients' updates and must not.
    pub fn on_ack(&mut self, r: &mut VaultReplica, results: &[PushResult], in_order: bool) {
        for res in results {
            self.in_flight.remove(&res.client_update_id);
            self.sent.remove(&res.client_update_id);
            self.alone.remove(&res.client_update_id);
        }
        r.ack(results, in_order);
    }

    /// The server refused single updates of a push (the rest was stored).
    /// `limits` are the limits the client knew when it sent them.
    pub fn on_rejected(
        &mut self,
        r: &mut VaultReplica,
        rejected: &[RejectedUpdate],
        limits: &Limits,
    ) -> Reaction {
        let mut out = Reaction::default();
        let mut hold_ids = Vec::new();
        for x in rejected {
            self.in_flight.remove(&x.client_update_id);
            self.alone.remove(&x.client_update_id);
            match x.code.as_str() {
                REJECT_PURGED => {
                    self.drop_object(r, &x.object_id);
                    if !out.purged.contains(&x.object_id) {
                        out.purged.push(x.object_id.clone());
                    }
                }
                REJECT_STALE_KEY => {
                    out.reload_keys = true;
                }
                code => {
                    let reason = if code == REJECT_TOO_LARGE {
                        HoldReason::TooLarge
                    } else {
                        HoldReason::Refused(x.message.clone())
                    };
                    hold_ids.push(x.client_update_id.clone());
                    if self.reported.insert(x.client_update_id.clone()) {
                        out.held.push(Held {
                            client_update_id: x.client_update_id.clone(),
                            object_id: x.object_id.clone(),
                            bytes: 0,
                            reason,
                        });
                    }
                }
            }
        }
        self.hold(r, &hold_ids, limits.max_update_bytes);
        out.resend = !r.pending.is_empty() && !out.reload_keys;
        out
    }

    /// An older server refused a whole batch (one update too large, or for a
    /// purged object, without saying which): send its updates one at a time
    /// next, so the refusal names the culprit. A single refused update is held
    /// (`too_large`) or dropped with its object (`purged`, the object id given).
    pub fn on_batch_refused(
        &mut self,
        r: &mut VaultReplica,
        ids: &[String],
        too_large: bool,
        limits: &Limits,
    ) -> Reaction {
        for id in ids {
            self.in_flight.remove(id);
        }
        if ids.len() > 1 {
            self.alone.extend(ids.iter().cloned());
            return Reaction {
                resend: true,
                ..Default::default()
            };
        }
        let Some(id) = ids.first() else {
            return Reaction::default();
        };
        let object_id = r
            .pending
            .iter()
            .find(|p| &p.client_update_id == id)
            .map(|p| p.object_id.clone())
            .unwrap_or_default();
        let code = if too_large {
            REJECT_TOO_LARGE
        } else {
            REJECT_PURGED
        };
        self.on_rejected(
            r,
            &[RejectedUpdate {
                client_update_id: id.clone(),
                object_id,
                code: code.into(),
                message: String::new(),
            }],
            limits,
        )
    }

    /// Drop every local change of an object deleted for good on the server.
    pub fn drop_object(&mut self, r: &mut VaultReplica, object_id: &str) {
        r.pending.retain(|p| p.object_id != object_id);
        self.held.retain(|p| p.object_id != object_id);
    }

    fn hold(&mut self, r: &mut VaultReplica, ids: &[String], limit: usize) {
        if ids.is_empty() {
            return;
        }
        for id in ids {
            self.held_under.insert(id.clone(), limit);
        }
        let (bad, keep): (Vec<Pending>, Vec<Pending>) = std::mem::take(&mut r.pending)
            .into_iter()
            .partition(|p| ids.contains(&p.client_update_id));
        r.pending = keep;
        for p in bad {
            self.in_flight.remove(&p.client_update_id);
            if !self
                .held
                .iter()
                .any(|h| h.client_update_id == p.client_update_id)
            {
                self.held.push(p);
            }
        }
    }

    /// Queue held updates that fit within `limits` again (the administrator
    /// raised the limit). Returns how many.
    pub fn release_fitting(&mut self, r: &mut VaultReplica, limits: &Limits) -> usize {
        if self.held.is_empty() {
            return 0;
        }
        let max = limits.max_update_bytes;
        let held_under = &self.held_under;
        let (ok, keep): (Vec<Pending>, Vec<Pending>) =
            std::mem::take(&mut self.held).into_iter().partition(|p| {
                held_under
                    .get(&p.client_update_id)
                    .is_none_or(|&under| max > under)
                    && r.encrypt_object(Purpose::Update, &p.object_id, &p.update)
                        .is_ok_and(|c| c.len() <= max)
            });
        self.held = keep;
        let n = ok.len();
        for p in ok {
            self.reported.remove(&p.client_update_id);
            self.held_under.remove(&p.client_update_id);
            r.pending.push(p);
        }
        n
    }
}

/// The server's history went backwards (restored from an older backup, or a
/// rollback): move the cursor back to its head and queue the complete local
/// state of every object, so CRDT merging restores whatever it lost.
/// Returns how many objects were queued (0 for read-only replicas).
pub fn recover_from_rollback(r: &mut VaultReplica, head: i64) -> usize {
    r.seq = r.seq.min(head);
    r.max_head_seen = head;
    if !r.can_write() || !r.member {
        return 0;
    }
    let mut items = vec![(
        coolpsonotes_core::protocol::INDEX_OBJECT_ID.to_string(),
        r.index.encode_state(),
    )];
    for (id, n) in &r.notes {
        items.push((id.clone(), n.encode_state()));
    }
    let mut n = 0;
    for (id, state) in items {
        if r.local_change(&id, state).is_ok() {
            n += 1;
        }
    }
    n
}

/// Encrypted copies of the queued updates for storage, e.g. in browser
/// storage. An update that can't be encrypted right now (its key is being
/// reloaded) keeps its previously stored copy instead of being lost.
/// `None` if nothing at all could be produced although updates are queued
/// (then keep what is stored).
pub fn stored_copies(
    r: &VaultReplica,
    outbox: &Outbox,
    previous: &[PushUpdate],
) -> Option<Vec<PushUpdate>> {
    let all: Vec<&Pending> = r.pending.iter().chain(outbox.held.iter()).collect();
    let out: Vec<PushUpdate> = all
        .iter()
        .filter_map(|p| {
            r.encrypt_object(Purpose::Update, &p.object_id, &p.update)
                .ok()
                .map(|payload| PushUpdate {
                    object_id: p.object_id.clone(),
                    client_update_id: p.client_update_id.clone(),
                    payload,
                    signature: None,
                })
                .or_else(|| {
                    previous
                        .iter()
                        .find(|s| s.client_update_id == p.client_update_id)
                        .cloned()
                })
        })
        .collect();
    if out.is_empty() && !all.is_empty() {
        return None;
    }
    Some(out)
}
