//! When a client may store a snapshot (compaction or a revision).

use coolpsonotes_core::engine::VaultReplica;
use coolpsonotes_core::protocol::{CreateSnapshotRequest, INDEX_OBJECT_ID};

use crate::outbox::Outbox;

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
