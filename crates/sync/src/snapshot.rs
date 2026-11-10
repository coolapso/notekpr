//! When a client may store a snapshot (compaction or a revision).

use notekpr_core::engine::VaultReplica;
use notekpr_core::keys::VaultKeyring;
use notekpr_core::protocol::{CreateSnapshotRequest, INDEX_OBJECT_ID, ObjectKeyUpdate};

use crate::outbox::{Limits, Outbox};

/// What a snapshot of ours says it covers of the vault's log: everything up
/// to our cursor for a vault member, once caught up (`seq` ≥ 1). Someone a
/// single note is shared with doesn't know the vault's sequence numbers
/// (protocol §5.1): what they store covers none of the log (`0`), once that
/// note has caught up (`note_synced`).
fn covers(r: &VaultReplica, note_synced: bool) -> Option<i64> {
    match r.member {
        true => (r.seq >= 1).then_some(r.seq),
        false => note_synced.then_some(0),
    }
}

/// A snapshot of `object_id` to upload now, if one may be stored: the
/// replica can write and has caught up ([`covers`]: `note_synced` says so
/// for a note shared with us), no local change of the object is unconfirmed
/// (queued or held), it isn't deleted for good, it exists here, and none of
/// its content waits for a key (checked by `VaultReplica::snapshot_request`).
///
/// Callers must have applied every update of the object up to the cursor —
/// after a REST push that means catching up first (protocol §6).
pub fn snapshot_now(
    r: &VaultReplica,
    outbox: &Outbox,
    object_id: &str,
    note_synced: bool,
) -> Option<CreateSnapshotRequest> {
    let unconfirmed = outbox.unconfirmed(r, object_id);
    let exists = object_id == INDEX_OBJECT_ID || r.notes.contains_key(object_id);
    if !r.can_write() || unconfirmed || r.purged.contains(object_id) || !exists {
        return None;
    }
    let seq = covers(r, note_synced)?;
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
/// object up to the cursor, as for any snapshot (protocol §6); `note_synced`
/// as for [`snapshot_now`].
pub fn carrier_now(
    r: &VaultReplica,
    outbox: &Outbox,
    object_id: &str,
    note_synced: bool,
    limits: &Limits,
) -> Option<Carrier> {
    let held: Vec<String> = outbox
        .held
        .iter()
        .filter(|p| p.object_id == object_id)
        .map(|p| p.client_update_id.clone())
        .collect();
    let exists = object_id == INDEX_OBJECT_ID || r.notes.contains_key(object_id);
    if held.is_empty() || !r.can_write() || r.purged.contains(object_id) || !exists {
        return None;
    }
    let seq = covers(r, note_synced)?;
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
        update: Box<ObjectKeyUpdate>,
    },
}

/// [`snapshot_now`], moving a vault member's note that has no document key
/// yet onto its own key at the same time.
pub fn snapshot_action(
    r: &VaultReplica,
    outbox: &Outbox,
    object_id: &str,
    note_synced: bool,
) -> Option<SnapshotAction> {
    let req = snapshot_now(r, outbox, object_id, note_synced)?;
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
    let (ring, update) = notekpr_core::engine::adopt_document_key(
        &r.keyring,
        object_id,
        &title,
        &state,
        req.covers_seq,
    )
    .ok()?;
    Some(SnapshotAction::AdoptKey {
        ring,
        update: Box::new(update),
    })
}
