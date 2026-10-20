//! When a client may store a snapshot (compaction or a revision).

use coolpsonotes_core::engine::VaultReplica;
use coolpsonotes_core::keys::VaultKeyring;
use coolpsonotes_core::protocol::{CreateSnapshotRequest, INDEX_OBJECT_ID, ObjectKeyUpdate};

use crate::outbox::{Limits, Outbox};

/// A snapshot of `object_id` to upload now, if one may be stored: the
/// replica can write and has caught up (`seq` ≥ 1), no local change of the
/// object is unconfirmed (queued or held), it isn't deleted for good, it
/// exists here, and none of its content waits for a key (checked by
/// `VaultReplica::snapshot_request`). `covers_seq` replaces the replica's
/// cursor for recipients of a single shared note, whose own sequence number
/// is the note's.
///
/// Callers must have applied every update of the object up to the cursor —
/// after a REST push that means catching up first (protocol §6).
pub fn snapshot_now(
    r: &VaultReplica,
    outbox: &Outbox,
    object_id: &str,
    covers_seq: Option<i64>,
) -> Option<CreateSnapshotRequest> {
    let unconfirmed = outbox.unconfirmed(r, object_id);
    let exists = object_id == INDEX_OBJECT_ID || r.notes.contains_key(object_id);
    let seq = covers_seq.unwrap_or(r.seq);
    if !r.can_write() || unconfirmed || r.purged.contains(object_id) || !exists || seq < 1 {
        return None;
    }
    let mut req = r.snapshot_request(object_id).ok()?;
    req.covers_seq = seq;
    Some(req)
}

/// A snapshot carrying held-back updates (see [`carrier_now`]).
#[derive(Debug)]
pub struct Carrier {
    /// `POST /vaults/{v}/objects/{o}/snapshots`, with `carries` set.
    pub request: CreateSnapshotRequest,
    /// The held updates it carries: [`Outbox::carried`] once it is stored.
    pub held: Vec<String>,
}

/// Updates of `object_id` too large to push (held back) can reach everyone
/// in a snapshot of the object's full state, which may be much larger: the
/// server sends it to everyone subscribed and with every catch-up from before
/// it. `None` when nothing is held, it can't be written now (read-only,
/// deleted, content waiting for a key) or even the whole state is larger than
/// the server's snapshot limit. Callers must have applied every update of the
/// object up to the cursor, as for any snapshot (protocol §6).
pub fn carrier_now(
    r: &VaultReplica,
    outbox: &Outbox,
    object_id: &str,
    covers_seq: Option<i64>,
    limits: &Limits,
) -> Option<Carrier> {
    let held: Vec<String> = outbox
        .held
        .iter()
        .filter(|p| p.object_id == object_id)
        .map(|p| p.client_update_id.clone())
        .collect();
    let exists = object_id == INDEX_OBJECT_ID || r.notes.contains_key(object_id);
    let seq = covers_seq.unwrap_or(r.seq);
    if held.is_empty() || !r.can_write() || r.purged.contains(object_id) || !exists || seq < 1 {
        return None;
    }
    let mut request = r.snapshot_request(object_id).ok()?;
    if request.payload.len() > limits.max_snapshot_bytes {
        return None;
    }
    request.covers_seq = seq;
    request.carries = true;
    Some(Carrier { request, held })
}

/// What storing a snapshot of a note means right now.
pub enum SnapshotAction {
    /// `POST /vaults/{v}/objects/{o}/snapshots`.
    Snapshot(CreateSnapshotRequest),
    /// The note's content is still under the vault key: give it its own
    /// (private) document key with this snapshot instead
    /// (`PUT /vaults/{v}/objects/{o}/key`; R2, lazy migration). Use `ring` as
    /// the note's keyring once the server accepted it.
    AdoptKey {
        ring: VaultKeyring,
        update: ObjectKeyUpdate,
    },
}

/// [`snapshot_now`], moving a vault member's note that has no document key
/// yet onto its own key at the same time.
pub fn snapshot_action(
    r: &VaultReplica,
    outbox: &Outbox,
    object_id: &str,
    covers_seq: Option<i64>,
) -> Option<SnapshotAction> {
    let req = snapshot_now(r, outbox, object_id, covers_seq)?;
    if !outbox.note_keys
        || !r.member
        || object_id == INDEX_OBJECT_ID
        || r.doc_keys.contains_key(object_id)
    {
        return Some(SnapshotAction::Snapshot(req));
    }
    let state = r.notes.get(object_id)?.encode_state();
    let title = r
        .index
        .get(object_id)
        .map(|e| e.title().to_string())
        .unwrap_or_default();
    let (ring, update) = coolpsonotes_core::engine::adopt_document_key(
        &r.keyring,
        object_id,
        &title,
        &state,
        req.covers_seq,
    )
    .ok()?;
    Some(SnapshotAction::AdoptKey { ring, update })
}
