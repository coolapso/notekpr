//! The per-vault encrypted log: ordered updates, compacted snapshots and purges.
//!
//! The server cannot read any payload. It only enforces who may append, keeps
//! the log ordered (one sequence counter per vault), makes retries idempotent
//! (`client_update_id`), and prunes updates that are covered by snapshots.

use std::collections::HashMap;

use coolpsonotes_core::keys::AccountPublic;
use coolpsonotes_core::memberlog;
use coolpsonotes_core::protocol::{
    ChangesResponse, CreateSnapshotRequest, INDEX_OBJECT_ID, ObjectState, PurgedObject, PushResult,
    PushUpdate, REJECT_BAD_SIGNATURE, REJECT_PURGED, REJECT_STALE_KEY, REJECT_TOO_LARGE,
    REJECT_UNSIGNED, RejectedUpdate, Snapshot, SnapshotInfo, Update,
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

// `doc_only` queries keep only payloads encrypted with the key sharing
// started with or a later one: the envelope epoch (header bytes 2..5,
// big-endian) is at least the note's `share_from`. People a single note is
// shared with get only those, never the note's history from before it was
// shared (under the vault key, or the note's earlier private keys).

/// Everything after `since`, optionally restricted to one object (for people
/// a single note is shared with). Snapshots (only when `include_snapshots`,
/// i.e. on the first page) replace the updates they cover.
pub async fn changes_since(
    db: &Db,
    vault_id: &str,
    object: Option<&str>,
    since: i64,
    limit: i64,
    include_snapshots: bool,
    doc_only: bool,
) -> AppResult<ChangesResponse> {
    let limit = limit.clamp(1, MAX_PAGE);
    let head = head_seq(db, vault_id).await?;
    let mut snapshots = Vec::new();
    let mut covered: HashMap<String, i64> = HashMap::new();
    if include_snapshots {
        let rows = sqlx::query(
            "SELECT s.id, s.object_id, s.covers_seq, s.author_id, s.payload, s.created_at, s.signature
             FROM snapshots s
             JOIN objects o ON o.vault_id = s.vault_id AND o.id = s.object_id AND o.purged_at IS NULL
             WHERE s.vault_id = ? AND s.covers_seq > ? AND (? IS NULL OR s.object_id = ?)
               AND (? = 0 OR substr(s.payload, 2, 4) >= (SELECT ob.share_from FROM objects ob WHERE ob.vault_id = s.vault_id AND ob.id = s.object_id))
               AND s.id = (SELECT s2.id FROM snapshots s2 WHERE s2.vault_id = s.vault_id AND s2.object_id = s.object_id
                             AND (? = 0 OR substr(s2.payload, 2, 4) >= (SELECT ob.share_from FROM objects ob WHERE ob.vault_id = s2.vault_id AND ob.id = s2.object_id))
                           ORDER BY s2.covers_seq DESC, s2.created_at DESC LIMIT 1)",
        )
        .bind(vault_id)
        .bind(since)
        .bind(object)
        .bind(object)
        .bind(doc_only)
        .bind(doc_only)
        .fetch_all(db)
        .await?;
        for r in &rows {
            let s = row_snapshot(r);
            covered.insert(s.object_id.clone(), s.covers_seq);
            snapshots.push(s);
        }
    }
    let rows = sqlx::query(
        "SELECT object_id, seq, author_id, payload, created_at, signature FROM updates
         WHERE vault_id = ? AND seq > ? AND (? IS NULL OR object_id = ?) AND (? = 0 OR substr(payload, 2, 4) >= (SELECT ob.share_from FROM objects ob WHERE ob.vault_id = updates.vault_id AND ob.id = updates.object_id)) ORDER BY seq LIMIT ?",
    )
    .bind(vault_id)
    .bind(since)
    .bind(object)
    .bind(object)
    .bind(doc_only)
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
           AND (? = 0 OR substr(s.payload, 2, 4) >= o.share_from)
         ORDER BY s.created_head",
    )
    .bind(vault_id)
    .bind(since)
    .bind(upper)
    .bind(object)
    .bind(object)
    .bind(doc_only)
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
    Ok(ChangesResponse {
        updates,
        snapshots,
        purged,
        head_seq: head,
        has_more,
        next_since,
    })
}

/// Current state of one object: latest snapshot plus the updates after it.
pub async fn object_state(
    db: &Db,
    vault_id: &str,
    object_id: &str,
    doc_only: bool,
) -> AppResult<ObjectState> {
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
         WHERE vault_id = ? AND object_id = ? AND (? = 0 OR substr(payload, 2, 4) >= (SELECT ob.share_from FROM objects ob WHERE ob.vault_id = snapshots.vault_id AND ob.id = snapshots.object_id)) ORDER BY covers_seq DESC, created_at DESC LIMIT 1",
    )
    .bind(vault_id)
    .bind(object_id)
    .bind(doc_only)
    .fetch_optional(db)
    .await?
    .map(|r| row_snapshot(&r));
    let after = snapshot.as_ref().map(|s| s.covers_seq).unwrap_or(0);
    let updates = sqlx::query(
        "SELECT object_id, seq, author_id, payload, created_at, signature FROM updates
         WHERE vault_id = ? AND object_id = ? AND seq > ? AND (? = 0 OR substr(payload, 2, 4) >= (SELECT ob.share_from FROM objects ob WHERE ob.vault_id = updates.vault_id AND ob.id = updates.object_id)) ORDER BY seq",
    )
    .bind(vault_id)
    .bind(object_id)
    .bind(after)
    .bind(doc_only)
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
    use coolpsonotes_core::keys::is_doc_epoch;
    let epoch =
        coolpsonotes_core::crypto::sym_epoch(payload).map_err(|_| bad("malformed ciphertext"))?;
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
/// is shared with as editor.
async fn write_access(
    db: &Db,
    member: Option<coolpsonotes_core::protocol::Role>,
    vault_id: &str,
    object_id: &str,
    user_id: &str,
) -> AppResult<()> {
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
/// object, under an outdated key) is refused alone and listed in
/// `rejected`; the others are stored. Malformed requests and missing access
/// still fail the whole push.
pub async fn push(
    state: &AppState,
    who: &Principal,
    vault_id: &str,
    updates: Vec<PushUpdate>,
    origin_conn: u64,
) -> AppResult<PushOutcome> {
    if updates.is_empty() {
        return Ok(PushOutcome {
            results: vec![],
            duplicates: vec![],
            rejected: vec![],
            head_seq: head_seq(&state.db, vault_id).await?,
        });
    }
    if updates.len() > MAX_BATCH {
        return Err(bad(format!("at most {MAX_BATCH} updates per request")));
    }
    let max = state.config.max_update_bytes() as usize;
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
            let ok = u.payload.len() <= max;
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
    let member = access::member_role_opt(&state.db, vault_id, &who.user_id).await?;
    if member.is_none() && head_seq(&state.db, vault_id).await.is_err() {
        return Err(AppError::NotFound);
    }
    let objects: std::collections::BTreeSet<&str> =
        updates.iter().map(|u| u.object_id.as_str()).collect();
    for o in objects {
        write_access(&state.db, member, vault_id, o, &who.user_id).await?;
    }
    let signer = repo::users::account_key(&state.db, &who.user_id)
        .await?
        .and_then(|k| k.verify().ok());
    let now = db::now();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let vault_epoch = vault_key_epoch(&mut *tx, vault_id).await?;
    let signed_from = repo::vaults::signed_from(&mut *tx, vault_id).await?;
    let mut results = Vec::with_capacity(updates.len());
    let mut duplicates = Vec::new();
    let mut fresh: Vec<(Update, String)> = Vec::new();
    for u in updates {
        let existing: Option<i64> = sqlx::query_scalar(
            "SELECT seq FROM updates WHERE vault_id = ? AND client_update_id = ?",
        )
        .bind(vault_id)
        .bind(&u.client_update_id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(seq) = existing {
            let r = PushResult {
                client_update_id: u.client_update_id,
                seq,
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
        if let Some((code, message)) = signature_problem(
            signed_from,
            signer.as_ref(),
            u.signature.as_deref(),
            |k, sig| memberlog::verify_update(k, vault_id, &u.object_id, &u.payload, sig),
        ) {
            refused.push(rejected(&u, code, message));
            continue;
        }
        match check_payload_key(vault_epoch, obj.and_then(|o| o.1), &u.payload) {
            Ok(()) => {}
            Err(AppError::StaleKey(m)) => {
                refused.push(rejected(&u, REJECT_STALE_KEY, &m));
                continue;
            }
            Err(e) => return Err(e),
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
                sqlx::query(
                    "INSERT INTO objects (vault_id, id, created_by, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
                )
                .bind(vault_id)
                .bind(&u.object_id)
                .bind(&who.user_id)
                .bind(now)
                .bind(now)
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
        sqlx::query(
            "INSERT INTO updates (vault_id, seq, object_id, client_update_id, author_id, device_id, payload, created_at, signature)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .execute(&mut *tx)
        .await?;
        results.push(PushResult {
            client_update_id: u.client_update_id.clone(),
            seq,
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
        ));
    }
    let head: i64 = sqlx::query_scalar("SELECT head_seq FROM vaults WHERE id = ?")
        .bind(vault_id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    metrics::counter!("coolpsonotes_updates_total").increment(fresh.len() as u64);
    for (update, client_update_id) in fresh {
        state.hub.publish(
            vault_id,
            VaultEvent::Update {
                update,
                client_update_id,
                origin_conn,
            },
        );
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
    if !db::valid_id(snapshot_id) {
        return Err(bad("invalid snapshot id"));
    }
    if payload.is_empty() {
        return Err(bad("empty payload"));
    }
    if payload.len() as u64 > state.config.max_snapshot_bytes() {
        return Err(AppError::TooLarge);
    }
    let _w = state.write_lock.lock().await;
    let member = access::member_role_opt(&state.db, vault_id, &who.user_id).await?;
    write_access(&state.db, member, vault_id, object_id, &who.user_id).await?;
    let head = head_seq(&state.db, vault_id).await?;
    if covers_seq < 1 || covers_seq > head {
        return Err(bad("covers_seq out of range"));
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
    if last.is_some_and(|t| now - t < state.config.snapshot_cooldown_secs * 1000) {
        return Err(AppError::RateLimited);
    }
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    insert_snapshot(
        &mut tx,
        &state.config,
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
            },
        );
    }
    state.hub.publish(
        vault_id,
        VaultEvent::SnapshotCreated {
            object_id: object_id.to_string(),
            snapshot_id: snapshot_id.to_string(),
            covers_seq,
        },
    );
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
/// transaction.
#[allow(clippy::too_many_arguments)]
pub async fn insert_snapshot(
    tx: &mut sqlx::Transaction<'static, sqlx::Sqlite>,
    config: &crate::config::Config,
    author_id: &str,
    vault_id: &str,
    object_id: &str,
    snapshot_id: &str,
    covers_seq: i64,
    payload: &[u8],
    signature: Option<&[u8]>,
    now: i64,
    carries: bool,
) -> AppResult<()> {
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
    let r = sqlx::query(
        "INSERT INTO snapshots (id, vault_id, object_id, covers_seq, author_id, payload, size, created_at, signature, carries, created_head)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
    .bind(config.snapshot_retention.max(2))
    .bind(vault_id)
    .bind(object_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn list_snapshots(
    db: &Db,
    vault_id: &str,
    object_id: &str,
    doc_only: bool,
) -> AppResult<Vec<SnapshotInfo>> {
    Ok(sqlx::query(
        "SELECT id, object_id, covers_seq, author_id, created_at, size FROM snapshots
         WHERE vault_id = ? AND object_id = ? AND (? = 0 OR substr(payload, 2, 4) >= (SELECT ob.share_from FROM objects ob WHERE ob.vault_id = snapshots.vault_id AND ob.id = snapshots.object_id)) ORDER BY covers_seq DESC, created_at DESC",
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
        covers_seq: r.get("covers_seq"),
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
    sqlx::query(
        "SELECT id, object_id, covers_seq, author_id, payload, created_at, signature FROM snapshots
         WHERE vault_id = ? AND object_id = ? AND id = ? AND (? = 0 OR substr(payload, 2, 4) >= (SELECT ob.share_from FROM objects ob WHERE ob.vault_id = snapshots.vault_id AND ob.id = snapshots.object_id))",
    )
    .bind(vault_id)
    .bind(object_id)
    .bind(id)
    .bind(doc_only)
    .fetch_optional(db)
    .await?
    .map(|r| row_snapshot(&r))
    .ok_or(AppError::NotFound)
}

/// Permanently delete an object's data (e.g. emptying the trash).
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
    let role = access::member_role(&state.db, vault_id, &who.user_id).await?;
    if !role.can_write() {
        return Err(forbidden("viewers cannot modify this vault"));
    }
    let _w = state.write_lock.lock().await;
    let now = db::now();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    // Re-checked in the transaction: the role may have changed meanwhile.
    access::require_writer(&mut *tx, vault_id, &who.user_id).await?;
    let exists: Option<Option<i64>> =
        sqlx::query_scalar("SELECT purged_at FROM objects WHERE vault_id = ? AND id = ?")
            .bind(vault_id)
            .bind(object_id)
            .fetch_optional(&mut *tx)
            .await?;
    if !matches!(exists, Some(None)) {
        return Err(AppError::NotFound);
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
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE objects SET purged_at = ?, purged_seq = ? WHERE vault_id = ? AND id = ?")
        .bind(now)
        .bind(seq)
        .bind(vault_id)
        .bind(object_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    state.hub.publish(
        vault_id,
        VaultEvent::Purged {
            object_id: object_id.to_string(),
            seq,
        },
    );
    for u in recipients {
        state.hub.control(
            &u,
            Control::RevokeObject(vault_id.to_string(), object_id.to_string()),
        );
        state.hub.control(
            &u,
            Control::User(coolpsonotes_core::protocol::UserEventKind::SharesChanged),
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
