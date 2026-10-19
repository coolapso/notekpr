//! Sharing individual notes without sharing their vault.
//!
//! A shared note gets its own document key. Vault members receive it wrapped
//! under the vault key; each recipient receives it encrypted (HPKE) to their
//! account key. Recipients can only reach that one object: its updates,
//! snapshots and presence, never the vault index, other notes, attachments
//! or the vault key. Once a note is shared the server only accepts content
//! encrypted with its current document key.

use std::collections::HashSet;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use coolpsonotes_core::keys::{FIRST_DOC_EPOCH, is_doc_epoch};
use coolpsonotes_core::protocol::{
    AddObjectShareRequest, ChainLink, INDEX_OBJECT_ID, KeyEnvelope, ObjectEventKind, ObjectKeyUpdate, ObjectKeysResponse,
    ObjectMetaUpdate, ObjectShareInfo, Role, SharedNote, UpdateMemberRequest, UserEventKind,
};
use sqlx::Row;

use crate::access;
use crate::auth::Principal;
use crate::db;
use crate::error::{AppError, AppResult, bad, conflict, forbidden};
use crate::hub::{Control, VaultEvent};
use crate::routes::me::notify_user;
use crate::routes::users::public_user;
use crate::state::AppState;
use crate::sync;

const MAX_META_BYTES: usize = 16 * 1024;
const MAX_ENVELOPE_BYTES: usize = 1024;

fn epoch_of(b: &[u8]) -> AppResult<u32> {
    coolpsonotes_core::crypto::sym_epoch(b).map_err(|_| bad("malformed ciphertext"))
}

fn check_object(o: &str) -> AppResult<()> {
    sync::validate_object_id(o)?;
    if o == INDEX_OBJECT_ID {
        return Err(bad("the vault index cannot be shared"));
    }
    Ok(())
}

async fn object_key_epoch(state: &AppState, v: &str, o: &str) -> AppResult<Option<i64>> {
    let row: Option<(Option<i64>, Option<i64>)> =
        sqlx::query_as("SELECT purged_at, key_epoch FROM objects WHERE vault_id = ? AND id = ?")
            .bind(v)
            .bind(o)
            .fetch_optional(&state.db)
            .await?;
    match row {
        Some((Some(_), _)) => Err(AppError::NotFound),
        Some((None, e)) => Ok(e),
        None => Ok(None),
    }
}

fn publish(state: &AppState, v: &str, o: &str, event: ObjectEventKind) {
    state.hub.publish(v, VaultEvent::Object { object_id: o.to_string(), event });
}

/// Notes shared with the caller individually.
pub async fn shared_with_me(State(state): State<AppState>, who: Principal) -> AppResult<Json<Vec<SharedNote>>> {
    let rows = sqlx::query(
        "SELECT s.vault_id, s.object_id, s.role, s.added_by, s.added_at, o.key_epoch, o.encrypted_meta, v.owner_id
         FROM object_shares s
         JOIN objects o ON o.vault_id = s.vault_id AND o.id = s.object_id AND o.purged_at IS NULL
         JOIN vaults v ON v.id = s.vault_id
         WHERE s.user_id = ? ORDER BY s.added_at DESC",
    )
    .bind(&who.user_id)
    .fetch_all(&state.db)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let owner_id: String = r.get("owner_id");
        out.push(SharedNote {
            vault_id: r.get("vault_id"),
            object_id: r.get("object_id"),
            role: Role::parse(r.get("role")).unwrap_or(Role::Viewer),
            owner: public_user(&state.db, &owner_id).await?,
            shared_by: r.get("added_by"),
            key_epoch: r.get::<Option<i64>, _>("key_epoch").unwrap_or(0) as u32,
            encrypted_meta: r.get("encrypted_meta"),
            added_at: r.get("added_at"),
        });
    }
    Ok(Json(out))
}

/// Keys of one shared note: the caller's own envelopes and the key chain.
pub async fn object_keys(
    State(state): State<AppState>,
    who: Principal,
    Path((v, o)): Path<(String, String)>,
) -> AppResult<Json<ObjectKeysResponse>> {
    let role = access::object_role(&state.db, &v, &o, &who.user_id).await?;
    let row: Option<(Option<i64>, Option<Vec<u8>>)> =
        sqlx::query_as("SELECT key_epoch, encrypted_meta FROM objects WHERE vault_id = ? AND id = ? AND purged_at IS NULL")
            .bind(&v)
            .bind(&o)
            .fetch_optional(&state.db)
            .await?;
    let Some((Some(epoch), meta)) = row else { return Err(AppError::NotFound) };
    let envelopes = sqlx::query("SELECT epoch, envelope FROM object_keys WHERE vault_id = ? AND object_id = ? AND user_id = ? ORDER BY epoch")
        .bind(&v)
        .bind(&o)
        .bind(&who.user_id)
        .fetch_all(&state.db)
        .await?
        .iter()
        .map(|r| KeyEnvelope { user_id: who.user_id.clone(), epoch: r.get::<i64, _>("epoch") as u32, envelope: r.get("envelope") })
        .collect();
    let chain = sqlx::query("SELECT epoch, link FROM object_key_chain WHERE vault_id = ? AND object_id = ? ORDER BY epoch")
        .bind(&v)
        .bind(&o)
        .fetch_all(&state.db)
        .await?
        .iter()
        .map(|r| ChainLink { epoch: r.get::<i64, _>("epoch") as u32, link: r.get("link") })
        .collect();
    Ok(Json(ObjectKeysResponse { object_id: o, role, current_epoch: epoch as u32, envelopes, chain, encrypted_meta: meta }))
}

/// Start sharing a note (first document key + covering snapshot) or rotate its key.
pub async fn set_key(
    State(state): State<AppState>,
    who: Principal,
    Path((v, o)): Path<(String, String)>,
    Json(req): Json<ObjectKeyUpdate>,
) -> AppResult<StatusCode> {
    who.require_human()?;
    check_object(&o)?;
    access::require_owner(&state.db, &v, &who.user_id).await?;
    if !is_doc_epoch(req.epoch) {
        return Err(bad("document key epochs have the top bit set"));
    }
    if req.vault_envelope.is_empty() || req.vault_envelope.len() > MAX_ENVELOPE_BYTES {
        return Err(bad("invalid vault envelope"));
    }
    if req.encrypted_meta.is_empty() || req.encrypted_meta.len() > MAX_META_BYTES || epoch_of(&req.encrypted_meta)? != req.epoch {
        return Err(bad("metadata must be encrypted with the new document key"));
    }
    let _w = state.write_lock.lock().await;
    let vault_epoch: i64 = sqlx::query_scalar("SELECT key_epoch FROM vaults WHERE id = ?").bind(&v).fetch_one(&state.db).await?;
    if epoch_of(&req.vault_envelope)? as i64 != vault_epoch {
        return Err(AppError::StaleKey("wrap the document key with the vault's current key".into()));
    }
    let current = object_key_epoch(&state, &v, &o).await?;
    match current {
        None => {
            if req.epoch != FIRST_DOC_EPOCH || req.chain_link.is_some() {
                return Err(bad("the first document key has epoch FIRST_DOC_EPOCH and no chain link"));
            }
            let Some(snap) = &req.snapshot else { return Err(bad("sharing a note needs a snapshot under its new key")) };
            if epoch_of(&snap.payload)? != req.epoch {
                return Err(bad("the snapshot must be encrypted with the new document key"));
            }
            // Every existing update of the note must be covered, otherwise
            // recipients would receive content they cannot decrypt.
            let last: Option<i64> = sqlx::query_scalar("SELECT MAX(seq) FROM updates WHERE vault_id = ? AND object_id = ?")
                .bind(&v)
                .bind(&o)
                .fetch_one(&state.db)
                .await?;
            let head = sync::head_seq(&state.db, &v).await?;
            if snap.covers_seq < last.unwrap_or(0) || snap.covers_seq > head {
                return Err(AppError::StaleKey("the snapshot does not cover the latest changes; catch up and retry".into()));
            }
        }
        Some(cur) => {
            if req.epoch as i64 != cur + 1 {
                return Err(conflict(format!("expected document key epoch {}", cur + 1)));
            }
            let Some(link) = &req.chain_link else { return Err(bad("a chain link is required when rotating")) };
            if epoch_of(link)? != req.epoch {
                return Err(bad("invalid chain link"));
            }
        }
    }
    // Envelopes must cover exactly the current recipients that have keys.
    let need: HashSet<String> = sqlx::query_scalar(
        "SELECT s.user_id FROM object_shares s JOIN account_keys k ON k.user_id = s.user_id WHERE s.vault_id = ? AND s.object_id = ?",
    )
    .bind(&v)
    .bind(&o)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();
    let got: HashSet<String> = req.envelopes.iter().map(|e| e.user_id.clone()).collect();
    if got != need || got.len() != req.envelopes.len() {
        return Err(conflict("envelopes must cover exactly the people the note is shared with"));
    }
    if req.envelopes.iter().any(|e| e.epoch != req.epoch || e.envelope.is_empty() || e.envelope.len() > MAX_ENVELOPE_BYTES) {
        return Err(bad("invalid envelope"));
    }
    let now = db::now();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query(
        "INSERT INTO objects (vault_id, id, created_by, created_at, updated_at) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(vault_id, id) DO NOTHING",
    )
    .bind(&v)
    .bind(&o)
    .bind(&who.user_id)
    .bind(now)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO object_keys (vault_id, object_id, epoch, user_id, envelope, created_by, created_at) VALUES (?, ?, ?, '', ?, ?, ?)")
        .bind(&v)
        .bind(&o)
        .bind(req.epoch as i64)
        .bind(&req.vault_envelope)
        .bind(&who.user_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    for e in &req.envelopes {
        sqlx::query("INSERT INTO object_keys (vault_id, object_id, epoch, user_id, envelope, created_by, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
            .bind(&v)
            .bind(&o)
            .bind(req.epoch as i64)
            .bind(&e.user_id)
            .bind(&e.envelope)
            .bind(&who.user_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(link) = &req.chain_link {
        sqlx::query("INSERT INTO object_key_chain (vault_id, object_id, epoch, link) VALUES (?, ?, ?, ?)")
            .bind(&v)
            .bind(&o)
            .bind(req.epoch as i64)
            .bind(link)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE objects SET key_epoch = ?, encrypted_meta = ?, updated_at = ? WHERE vault_id = ? AND id = ?")
        .bind(req.epoch as i64)
        .bind(&req.encrypted_meta)
        .bind(now)
        .bind(&v)
        .bind(&o)
        .execute(&mut *tx)
        .await?;
    if let Some(snap) = &req.snapshot {
        if !db::valid_id(&snap.snapshot_id) || snap.payload.len() as u64 > state.config.max_snapshot_bytes() {
            return Err(bad("invalid snapshot"));
        }
        sync::insert_snapshot(&mut tx, &state.config, &who.user_id, &v, &o, &snap.snapshot_id, snap.covers_seq.max(0), &snap.payload, now)
            .await?;
    }
    tx.commit().await?;
    drop(_w);
    let first = current.is_none();
    publish(&state, &v, &o, if first { ObjectEventKind::Shared } else { ObjectEventKind::KeyRotated });
    db::audit(&state.db, Some(&who.user_id), None, Some(&v), if first { "object.share_start" } else { "object.key_rotate" }, Some(&o), serde_json::json!({ "epoch": req.epoch })).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Update the encrypted title of a shared note (after a rename).
pub async fn set_meta(
    State(state): State<AppState>,
    who: Principal,
    Path((v, o)): Path<(String, String)>,
    Json(req): Json<ObjectMetaUpdate>,
) -> AppResult<StatusCode> {
    access::require_writer(&state.db, &v, &who.user_id).await?;
    let Some(cur) = object_key_epoch(&state, &v, &o).await? else { return Err(AppError::NotFound) };
    if req.encrypted_meta.is_empty() || req.encrypted_meta.len() > MAX_META_BYTES {
        return Err(bad("invalid metadata"));
    }
    let e = epoch_of(&req.encrypted_meta)?;
    if !is_doc_epoch(e) || e as i64 != cur {
        return Err(AppError::StaleKey("encrypt the title with the note's current key".into()));
    }
    sqlx::query("UPDATE objects SET encrypted_meta = ? WHERE vault_id = ? AND id = ?")
        .bind(&req.encrypted_meta)
        .bind(&v)
        .bind(&o)
        .execute(&state.db)
        .await?;
    publish(&state, &v, &o, ObjectEventKind::MetaChanged);
    Ok(StatusCode::NO_CONTENT)
}

/// People a note is shared with (visible to vault members).
pub async fn list(
    State(state): State<AppState>,
    who: Principal,
    Path((v, o)): Path<(String, String)>,
) -> AppResult<Json<Vec<ObjectShareInfo>>> {
    access::member_role(&state.db, &v, &who.user_id).await?;
    let cur = object_key_epoch(&state, &v, &o).await?.unwrap_or(0);
    let rows = sqlx::query(
        "SELECT s.user_id, s.role, s.added_at, s.added_by,
                EXISTS(SELECT 1 FROM object_keys k WHERE k.vault_id = s.vault_id AND k.object_id = s.object_id
                       AND k.user_id = s.user_id AND k.epoch = ?) AS has_key
         FROM object_shares s WHERE s.vault_id = ? AND s.object_id = ? ORDER BY s.added_at",
    )
    .bind(cur)
    .bind(&v)
    .bind(&o)
    .fetch_all(&state.db)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let uid: String = r.get("user_id");
        out.push(ObjectShareInfo {
            user: public_user(&state.db, &uid).await?,
            role: Role::parse(r.get("role")).unwrap_or(Role::Viewer),
            added_at: r.get("added_at"),
            added_by: r.get("added_by"),
            has_current_key: r.get("has_key"),
        });
    }
    Ok(Json(out))
}

pub async fn add(
    State(state): State<AppState>,
    who: Principal,
    Path((v, o)): Path<(String, String)>,
    Json(req): Json<AddObjectShareRequest>,
) -> AppResult<(StatusCode, Json<ObjectShareInfo>)> {
    who.require_human()?;
    check_object(&o)?;
    access::require_owner(&state.db, &v, &who.user_id).await?;
    if req.role == Role::Owner {
        return Err(bad("notes can be shared as editor or viewer"));
    }
    let target = sqlx::query("SELECT kind, owner_id, disabled FROM users WHERE id = ?")
        .bind(&req.user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    let kind: String = target.get("kind");
    let owner: Option<String> = target.get("owner_id");
    let disabled: bool = target.get("disabled");
    if disabled || (kind == "agent" && owner.as_deref() != Some(who.user_id.as_str())) {
        return Err(AppError::NotFound);
    }
    if access::member_role_opt(&state.db, &v, &req.user_id).await?.is_some() {
        return Err(conflict("this person already has access through the vault"));
    }
    let _w = state.write_lock.lock().await;
    let Some(cur) = object_key_epoch(&state, &v, &o).await? else {
        return Err(conflict("start sharing the note (set its document key) first"));
    };
    let env = &req.envelope;
    if env.user_id != req.user_id || env.epoch as i64 != cur || env.envelope.is_empty() || env.envelope.len() > MAX_ENVELOPE_BYTES {
        return Err(AppError::StaleKey("the key envelope must be for this person at the note's current key epoch".into()));
    }
    let now = db::now();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let r = sqlx::query("INSERT INTO object_shares (vault_id, object_id, user_id, role, added_by, added_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(&v)
        .bind(&o)
        .bind(&req.user_id)
        .bind(req.role.as_str())
        .bind(&who.user_id)
        .bind(now)
        .execute(&mut *tx)
        .await;
    if let Err(sqlx::Error::Database(e)) = &r
        && e.is_unique_violation()
    {
        return Err(conflict("the note is already shared with this person"));
    }
    r?;
    sqlx::query(
        "INSERT OR REPLACE INTO object_keys (vault_id, object_id, epoch, user_id, envelope, created_by, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&v)
    .bind(&o)
    .bind(cur)
    .bind(&req.user_id)
    .bind(&env.envelope)
    .bind(&who.user_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    drop(_w);
    db::audit(&state.db, Some(&who.user_id), None, Some(&v), "object.share", Some(&o), serde_json::json!({ "user": req.user_id, "role": req.role })).await;
    publish(&state, &v, &o, ObjectEventKind::SharesChanged);
    notify_user(&state, &req.user_id, UserEventKind::SharesChanged);
    let info = ObjectShareInfo {
        user: public_user(&state.db, &req.user_id).await?,
        role: req.role,
        added_at: now,
        added_by: Some(who.user_id.clone()),
        has_current_key: true,
    };
    Ok((StatusCode::CREATED, Json(info)))
}

pub async fn update(
    State(state): State<AppState>,
    who: Principal,
    Path((v, o, u)): Path<(String, String, String)>,
    Json(req): Json<UpdateMemberRequest>,
) -> AppResult<StatusCode> {
    who.require_human()?;
    access::require_owner(&state.db, &v, &who.user_id).await?;
    if req.role == Role::Owner {
        return Err(bad("notes can be shared as editor or viewer"));
    }
    let n = sqlx::query("UPDATE object_shares SET role = ? WHERE vault_id = ? AND object_id = ? AND user_id = ?")
        .bind(req.role.as_str())
        .bind(&v)
        .bind(&o)
        .bind(&u)
        .execute(&state.db)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::NotFound);
    }
    db::audit(&state.db, Some(&who.user_id), None, Some(&v), "object.share_role", Some(&o), serde_json::json!({ "user": u, "role": req.role })).await;
    publish(&state, &v, &o, ObjectEventKind::SharesChanged);
    notify_user(&state, &u, UserEventKind::SharesChanged);
    Ok(StatusCode::NO_CONTENT)
}

/// Stop sharing a note with someone (owner), or remove a note shared with
/// you from your list. Owners should rotate the note's key afterwards.
pub async fn remove(
    State(state): State<AppState>,
    who: Principal,
    Path((v, o, u)): Path<(String, String, String)>,
) -> AppResult<StatusCode> {
    if u != who.user_id {
        access::require_owner(&state.db, &v, &who.user_id).await?;
    } else if access::share_role(&state.db, &v, &o, &who.user_id).await?.is_none() {
        return Err(AppError::NotFound);
    }
    {
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        let n = sqlx::query("DELETE FROM object_shares WHERE vault_id = ? AND object_id = ? AND user_id = ?")
            .bind(&v)
            .bind(&o)
            .bind(&u)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if n == 0 {
            return Err(if u == who.user_id { AppError::NotFound } else { forbidden("the note is not shared with this person") });
        }
        sqlx::query("DELETE FROM object_keys WHERE vault_id = ? AND object_id = ? AND user_id = ?")
            .bind(&v)
            .bind(&o)
            .bind(&u)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    db::audit(&state.db, Some(&who.user_id), None, Some(&v), "object.unshare", Some(&o), serde_json::json!({ "user": u })).await;
    state.hub.control(&u, Control::RevokeObject(v.clone(), o.clone()));
    publish(&state, &v, &o, ObjectEventKind::SharesChanged);
    notify_user(&state, &u, UserEventKind::SharesChanged);
    Ok(StatusCode::NO_CONTENT)
}
