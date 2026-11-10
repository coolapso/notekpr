//! The per-vault encrypted log: ordered updates, compacted snapshots, purges
//! and their undoing.
//!
//! The server cannot read any payload. It only enforces who may append, keeps
//! the log ordered (one sequence counter per vault), makes retries idempotent
//! (`client_update_id`), and prunes updates that are covered by snapshots.
//!
//! **Positions.** People a single note is shared with never see the vault's
//! sequence numbers: those would tell them how much happens in the rest of
//! the vault. Each note numbers its own updates and its comments' (`pos`,
//! the last one given in `objects.pos_head` of the note), and they are told
//! positions counted from where sharing started (`objects.pos_base`): an
//! update's `seq` is its position, a snapshot's `covers_seq` how far it
//! covers, `head_seq` the note's last position. Vault members get sequence
//! numbers as always.

use std::collections::HashMap;

use notekpr_core::keys::AccountPublic;
use notekpr_core::memberlog;
use notekpr_core::protocol::{
    ChangesResponse, CreateSnapshotRequest, INDEX_OBJECT_ID, ObjectEventKind, ObjectKind,
    ObjectState, PurgedItem, PurgedObject, PushResult, PushUpdate, REJECT_BAD_SIGNATURE,
    REJECT_PURGED, REJECT_STALE_KEY, REJECT_TOO_LARGE, REJECT_UNSIGNED, RejectedUpdate,
    RestoredObject, Snapshot, SnapshotInfo, Update, comments_note, comments_object,
};
use sqlx::Row;

use crate::access;
use crate::auth::Principal;
use crate::db::{self, Db};
use crate::error::{AppError, AppResult, bad, conflict, forbidden};
use crate::hub::Control;
use crate::hub::VaultEvent;
use crate::repo;
use crate::state::AppState;

pub const MAX_BATCH: usize = 500;
/// Largest comment record (an update of a note's comments object).
pub const MAX_COMMENT_BYTES: usize = 64 * 1024;
pub const DEFAULT_PAGE: i64 = 1000;
pub const MAX_PAGE: i64 = 5000;
/// Upper bound for the update payloads in one page of changes.
pub const PAGE_BYTES: usize = 16 << 20;

fn row_update(r: &sqlx::sqlite::SqliteRow) -> Update {
    Update {
        object_id: r.get("object_id"),
        seq: r.get("seq"),
        author_id: r.get("author_id"),
        payload: r.get("payload"),
        created_at: r.get("created_at"),
        signature: r.get("signature"),
    }
}

fn row_snapshot(r: &sqlx::sqlite::SqliteRow) -> Snapshot {
    Snapshot {
        id: r.get("id"),
        object_id: r.get("object_id"),
        covers_seq: r.get("covers_seq"),
        author_id: r.get("author_id"),
        payload: r.get("payload"),
        created_at: r.get("created_at"),
        signature: r.get("signature"),
    }
}

pub async fn head_seq(db: &Db, vault_id: &str) -> AppResult<i64> {
    sqlx::query_scalar("SELECT head_seq FROM vaults WHERE id = ?")
        .bind(vault_id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// Where a note's own numbering stands (module docs, *Positions*): where
/// sharing last started and the last position given. A note's comments
/// count with the note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotePositions {
    pub base: i64,
    pub head: i64,
}

impl NotePositions {
    /// A position as the people the note is shared with are told it.
    pub fn shown(self, pos: i64) -> i64 {
        (pos - self.base).max(0)
    }

    /// Their last position.
    pub fn shown_head(self) -> i64 {
        self.shown(self.head)
    }

    /// A position they name (`since`, a snapshot's `covers_seq`), as stored.
    pub fn stored(self, shown: i64) -> i64 {
        self.base.saturating_add(shown.max(0))
    }
}

pub async fn note_positions(
    db: impl sqlx::SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
) -> AppResult<NotePositions> {
    let note = comments_note(object_id).unwrap_or(object_id);
    let row: Option<(i64, i64)> =
        sqlx::query_as("SELECT pos_base, pos_head FROM objects WHERE vault_id = ? AND id = ?")
            .bind(vault_id)
            .bind(note)
            .fetch_optional(db)
            .await?;
    let (base, head) = row.ok_or(AppError::NotFound)?;
    Ok(NotePositions { base, head })
}

/// Give the next position in an object's note (the note itself for its
/// comments), as an update of it is stored. Inside the storing transaction.
async fn next_position(
    db: impl sqlx::SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
) -> AppResult<i64> {
    let note = comments_note(object_id).unwrap_or(object_id);
    Ok(sqlx::query_scalar(
        "UPDATE objects SET pos_head = pos_head + 1 WHERE vault_id = ? AND id = ? RETURNING pos_head",
    )
    .bind(vault_id)
    .bind(note)
    .fetch_optional(db)
    .await?
    // Comments need their note (`access::comment_access`); without it
    // nobody it could be shared with is told this one.
    .unwrap_or(0))
}

// Queries for people a single note is shared with keep only payloads
// encrypted with the key sharing started with or a later one: the envelope
// epoch (header bytes 2..5, big-endian) is at least the note's `share_from`.
// They get only those, never the note's history from before it was shared
// (under the vault key, or the note's earlier private keys).

/// Everything after `since`, optionally restricted to one object. Snapshots
/// (only when `include_snapshots`, i.e. on the first page) replace the
/// updates they cover. `doc_only`: for someone the note `object` is shared
/// with, who isn't a member ([`note_changes_since`]).
pub async fn changes_since(
    db: &Db,
    vault_id: &str,
    object: Option<&str>,
    since: i64,
    limit: i64,
    include_snapshots: bool,
    doc_only: bool,
) -> AppResult<ChangesResponse> {
    if doc_only {
        let note = object.ok_or_else(|| bad("which note?"))?;
        return note_changes_since(db, vault_id, note, since, limit, include_snapshots).await;
    }
    let head = head_seq(db, vault_id).await?;
    let mut snapshots = Vec::new();
    let mut covered: HashMap<String, i64> = HashMap::new();
    if include_snapshots {
        let rows = sqlx::query(
            "SELECT s.id, s.object_id, s.covers_seq, s.author_id, s.payload, s.created_at, s.signature
             FROM snapshots s
             JOIN objects o ON o.vault_id = s.vault_id AND o.id = s.object_id AND o.purged_at IS NULL
             WHERE s.vault_id = ? AND s.covers_seq > ? AND (? IS NULL OR s.object_id = ?)
               AND s.id = (SELECT s2.id FROM snapshots s2 WHERE s2.vault_id = s.vault_id AND s2.object_id = s.object_id
                           ORDER BY s2.covers_seq DESC, s2.created_at DESC LIMIT 1)",
        )
        .bind(vault_id)
        .bind(since)
        .bind(object)
        .bind(object)
        .fetch_all(db)
        .await?;
        for r in &rows {
            let s = row_snapshot(r);
            covered.insert(s.object_id.clone(), s.covers_seq);
            snapshots.push(s);
        }
    }
    // One note's changes include its comments.
    let comments = object
        .filter(|o| comments_note(o).is_none())
        .map(comments_object);
    // Objects deleted for good keep their updates while that can be undone;
    // they aren't served.
    let rows = sqlx::query(
        "SELECT object_id, seq, author_id, payload, created_at, signature FROM updates
         WHERE vault_id = ? AND seq > ? AND (? IS NULL OR object_id = ? OR object_id = ?)
           AND NOT EXISTS (SELECT 1 FROM objects gone WHERE gone.vault_id = updates.vault_id
                AND gone.id = updates.object_id AND gone.purged_at IS NOT NULL)
         ORDER BY seq LIMIT ?",
    )
    .bind(vault_id)
    .bind(since)
    .bind(object)
    .bind(object)
    .bind(&comments)
    .bind(limit + 1)
    .fetch_all(db)
    .await?;
    let mut has_more = rows.len() as i64 > limit;
    let mut last = since;
    let mut updates = Vec::with_capacity(rows.len());
    let mut bytes = 0usize;
    for r in rows.iter().take(limit as usize) {
        let u = row_update(r);
        // Pages are bounded in bytes too, so one page never holds thousands
        // of large updates; the rest follows on the next page.
        if !updates.is_empty() && bytes + u.payload.len() > PAGE_BYTES {
            has_more = true;
            break;
        }
        last = u.seq;
        if covered.get(&u.object_id).is_some_and(|c| u.seq <= *c) {
            continue;
        }
        bytes += u.payload.len();
        updates.push(u);
    }
    let next_since = if has_more { last } else { head };
    let upper = if has_more { last } else { head };
    // Snapshots carrying changes the log doesn't have, stored while the
    // log's head was within this page: every catch-up passes them once.
    let carried = sqlx::query(
        "SELECT s.id, s.object_id, s.covers_seq, s.author_id, s.payload, s.created_at, s.signature
         FROM snapshots s
         JOIN objects o ON o.vault_id = s.vault_id AND o.id = s.object_id AND o.purged_at IS NULL
         WHERE s.vault_id = ? AND s.carries = 1 AND s.created_head >= ? AND s.created_head <= ?
           AND (? IS NULL OR s.object_id = ?)
         ORDER BY s.created_head",
    )
    .bind(vault_id)
    .bind(since)
    .bind(upper)
    .bind(object)
    .bind(object)
    .fetch_all(db)
    .await?;
    for r in &carried {
        let s = row_snapshot(r);
        if !snapshots.iter().any(|x: &Snapshot| x.id == s.id) {
            snapshots.push(s);
        }
    }
    let purged = sqlx::query(
        "SELECT id, purged_seq FROM objects WHERE vault_id = ? AND purged_seq > ? AND purged_seq <= ? AND (? IS NULL OR id = ?)",
    )
        .bind(vault_id)
        .bind(since)
        .bind(upper)
        .bind(object)
        .bind(object)
        .fetch_all(db)
        .await?
        .iter()
        .map(|r| PurgedObject { object_id: r.get("id"), seq: r.get("purged_seq") })
        .collect();
    // Also from the start: a copy can have dropped the object without
    // having caught up yet (deleted for good from that copy).
    let restored = sqlx::query(
        "SELECT id, restored_seq FROM objects WHERE vault_id = ? AND restored_seq > ? AND restored_seq <= ? AND (? IS NULL OR id = ?)",
    )
        .bind(vault_id)
        .bind(since)
        .bind(upper)
        .bind(object)
        .bind(object)
        .fetch_all(db)
        .await?
        .iter()
        .map(|r| RestoredObject { object_id: r.get("id"), seq: r.get("restored_seq") })
        .collect();
    Ok(ChangesResponse {
        updates,
        snapshots,
        purged,
        restored,
        head_seq: head,
        has_more,
        next_since,
    })
}

/// A snapshot row (with `covers_pos`) as someone the note is shared with
/// is told it: in positions, and without its signature, which signs the
/// vault's sequence number (they could find it by trying them all). Their
/// apps don't check the signatures of what members store (they don't have
/// the vault's membership log).
fn shown_snapshot(r: &sqlx::sqlite::SqliteRow, p: NotePositions) -> Snapshot {
    Snapshot {
        covers_seq: p.shown(r.get("covers_pos")),
        signature: None,
        ..row_snapshot(r)
    }
}

/// One note's part of the log for someone it is shared with who isn't a
/// member: its updates and its comments' from where sharing started, after
/// position `since`, all numbered in positions (module docs).
async fn note_changes_since(
    db: &Db,
    vault_id: &str,
    object: &str,
    since: i64,
    limit: i64,
    include_snapshots: bool,
) -> AppResult<ChangesResponse> {
    let limit = limit.clamp(1, MAX_PAGE);
    let note = comments_note(object).unwrap_or(object);
    let comments = comments_object(note);
    let p = note_positions(db, vault_id, note).await?;
    let since = since.max(0);
    let from = p.stored(since);
    let mut snapshots = Vec::new();
    let mut covered = None;
    if include_snapshots {
        // The newest one they may read. From the start, always: it holds
        // everything from before sharing started, which no update gives.
        let row = sqlx::query(
            "SELECT s.id, s.object_id, s.covers_seq, s.covers_pos, s.author_id, s.payload, s.created_at, s.signature
             FROM snapshots s
             JOIN objects o ON o.vault_id = s.vault_id AND o.id = s.object_id AND o.purged_at IS NULL
             WHERE s.vault_id = ? AND s.object_id = ? AND substr(s.payload, 2, 4) >= o.share_from
             ORDER BY s.covers_seq DESC, s.created_at DESC LIMIT 1",
        )
        .bind(vault_id)
        .bind(note)
        .fetch_optional(db)
        .await?;
        if let Some(r) = row.filter(|r| since == 0 || r.get::<i64, _>("covers_pos") > from) {
            covered = Some(r.get::<i64, _>("covers_seq"));
            snapshots.push(shown_snapshot(&r, p));
        }
    }
    let rows = sqlx::query(
        "SELECT object_id, seq, pos, author_id, payload, created_at, signature FROM updates
         WHERE vault_id = ? AND (object_id = ? OR object_id = ?) AND pos > ?
           AND substr(payload, 2, 4) >= (SELECT ob.share_from FROM objects ob
                WHERE ob.vault_id = updates.vault_id AND ob.id = ?)
           AND NOT EXISTS (SELECT 1 FROM objects gone WHERE gone.vault_id = updates.vault_id
                AND gone.id = updates.object_id AND gone.purged_at IS NOT NULL)
         ORDER BY pos LIMIT ?",
    )
    .bind(vault_id)
    .bind(note)
    .bind(&comments)
    .bind(from)
    .bind(note)
    .bind(limit + 1)
    .fetch_all(db)
    .await?;
    let mut has_more = rows.len() as i64 > limit;
    let mut last = from;
    let mut updates = Vec::with_capacity(rows.len());
    let mut bytes = 0usize;
    for r in rows.iter().take(limit as usize) {
        let u = row_update(r);
        if !updates.is_empty() && bytes + u.payload.len() > PAGE_BYTES {
            has_more = true;
            break;
        }
        let pos: i64 = r.get("pos");
        last = pos;
        // The snapshot covers the note's updates up to its sequence number.
        if u.object_id == note && covered.is_some_and(|c| u.seq <= c) {
            continue;
        }
        bytes += u.payload.len();
        updates.push(Update {
            seq: p.shown(pos),
            ..u
        });
    }
    let upper = if has_more { last } else { p.head };
    let carried = sqlx::query(
        "SELECT s.id, s.object_id, s.covers_seq, s.covers_pos, s.author_id, s.payload, s.created_at, s.signature
         FROM snapshots s
         JOIN objects o ON o.vault_id = s.vault_id AND o.id = s.object_id AND o.purged_at IS NULL
         WHERE s.vault_id = ? AND s.object_id = ? AND s.carries = 1
           AND s.created_pos >= ? AND s.created_pos <= ?
           AND substr(s.payload, 2, 4) >= o.share_from
         ORDER BY s.created_pos",
    )
    .bind(vault_id)
    .bind(note)
    .bind(from)
    .bind(upper)
    .fetch_all(db)
    .await?;
    for r in &carried {
        let s = shown_snapshot(r, p);
        if !snapshots.iter().any(|x: &Snapshot| x.id == s.id) {
            snapshots.push(s);
        }
    }
    // A note deleted for good, or brought back, is shared with nobody: no
    // `purged` or `restored` for them.
    Ok(ChangesResponse {
        updates,
        snapshots,
        purged: vec![],
        restored: vec![],
        head_seq: p.shown_head(),
        has_more,
        next_since: p.shown(upper),
    })
}

/// Current state of one object: latest snapshot plus the updates after it.
/// `doc_only`: for someone the note is shared with, in positions.
pub async fn object_state(
    db: &Db,
    vault_id: &str,
    object_id: &str,
    doc_only: bool,
) -> AppResult<ObjectState> {
    if doc_only {
        return note_state(db, vault_id, object_id).await;
    }
    let head = head_seq(db, vault_id).await?;
    let purged: Option<Option<i64>> =
        sqlx::query_scalar("SELECT purged_at FROM objects WHERE vault_id = ? AND id = ?")
            .bind(vault_id)
            .bind(object_id)
            .fetch_optional(db)
            .await?;
    match purged {
        None => {
            return Ok(ObjectState {
                object_id: object_id.to_string(),
                snapshot: None,
                updates: vec![],
                head_seq: head,
            });
        }
        Some(Some(_)) => return Err(AppError::NotFound),
        Some(None) => {}
    }
    let snapshot = sqlx::query(
        "SELECT id, object_id, covers_seq, author_id, payload, created_at, signature FROM snapshots
         WHERE vault_id = ? AND object_id = ? ORDER BY covers_seq DESC, created_at DESC LIMIT 1",
    )
    .bind(vault_id)
    .bind(object_id)
    .fetch_optional(db)
    .await?
    .map(|r| row_snapshot(&r));
    let after = snapshot.as_ref().map(|s| s.covers_seq).unwrap_or(0);
    let updates = sqlx::query(
        "SELECT object_id, seq, author_id, payload, created_at, signature FROM updates
         WHERE vault_id = ? AND object_id = ? AND seq > ? ORDER BY seq",
    )
    .bind(vault_id)
    .bind(object_id)
    .bind(after)
    .fetch_all(db)
    .await?
    .iter()
    .map(row_update)
    .collect();
    Ok(ObjectState {
        object_id: object_id.to_string(),
        snapshot,
        updates,
        head_seq: head,
    })
}

/// [`object_state`] for someone the note is shared with: only what they may
/// read, in positions.
async fn note_state(db: &Db, vault_id: &str, object_id: &str) -> AppResult<ObjectState> {
    not_purged(db, vault_id, object_id).await?;
    let p = note_positions(db, vault_id, object_id).await?;
    let note = comments_note(object_id).unwrap_or(object_id);
    let snapshot = sqlx::query(
        "SELECT s.id, s.object_id, s.covers_seq, s.covers_pos, s.author_id, s.payload, s.created_at, s.signature
         FROM snapshots s JOIN objects o ON o.vault_id = s.vault_id AND o.id = s.object_id
         WHERE s.vault_id = ? AND s.object_id = ? AND substr(s.payload, 2, 4) >= o.share_from
         ORDER BY s.covers_seq DESC, s.created_at DESC LIMIT 1",
    )
    .bind(vault_id)
    .bind(object_id)
    .fetch_optional(db)
    .await?;
    let after = snapshot
        .as_ref()
        .map(|r| r.get("covers_seq"))
        .unwrap_or(0i64);
    let updates = sqlx::query(
        "SELECT object_id, seq, pos, author_id, payload, created_at, signature FROM updates
         WHERE vault_id = ? AND object_id = ? AND seq > ?
           AND substr(payload, 2, 4) >= (SELECT ob.share_from FROM objects ob
                WHERE ob.vault_id = updates.vault_id AND ob.id = ?)
         ORDER BY seq",
    )
    .bind(vault_id)
    .bind(object_id)
    .bind(after)
    .bind(note)
    .fetch_all(db)
    .await?
    .iter()
    .map(|r| Update {
        seq: p.shown(r.get("pos")),
        ..row_update(r)
    })
    .collect();
    Ok(ObjectState {
        object_id: object_id.to_string(),
        snapshot: snapshot.map(|r| shown_snapshot(&r, p)),
        updates,
        head_seq: p.shown_head(),
    })
}

pub fn validate_object_id(id: &str) -> AppResult<()> {
    if id == INDEX_OBJECT_ID || db::valid_id(id) {
        Ok(())
    } else {
        Err(bad("invalid object id"))
    }
}

/// Shared notes accept only content encrypted with their current document
/// key; other notes only vault-key content. The key epoch is in the
/// (authenticated, but not secret) ciphertext header.
/// Content must be encrypted with the current key: the note's own key for
/// individually shared notes, otherwise the vault's current key. Anything
/// older is refused with `stale_key` (the client reloads keys and
/// re-encrypts), so after a key rotation nobody keeps writing content that
/// removed members could still read.
pub fn check_payload_key(
    vault_epoch: i64,
    object_key_epoch: Option<i64>,
    payload: &[u8],
) -> AppResult<()> {
    use notekpr_core::keys::is_doc_epoch;
    let epoch =
        notekpr_core::crypto::sym_epoch(payload).map_err(|_| bad("malformed ciphertext"))?;
    match object_key_epoch {
        Some(cur) if !is_doc_epoch(epoch) || (epoch as i64) < cur => Err(AppError::StaleKey(
            "this note now has its own key; reload keys and encrypt with it".into(),
        )),
        Some(cur) if (epoch as i64) > cur => Err(bad("unknown document key epoch")),
        Some(_) => Ok(()),
        None if is_doc_epoch(epoch) => Err(bad("document key used for a note that is not shared")),
        None if (epoch as i64) < vault_epoch => Err(AppError::StaleKey(
            "the vault has a new key; reload keys and encrypt with it".into(),
        )),
        None if (epoch as i64) > vault_epoch => Err(bad("unknown vault key epoch")),
        None => Ok(()),
    }
}

async fn vault_key_epoch(db: impl sqlx::SqliteExecutor<'_>, vault_id: &str) -> AppResult<i64> {
    sqlx::query_scalar("SELECT key_epoch FROM vaults WHERE id = ?")
        .bind(vault_id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// May `who` write to `object_id`? Vault editors/owners, or people the note
/// is shared with as editor; for a note's comments, see
/// [`access::comment_access`].
async fn write_access(
    db: &Db,
    member: Option<notekpr_core::protocol::Role>,
    vault_id: &str,
    object_id: &str,
    user_id: &str,
) -> AppResult<()> {
    if let Some(note) = comments_note(object_id) {
        return access::comment_access(db, member, vault_id, note, user_id).await;
    }
    match member {
        Some(r) if r.can_write() => Ok(()),
        Some(_) => Err(forbidden("viewers cannot modify this vault")),
        None => match access::share_role(db, vault_id, object_id, user_id).await? {
            Some(r) if r.can_write() => Ok(()),
            Some(_) => Err(forbidden("this note is shared with you read-only")),
            None => Err(AppError::NotFound),
        },
    }
}

pub struct PushOutcome {
    pub results: Vec<PushResult>,
    /// Retries of updates that were already stored (not broadcast again).
    pub duplicates: Vec<PushResult>,
    /// Updates refused one by one; the rest of the batch was stored.
    pub rejected: Vec<RejectedUpdate>,
    pub head_seq: i64,
}

/// Why content can't be stored for its signature, if so: unsigned where the
/// vault requires signatures, or not signed with the author's account key.
/// (Clients check authorship themselves; this keeps honest servers clean.)
pub fn signature_problem(
    signed_from: Option<i64>,
    signer: Option<&AccountPublic>,
    signature: Option<&[u8]>,
    verify: impl Fn(&AccountPublic, &[u8]) -> bool,
) -> Option<(&'static str, &'static str)> {
    match signature {
        None if signed_from.is_some() => Some((
            REJECT_UNSIGNED,
            "this vault requires signed changes; update the app",
        )),
        None => None,
        Some(sig) if signer.is_some_and(|k| verify(k, sig)) => None,
        Some(_) => Some((
            REJECT_BAD_SIGNATURE,
            "the signature doesn't match your account key",
        )),
    }
}

fn rejected(u: &PushUpdate, code: &str, message: &str) -> RejectedUpdate {
    RejectedUpdate {
        client_update_id: u.client_update_id.clone(),
        object_id: u.object_id.clone(),
        code: code.to_string(),
        message: message.to_string(),
    }
}

/// Append encrypted updates. Duplicate `client_update_id`s return the original
/// sequence number without appending again, so clients can retry safely.
/// An update that can't be stored on its own merits (too large, for a purged
/// object, under an outdated key, a new object without room: a note in full
/// quick notes, a drawing in a vault holding as many as it may) is refused
/// alone and listed in `rejected`; the others are stored. Malformed requests and missing access
/// still fail the whole push.
pub async fn push(
    state: &AppState,
    who: &Principal,
    vault_id: &str,
    updates: Vec<PushUpdate>,
    origin_conn: u64,
) -> AppResult<PushOutcome> {
    if updates.is_empty() {
        // Nothing to store: the head for members, nothing else for anyone
        // (not even whether the vault exists).
        let member = access::member_role_opt(&state.db, vault_id, &who.user_id).await?;
        return Ok(PushOutcome {
            results: vec![],
            duplicates: vec![],
            rejected: vec![],
            head_seq: match member {
                Some(_) => head_seq(&state.db, vault_id).await?,
                None => 0,
            },
        });
    }
    if updates.len() > MAX_BATCH {
        return Err(bad(format!("at most {MAX_BATCH} updates per request")));
    }
    let max = state.instance.settings().max_update_bytes() as usize;
    for u in &updates {
        validate_object_id(&u.object_id)?;
        if u.client_update_id.is_empty() || u.client_update_id.len() > 64 {
            return Err(bad("invalid client_update_id"));
        }
        if u.payload.is_empty() {
            return Err(bad("empty payload"));
        }
    }
    let mut refused = Vec::new();
    let updates: Vec<PushUpdate> = updates
        .into_iter()
        .filter(|u| {
            let limit = if comments_note(&u.object_id).is_some() {
                MAX_COMMENT_BYTES.min(max)
            } else {
                max
            };
            let ok = u.payload.len() <= limit;
            if !ok {
                refused.push(rejected(
                    u,
                    REJECT_TOO_LARGE,
                    "larger than the server's update limit",
                ));
            }
            ok
        })
        .collect();
    // Access is checked under the write lock, so a push can't land after
    // the writer was removed (membership changes take the same lock).
    let _w = state.write_lock.lock().await;
    // Checked here for WebSocket pushes (REST ones are refused before):
    // under the lock, so nothing lands after read-only mode began.
    state.instance.require_writable()?;
    let member = access::member_role_opt(&state.db, vault_id, &who.user_id).await?;
    if member.is_none() && head_seq(&state.db, vault_id).await.is_err() {
        return Err(AppError::NotFound);
    }
    let objects: std::collections::BTreeSet<&str> =
        updates.iter().map(|u| u.object_id.as_str()).collect();
    for o in objects {
        write_access(&state.db, member.clone(), vault_id, o, &who.user_id).await?;
    }
    let signer = repo::users::account_key(&state.db, &who.user_id)
        .await?
        .and_then(|k| k.verify().ok());
    let now = db::now();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let vault_epoch = vault_key_epoch(&mut *tx, vault_id).await?;
    let signed_from = repo::vaults::signed_from(&mut *tx, vault_id).await?;
    // What new objects may still be made: quick notes and drawings are
    // limited.
    let mut room = state.instance.new_objects(&mut tx, vault_id).await?;
    let mut results = Vec::with_capacity(updates.len());
    let mut duplicates = Vec::new();
    let mut fresh: Vec<(Update, String, i64)> = Vec::new();
    // Checked before the first new update: retries of stored ones are fine.
    let mut room_checked = false;
    // Someone a note is shared with (not a member) is answered in that
    // note's positions (module docs); the notes they wrote to, for the head.
    let shown = |p: NotePositions, seq: i64, pos: i64| {
        if member.is_some() { seq } else { p.shown(pos) }
    };
    let mut notes: HashMap<String, NotePositions> = HashMap::new();
    for u in updates {
        let note = comments_note(&u.object_id)
            .unwrap_or(&u.object_id)
            .to_string();
        let p = match (&member, notes.get(&note)) {
            (Some(_), _) => NotePositions { base: 0, head: 0 },
            (None, Some(p)) => *p,
            (None, None) => {
                let p = note_positions(&mut *tx, vault_id, &note).await?;
                notes.insert(note.clone(), p);
                p
            }
        };
        let existing: Option<(i64, i64)> = sqlx::query_as(
            "SELECT seq, pos FROM updates WHERE vault_id = ? AND client_update_id = ?",
        )
        .bind(vault_id)
        .bind(&u.client_update_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((seq, pos)) = existing {
            let r = PushResult {
                client_update_id: u.client_update_id,
                seq: shown(p, seq, pos),
            };
            duplicates.push(r.clone());
            results.push(r);
            continue;
        }
        let obj: Option<(Option<i64>, Option<i64>)> = sqlx::query_as(
            "SELECT purged_at, key_epoch FROM objects WHERE vault_id = ? AND id = ?",
        )
        .bind(vault_id)
        .bind(&u.object_id)
        .fetch_optional(&mut *tx)
        .await?;
        // A note's comments use the note's key.
        let key_epoch = match comments_note(&u.object_id) {
            Some(note) => sqlx::query_scalar::<_, Option<i64>>(
                "SELECT key_epoch FROM objects WHERE vault_id = ? AND id = ?",
            )
            .bind(vault_id)
            .bind(note)
            .fetch_optional(&mut *tx)
            .await?
            .flatten(),
            None => obj.and_then(|o| o.1),
        };
        if let Some((code, message)) = signature_problem(
            signed_from,
            signer.as_ref(),
            u.signature.as_deref(),
            |k, sig| memberlog::verify_update(k, vault_id, &u.object_id, &u.payload, sig),
        ) {
            refused.push(rejected(&u, code, message));
            continue;
        }
        match check_payload_key(vault_epoch, key_epoch, &u.payload) {
            Ok(()) => {}
            Err(AppError::StaleKey(m)) => {
                refused.push(rejected(&u, REJECT_STALE_KEY, &m));
                continue;
            }
            Err(e) => return Err(e),
        }
        if !room_checked && !matches!(obj, Some((Some(_), _))) {
            state.instance.require_room(&mut *tx, vault_id).await?;
            room_checked = true;
        }
        match obj.map(|o| o.0) {
            Some(Some(_)) => {
                refused.push(rejected(
                    &u,
                    REJECT_PURGED,
                    "the note was permanently deleted",
                ));
                continue;
            }
            Some(None) => {
                sqlx::query("UPDATE objects SET updated_at = ? WHERE vault_id = ? AND id = ?")
                    .bind(now)
                    .bind(vault_id)
                    .bind(&u.object_id)
                    .execute(&mut *tx)
                    .await?;
            }
            None => {
                // The index and comments are what they are, whatever is said.
                let kind =
                    if u.object_id == INDEX_OBJECT_ID || comments_note(&u.object_id).is_some() {
                        ObjectKind::Note
                    } else {
                        u.kind
                    };
                if let Err(no) = state
                    .instance
                    .admit(&mut tx, &mut room, &u.object_id, kind)
                    .await?
                {
                    refused.push(rejected(&u, no.code, &no.message));
                    continue;
                }
                sqlx::query(
                    "INSERT INTO objects (vault_id, id, created_by, created_at, updated_at, kind) VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(vault_id)
                .bind(&u.object_id)
                .bind(&who.user_id)
                .bind(now)
                .bind(now)
                .bind(kind.as_str())
                .execute(&mut *tx)
                .await?;
            }
        }
        let seq: i64 =
            sqlx::query_scalar("UPDATE vaults SET head_seq = head_seq + 1, updated_at = ? WHERE id = ? RETURNING head_seq")
                .bind(now)
                .bind(vault_id)
                .fetch_one(&mut *tx)
                .await?;
        let pos = next_position(&mut *tx, vault_id, &u.object_id).await?;
        if let Some(p) = notes.get_mut(&note) {
            p.head = pos;
        }
        sqlx::query(
            "INSERT INTO updates (vault_id, seq, object_id, client_update_id, author_id, device_id, payload, created_at, signature, pos)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(vault_id)
        .bind(seq)
        .bind(&u.object_id)
        .bind(&u.client_update_id)
        .bind(&who.user_id)
        .bind(&who.device_id)
        .bind(&u.payload)
        .bind(now)
        .bind(&u.signature)
        .bind(pos)
        .execute(&mut *tx)
        .await?;
        results.push(PushResult {
            client_update_id: u.client_update_id.clone(),
            seq: shown(p, seq, pos),
        });
        fresh.push((
            Update {
                object_id: u.object_id,
                seq,
                author_id: who.user_id.clone(),
                payload: u.payload,
                created_at: now,
                signature: u.signature,
            },
            u.client_update_id,
            pos,
        ));
    }
    let head: i64 = match member {
        Some(_) => {
            sqlx::query_scalar("SELECT head_seq FROM vaults WHERE id = ?")
                .bind(vault_id)
                .fetch_one(&mut *tx)
                .await?
        }
        None => notes.values().map(|p| p.shown_head()).max().unwrap_or(0),
    };
    tx.commit().await?;
    metrics::counter!("notekpr_updates_total").increment(fresh.len() as u64);
    let stored = !fresh.is_empty();
    for (update, client_update_id, pos) in fresh {
        state.hub.publish(
            vault_id,
            VaultEvent::Update {
                update,
                client_update_id,
                origin_conn,
                pos,
            },
        );
    }
    if stored {
        storage_changed(state, vault_id, false).await;
    }
    Ok(PushOutcome {
        results,
        duplicates,
        rejected: refused,
        head_seq: head,
    })
}

/// Store a client-produced compaction of an object. Updates covered by the
/// *previous* snapshot are pruned, so one generation of updates always remains
/// as a safety net against a bad snapshot.
pub async fn create_snapshot(
    state: &AppState,
    who: &Principal,
    vault_id: &str,
    object_id: &str,
    req: CreateSnapshotRequest,
) -> AppResult<SnapshotInfo> {
    let CreateSnapshotRequest {
        snapshot_id,
        covers_seq,
        payload,
        signature,
        carries,
    } = req;
    let snapshot_id = snapshot_id.as_str();
    validate_object_id(object_id)?;
    if comments_note(object_id).is_some() {
        // Each comment keeps its author's signature: never compacted.
        return Err(bad("comments have no snapshots"));
    }
    if !db::valid_id(snapshot_id) {
        return Err(bad("invalid snapshot id"));
    }
    if payload.is_empty() {
        return Err(bad("empty payload"));
    }
    let settings = state.instance.settings();
    if payload.len() as u64 > settings.max_snapshot_bytes() {
        return Err(AppError::TooLarge);
    }
    let _w = state.write_lock.lock().await;
    let member = access::member_role_opt(&state.db, vault_id, &who.user_id).await?;
    let is_member = member.is_some();
    write_access(&state.db, member, vault_id, object_id, &who.user_id).await?;
    if is_member {
        let head = head_seq(&state.db, vault_id).await?;
        if covers_seq < 1 || covers_seq > head {
            return Err(bad("covers_seq out of range"));
        }
    } else if covers_seq != 0 {
        // They don't know the vault's sequence numbers (module docs): what
        // they store covers none of its log, so it never stands in for
        // updates in a catch-up.
        return Err(bad("covers_seq is 0 for someone the note is shared with"));
    }
    let obj: Option<(Option<i64>, Option<i64>)> =
        sqlx::query_as("SELECT purged_at, key_epoch FROM objects WHERE vault_id = ? AND id = ?")
            .bind(vault_id)
            .bind(object_id)
            .fetch_optional(&state.db)
            .await?;
    let Some((None, key_epoch)) = obj else {
        return Err(AppError::NotFound);
    };
    check_payload_key(
        vault_key_epoch(&state.db, vault_id).await?,
        key_epoch,
        &payload,
    )?;
    // Clients snapshot a note after many updates or when an editing session
    // ends; a cooldown per author and note keeps anyone from flooding the
    // history and pushing real revisions out of retention.
    let last: Option<i64> = sqlx::query_scalar(
        "SELECT MAX(created_at) FROM snapshots WHERE vault_id = ? AND object_id = ? AND author_id = ?",
    )
    .bind(vault_id)
    .bind(object_id)
    .bind(&who.user_id)
    .fetch_one(&state.db)
    .await?;
    let now = db::now();
    if last.is_some_and(|t| now - t < settings.snapshot_cooldown_ms()) {
        return Err(AppError::RateLimited);
    }
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    state.instance.require_room(&mut *tx, vault_id).await?;
    let covers_pos = insert_snapshot(
        &mut tx,
        settings.snapshot_retention,
        &who.user_id,
        vault_id,
        object_id,
        snapshot_id,
        covers_seq,
        &payload,
        signature.as_deref(),
        now,
        carries,
    )
    .await?;
    tx.commit().await?;
    // One carrying changes the log doesn't have goes to everyone now.
    if carries {
        state.hub.publish(
            vault_id,
            VaultEvent::Snapshot {
                snapshot: Snapshot {
                    id: snapshot_id.to_string(),
                    object_id: object_id.to_string(),
                    covers_seq,
                    author_id: who.user_id.clone(),
                    payload: payload.clone(),
                    created_at: now,
                    signature: signature.clone(),
                },
                covers_pos,
            },
        );
    }
    state.hub.publish(
        vault_id,
        VaultEvent::SnapshotCreated {
            object_id: object_id.to_string(),
            snapshot_id: snapshot_id.to_string(),
            covers_seq,
            covers_pos,
        },
    );
    storage_changed(state, vault_id, false).await;
    Ok(SnapshotInfo {
        id: snapshot_id.to_string(),
        object_id: object_id.to_string(),
        covers_seq,
        author_id: who.user_id.clone(),
        created_at: now,
        size: payload.len() as i64,
    })
}

/// Insert a snapshot, prune the update generation covered by the previous
/// snapshot, and apply revision retention (the newest carrying snapshot is
/// kept too: it holds what the log doesn't). Runs inside the caller's
/// transaction. Returns how far it covers in the note's positions (module
/// docs).
#[allow(clippy::too_many_arguments)]
pub async fn insert_snapshot(
    tx: &mut sqlx::Transaction<'static, sqlx::Sqlite>,
    retention: u32,
    author_id: &str,
    vault_id: &str,
    object_id: &str,
    snapshot_id: &str,
    covers_seq: i64,
    payload: &[u8],
    signature: Option<&[u8]>,
    now: i64,
    carries: bool,
) -> AppResult<i64> {
    let signer = repo::users::account_key(&mut **tx, author_id)
        .await?
        .and_then(|k| k.verify().ok());
    let required = repo::vaults::signed_from(&mut **tx, vault_id).await?;
    if let Some((code, message)) =
        signature_problem(required, signer.as_ref(), signature, |k, sig| {
            memberlog::verify_snapshot(k, vault_id, object_id, covers_seq, payload, sig)
        })
    {
        return Err(if code == REJECT_UNSIGNED {
            conflict(message)
        } else {
            bad(message)
        });
    }
    let size = payload.len() as i64;
    let prev: Option<i64> = sqlx::query_scalar(
        "SELECT covers_seq FROM snapshots WHERE vault_id = ? AND object_id = ? ORDER BY covers_seq DESC LIMIT 1",
    )
    .bind(vault_id)
    .bind(object_id)
    .fetch_optional(&mut **tx)
    .await?;
    let head: i64 = sqlx::query_scalar("SELECT head_seq FROM vaults WHERE id = ?")
        .bind(vault_id)
        .fetch_one(&mut **tx)
        .await?;
    // The last position of the note's updates (and comments) it covers. Its
    // pruned updates are covered by an earlier snapshot, which knows theirs.
    let covers_pos: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(p), 0) FROM (
            SELECT MAX(pos) AS p FROM updates
             WHERE vault_id = ? AND (object_id = ? OR object_id = ?) AND seq <= ?
            UNION ALL
            SELECT MAX(covers_pos) FROM snapshots
             WHERE vault_id = ? AND object_id = ? AND covers_seq <= ?)",
    )
    .bind(vault_id)
    .bind(object_id)
    .bind(comments_object(object_id))
    .bind(covers_seq)
    .bind(vault_id)
    .bind(object_id)
    .bind(covers_seq)
    .fetch_one(&mut **tx)
    .await?;
    let created_pos = note_positions(&mut **tx, vault_id, object_id).await?.head;
    let r = sqlx::query(
        "INSERT INTO snapshots (id, vault_id, object_id, covers_seq, author_id, payload, size, created_at, signature, carries, created_head, covers_pos, created_pos)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(snapshot_id)
    .bind(vault_id)
    .bind(object_id)
    .bind(covers_seq)
    .bind(author_id)
    .bind(payload)
    .bind(size)
    .bind(now)
    .bind(signature)
    .bind(carries)
    .bind(head)
    .bind(covers_pos)
    .bind(created_pos)
    .execute(&mut **tx)
    .await;
    if let Err(sqlx::Error::Database(e)) = &r
        && e.is_unique_violation()
    {
        return Err(conflict("snapshot id already exists"));
    }
    r?;
    if let Some(prev) = prev
        && covers_seq > prev
    {
        sqlx::query("DELETE FROM updates WHERE vault_id = ? AND object_id = ? AND seq <= ?")
            .bind(vault_id)
            .bind(object_id)
            .bind(prev)
            .execute(&mut **tx)
            .await?;
    }
    // Revision history retention: keep the newest N snapshots.
    sqlx::query(
        "DELETE FROM snapshots WHERE vault_id = ? AND object_id = ? AND id NOT IN (
            SELECT id FROM snapshots WHERE vault_id = ? AND object_id = ?
            ORDER BY covers_seq DESC, created_at DESC LIMIT ?)
          AND id NOT IN (
            SELECT id FROM snapshots WHERE vault_id = ? AND object_id = ? AND carries = 1
            ORDER BY created_head DESC, created_at DESC LIMIT 1)",
    )
    .bind(vault_id)
    .bind(object_id)
    .bind(vault_id)
    .bind(object_id)
    .bind(retention.max(2))
    .bind(vault_id)
    .bind(object_id)
    .execute(&mut **tx)
    .await?;
    Ok(covers_pos)
}

/// `NotFound` for an object deleted for good (its data may still be kept,
/// for an undo).
async fn not_purged(db: &Db, vault_id: &str, object_id: &str) -> AppResult<()> {
    let purged: Option<Option<i64>> =
        sqlx::query_scalar("SELECT purged_at FROM objects WHERE vault_id = ? AND id = ?")
            .bind(vault_id)
            .bind(object_id)
            .fetch_optional(db)
            .await?;
    match purged {
        Some(Some(_)) => Err(AppError::NotFound),
        _ => Ok(()),
    }
}

pub async fn list_snapshots(
    db: &Db,
    vault_id: &str,
    object_id: &str,
    doc_only: bool,
) -> AppResult<Vec<SnapshotInfo>> {
    not_purged(db, vault_id, object_id).await?;
    // For someone the note is shared with: those they may read, in positions.
    let p = match doc_only {
        true => note_positions(db, vault_id, object_id).await?,
        false => NotePositions { base: 0, head: 0 },
    };
    // Newest first. (Retention keeps the newest by `covers_seq`; what
    // people a note is shared with store covers none of the log.)
    Ok(sqlx::query(
        "SELECT id, object_id, covers_seq, covers_pos, author_id, created_at, size FROM snapshots
         WHERE vault_id = ? AND object_id = ? AND (? = 0 OR substr(payload, 2, 4) >= (SELECT ob.share_from FROM objects ob WHERE ob.vault_id = snapshots.vault_id AND ob.id = snapshots.object_id)) ORDER BY created_at DESC, covers_seq DESC",
    )
    .bind(vault_id)
    .bind(object_id)
    .bind(doc_only)
    .fetch_all(db)
    .await?
    .iter()
    .map(|r| SnapshotInfo {
        id: r.get("id"),
        object_id: r.get("object_id"),
        covers_seq: match doc_only {
            true => p.shown(r.get("covers_pos")),
            false => r.get("covers_seq"),
        },
        author_id: r.get("author_id"),
        created_at: r.get("created_at"),
        size: r.get("size"),
    })
    .collect())
}

pub async fn get_snapshot(
    db: &Db,
    vault_id: &str,
    object_id: &str,
    id: &str,
    doc_only: bool,
) -> AppResult<Snapshot> {
    not_purged(db, vault_id, object_id).await?;
    let row = sqlx::query(
        "SELECT id, object_id, covers_seq, covers_pos, author_id, payload, created_at, signature FROM snapshots
         WHERE vault_id = ? AND object_id = ? AND id = ? AND (? = 0 OR substr(payload, 2, 4) >= (SELECT ob.share_from FROM objects ob WHERE ob.vault_id = snapshots.vault_id AND ob.id = snapshots.object_id))",
    )
    .bind(vault_id)
    .bind(object_id)
    .bind(id)
    .bind(doc_only)
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(match doc_only {
        true => shown_snapshot(&row, note_positions(db, vault_id, object_id).await?),
        false => row_snapshot(&row),
    })
}

/// Delete an object for good (e.g. emptying the trash): the vault's owner
/// and admins only, so an editor can't destroy what others wrote (they move
/// things to the trash). It is gone for everyone at once — its content and
/// comments aren't served, writes to it are refused, the people it was
/// shared with lose it (for good: an undo doesn't share it again) — but the
/// server keeps its data for `purge_undo_days`, during which [`restore`]
/// undoes it; [`erase_expired`] deletes it after. Meanwhile its document key
/// still moves on with every rotation of the vault's key, so an undo never
/// brings back a key a removed member knew. Repeated while it can be undone,
/// it changes nothing.
pub async fn purge(
    state: &AppState,
    who: &Principal,
    vault_id: &str,
    object_id: &str,
) -> AppResult<i64> {
    validate_object_id(object_id)?;
    if object_id == INDEX_OBJECT_ID {
        return Err(bad("the vault index cannot be deleted"));
    }
    if comments_note(object_id).is_some() {
        return Err(bad("comments go with their note"));
    }
    access::require_admin(&state.db, vault_id, &who.user_id).await?;
    let _w = state.write_lock.lock().await;
    let now = db::now();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    // Re-checked in the transaction: the role may have changed meanwhile.
    access::require_admin(&mut *tx, vault_id, &who.user_id).await?;
    let was_full = state.instance.vault_full(&mut *tx, vault_id).await?;
    let row: Option<(Option<i64>, Option<i64>)> =
        sqlx::query_as("SELECT purged_seq, erased_at FROM objects WHERE vault_id = ? AND id = ?")
            .bind(vault_id)
            .bind(object_id)
            .fetch_optional(&mut *tx)
            .await?;
    match row {
        Some((None, None)) => {}
        Some((Some(seq), None)) => return Ok(seq),
        _ => return Err(AppError::NotFound),
    }
    let seq: i64 = sqlx::query_scalar(
        "UPDATE vaults SET head_seq = head_seq + 1, updated_at = ? WHERE id = ? RETURNING head_seq",
    )
    .bind(now)
    .bind(vault_id)
    .fetch_one(&mut *tx)
    .await?;
    let recipients: Vec<String> = sqlx::query_scalar(
        "SELECT user_id FROM object_shares WHERE vault_id = ? AND object_id = ?",
    )
    .bind(vault_id)
    .bind(object_id)
    .fetch_all(&mut *tx)
    .await?;
    // Its shares end now; members keep the note's key (wrapped under the
    // vault key, user_id '') for an undo.
    for q in [
        "DELETE FROM object_shares WHERE vault_id = ? AND object_id = ?",
        "DELETE FROM object_keys WHERE vault_id = ? AND object_id = ? AND user_id <> ''",
    ] {
        sqlx::query(q)
            .bind(vault_id)
            .bind(object_id)
            .execute(&mut *tx)
            .await?;
    }
    // They know its key, which an undo would bring back: it is rotated then.
    if !recipients.is_empty() {
        repo::shares::set_rotation_pending(&mut *tx, vault_id, object_id).await?;
    }
    // Its comments go with it (clients drop them with the note).
    let comments = comments_object(object_id);
    let undo_days = state.instance.settings().purge_undo_days;
    for o in [object_id, comments.as_str()] {
        sqlx::query(
            "UPDATE objects SET purged_at = ?, purged_seq = ?, restored_seq = NULL WHERE vault_id = ? AND id = ?",
        )
        .bind(now)
        .bind(seq)
        .bind(vault_id)
        .bind(o)
        .execute(&mut *tx)
        .await?;
        if undo_days == 0 {
            erase(&mut tx, vault_id, o, now).await?;
        }
    }
    tx.commit().await?;
    state.hub.publish(
        vault_id,
        VaultEvent::Purged {
            object_id: object_id.to_string(),
            seq,
        },
    );
    storage_changed(state, vault_id, was_full).await;
    for u in recipients {
        state.hub.control(
            &u,
            Control::RevokeObject(vault_id.to_string(), object_id.to_string()),
        );
        state.hub.control(
            &u,
            Control::User(notekpr_core::protocol::UserEventKind::SharesChanged),
        );
    }
    db::audit(
        &state.db,
        Some(&who.user_id),
        None,
        Some(vault_id),
        "object.purge",
        Some(object_id),
        serde_json::json!({}),
    )
    .await;
    Ok(seq)
}

/// Undo deleting an object for good, while the server still keeps it: it
/// is back for everyone, with its history and comments, though shared with
/// nobody. The vault's owner and admins only, as deleting it.
pub async fn restore(
    state: &AppState,
    who: &Principal,
    vault_id: &str,
    object_id: &str,
) -> AppResult<i64> {
    validate_object_id(object_id)?;
    if object_id == INDEX_OBJECT_ID || comments_note(object_id).is_some() {
        return Err(AppError::NotFound);
    }
    access::require_admin(&state.db, vault_id, &who.user_id).await?;
    let _w = state.write_lock.lock().await;
    let now = db::now();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    access::require_admin(&mut *tx, vault_id, &who.user_id).await?;
    let row: Option<(Option<i64>, Option<i64>, bool)> = sqlx::query_as(
        "SELECT purged_at, erased_at, rotation_pending FROM objects WHERE vault_id = ? AND id = ?",
    )
    .bind(vault_id)
    .bind(object_id)
    .fetch_optional(&mut *tx)
    .await?;
    let rotation_pending = match row {
        Some((Some(at), None, pending))
            if at >= now - state.instance.settings().purge_undo_ms() =>
        {
            pending
        }
        _ => return Err(AppError::NotFound),
    };
    // It counts again: in its vault's size, and a drawing among its
    // vault's drawings.
    state.instance.require_room(&mut *tx, vault_id).await?;
    state
        .instance
        .check_drawing_back(&mut tx, vault_id, object_id)
        .await?;
    let seq: i64 = sqlx::query_scalar(
        "UPDATE vaults SET head_seq = head_seq + 1, updated_at = ? WHERE id = ? RETURNING head_seq",
    )
    .bind(now)
    .bind(vault_id)
    .fetch_one(&mut *tx)
    .await?;
    let comments = comments_object(object_id);
    for o in [object_id, comments.as_str()] {
        sqlx::query(
            "UPDATE objects SET purged_at = NULL, purged_seq = NULL, restored_seq = ? WHERE vault_id = ? AND id = ?",
        )
        .bind(seq)
        .bind(vault_id)
        .bind(o)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    state.hub.publish(
        vault_id,
        VaultEvent::Restored {
            object_id: object_id.to_string(),
            seq,
        },
    );
    storage_changed(state, vault_id, false).await;
    // It was shared when deleted: the owner's apps rotate its key now.
    if rotation_pending {
        crate::routes::shares::publish(state, vault_id, object_id, ObjectEventKind::SharesChanged);
    }
    db::audit(
        &state.db,
        Some(&who.user_id),
        None,
        Some(vault_id),
        "object.restore",
        Some(object_id),
        serde_json::json!({}),
    )
    .await;
    Ok(seq)
}

/// Tell a vault's subscribers (also people a note of it is shared with)
/// when a write filled it or made room in it; `was_full`: before the write.
/// Called after the commit, under the write lock, so that these messages
/// keep the order of the writes.
pub async fn storage_changed(state: &AppState, vault_id: &str, was_full: bool) {
    match state.instance.vault_full(&state.db, vault_id).await {
        Ok(full) if full != was_full => {
            state.hub.publish(vault_id, VaultEvent::Storage { full });
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("could not tell whether a vault is full: {e}"),
    }
}

/// Measure the database ([`Instance::refresh_storage`]) and, when that
/// fills the server or makes room on it, tell every vault's subscribers.
/// Takes the write lock, so that these messages keep the order of the
/// writes. Returns the bytes in use.
///
/// [`Instance::refresh_storage`]: crate::instance::Instance::refresh_storage
pub async fn check_storage(state: &AppState) -> AppResult<u64> {
    let _w = state.write_lock.lock().await;
    let was_full = state.instance.storage_full();
    let used = state.instance.refresh_storage(&state.db).await?;
    if state.instance.storage_full() != was_full {
        server_storage_changed(state).await?;
    }
    Ok(used)
}

/// After the server's storage filled or has room again: how each vault is
/// now (full while the server is, else by its own limit). Under the write
/// lock.
pub async fn server_storage_changed(state: &AppState) -> AppResult<()> {
    for (vault_id, full) in state.instance.every_vault_full(&state.db).await? {
        state.hub.publish(&vault_id, VaultEvent::Storage { full });
    }
    Ok(())
}

/// What of a vault was deleted for good and can still be undone (notes and
/// attachments), newest first.
pub async fn purged_items(state: &AppState, vault_id: &str) -> AppResult<Vec<PurgedItem>> {
    let keep = state.instance.settings().purge_undo_ms();
    let since = db::now() - keep;
    let notes: Vec<(String, i64)> = sqlx::query_as(
        "SELECT id, purged_at FROM objects
         WHERE vault_id = ? AND purged_at >= ? AND erased_at IS NULL
           AND id <> 'index' AND substr(id, 1, 2) <> 'c_'",
    )
    .bind(vault_id)
    .bind(since)
    .fetch_all(&state.db)
    .await?;
    let files = state.blobs.purged_since(vault_id, since).await?;
    let mut out: Vec<PurgedItem> = notes
        .into_iter()
        .map(|n| (n, false))
        .chain(files.into_iter().map(|f| (f, true)))
        .map(|((id, purged_at), attachment)| PurgedItem {
            id,
            attachment,
            purged_at,
            kept_until: purged_at + keep,
        })
        .collect();
    out.sort_by(|a, b| b.purged_at.cmp(&a.purged_at).then(a.id.cmp(&b.id)));
    Ok(out)
}

/// Delete an object's data for real (it was purged): its log, history,
/// keys; the row stays, so the purge is still reported to catch-ups and
/// writes to it are refused.
async fn erase(
    tx: &mut sqlx::Transaction<'static, sqlx::Sqlite>,
    vault_id: &str,
    object_id: &str,
    now: i64,
) -> AppResult<()> {
    for q in [
        "DELETE FROM updates WHERE vault_id = ? AND object_id = ?",
        "DELETE FROM snapshots WHERE vault_id = ? AND object_id = ?",
        "DELETE FROM object_shares WHERE vault_id = ? AND object_id = ?",
        "DELETE FROM object_keys WHERE vault_id = ? AND object_id = ?",
        "DELETE FROM object_key_chain WHERE vault_id = ? AND object_id = ?",
    ] {
        sqlx::query(q)
            .bind(vault_id)
            .bind(object_id)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query(
        "UPDATE objects SET erased_at = ?, key_epoch = NULL, encrypted_meta = NULL, share_from = NULL
         WHERE vault_id = ? AND id = ?",
    )
    .bind(now)
    .bind(vault_id)
    .bind(object_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Most objects [`erase_expired`] erases in one transaction.
const ERASE_BATCH: i64 = 200;

/// Delete for real what was deleted for good longer ago than the undo
/// period: objects (in batches) and attachments. Returns how many.
pub async fn erase_expired(state: &AppState) -> AppResult<u64> {
    let now = db::now();
    let before = now - state.instance.settings().purge_undo_ms();
    let mut n = 0;
    loop {
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        let due: Vec<(String, String)> = sqlx::query_as(
            "SELECT vault_id, id FROM objects
             WHERE purged_at IS NOT NULL AND erased_at IS NULL AND purged_at < ? LIMIT ?",
        )
        .bind(before)
        .bind(ERASE_BATCH)
        .fetch_all(&mut *tx)
        .await?;
        for (v, o) in &due {
            erase(&mut tx, v, o, now).await?;
        }
        tx.commit().await?;
        n += due.len() as u64;
        if (due.len() as i64) < ERASE_BATCH {
            break;
        }
    }
    Ok(n + state.blobs.erase_purged_before(before).await?)
}
