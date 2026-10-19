//! Sharing individual notes without sharing their vault.
//!
//! A shared note gets its own document key. Vault members receive it wrapped
//! under the vault key; each recipient receives it encrypted (HPKE) to their
//! account key. Recipients can only reach that one object: its updates,
//! snapshots and presence, never the vault index, other notes, attachments
//! or the vault key. Once a note is shared the server only accepts content
//! encrypted with its current document key.

use std::collections::HashSet;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use coolpsonotes_core::keys::{FIRST_DOC_EPOCH, is_doc_epoch};
use coolpsonotes_core::protocol::{
    AddObjectShareRequest, INDEX_OBJECT_ID, ObjectEventKind, ObjectKeyUpdate, ObjectKeysResponse,
    ObjectMetaUpdate, ObjectShareInfo, Role, SharedNote, UpdateMemberRequest, UserEventKind,
};

use crate::access;
use crate::auth::Principal;
use crate::db;
use crate::error::{AppError, AppResult, bad, conflict, forbidden};
use crate::hub::{Control, VaultEvent};
use crate::repo;
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
    match repo::shares::object_state(&state.db, v, o).await? {
        Some((Some(_), _)) => Err(AppError::NotFound),
        Some((None, e)) => Ok(e),
        None => Ok(None),
    }
}

fn publish(state: &AppState, v: &str, o: &str, event: ObjectEventKind) {
    state.hub.publish(
        v,
        VaultEvent::Object {
            object_id: o.to_string(),
            event,
        },
    );
}

/// Notes shared with the caller individually.
pub async fn shared_with_me(
    State(state): State<AppState>,
    who: Principal,
) -> AppResult<Json<Vec<SharedNote>>> {
    let rows = repo::shares::shared_with(&state.db, &who.user_id).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(SharedNote {
            owner: public_user(&state.db, &r.vault_owner_id).await?,
            vault_id: r.vault_id,
            object_id: r.object_id,
            role: r.role,
            shared_by: r.added_by,
            key_epoch: r.key_epoch.unwrap_or(0) as u32,
            encrypted_meta: r.encrypted_meta,
            added_at: r.added_at,
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
    Ok(Json(
        repo::keys::document_keys(&state.db, &v, &o, &who.user_id, role).await?,
    ))
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
    if req.encrypted_meta.is_empty()
        || req.encrypted_meta.len() > MAX_META_BYTES
        || epoch_of(&req.encrypted_meta)? != req.epoch
    {
        return Err(bad("metadata must be encrypted with the new document key"));
    }
    let _w = state.write_lock.lock().await;
    let vault_epoch = repo::vaults::key_epoch(&state.db, &v)
        .await?
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("vault without a key epoch")))?;
    if epoch_of(&req.vault_envelope)? as i64 != vault_epoch {
        return Err(AppError::StaleKey(
            "wrap the document key with the vault's current key".into(),
        ));
    }
    let current = object_key_epoch(&state, &v, &o).await?;
    match current {
        None => {
            if req.epoch != FIRST_DOC_EPOCH || req.chain_link.is_some() {
                return Err(bad(
                    "the first document key has epoch FIRST_DOC_EPOCH and no chain link",
                ));
            }
            let Some(snap) = &req.snapshot else {
                return Err(bad("sharing a note needs a snapshot under its new key"));
            };
            if epoch_of(&snap.payload)? != req.epoch {
                return Err(bad(
                    "the snapshot must be encrypted with the new document key",
                ));
            }
            // Every existing update of the note must be covered, otherwise
            // recipients would receive content they cannot decrypt.
            let last = repo::shares::last_update_seq(&state.db, &v, &o).await?;
            // ...and it must be the newest snapshot of the note, or readers
            // would be handed an older one they can't use instead.
            let newest = repo::shares::newest_snapshot_covers(&state.db, &v, &o).await?;
            let head = sync::head_seq(&state.db, &v).await?;
            if snap.covers_seq < last.unwrap_or(0).max(newest.unwrap_or(0))
                || snap.covers_seq > head
            {
                return Err(AppError::StaleKey(
                    "the snapshot does not cover the latest changes; catch up and retry".into(),
                ));
            }
        }
        Some(cur) => {
            if req.epoch as i64 != cur + 1 {
                return Err(conflict(format!("expected document key epoch {}", cur + 1)));
            }
            let Some(link) = &req.chain_link else {
                return Err(bad("a chain link is required when rotating"));
            };
            if epoch_of(link)? != req.epoch {
                return Err(bad("invalid chain link"));
            }
        }
    }
    // Envelopes must cover exactly the current recipients that have keys.
    let need: HashSet<String> = repo::shares::recipient_ids_with_account_keys(&state.db, &v, &o)
        .await?
        .into_iter()
        .collect();
    let got: HashSet<String> = req.envelopes.iter().map(|e| e.user_id.clone()).collect();
    if got != need || got.len() != req.envelopes.len() {
        return Err(conflict(
            "envelopes must cover exactly the people the note is shared with",
        ));
    }
    if req.envelopes.iter().any(|e| {
        e.epoch != req.epoch || e.envelope.is_empty() || e.envelope.len() > MAX_ENVELOPE_BYTES
    }) {
        return Err(bad("invalid envelope"));
    }
    let now = db::now();
    let epoch = req.epoch as i64;
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    repo::shares::ensure_object(&mut *tx, &v, &o, &who.user_id, now).await?;
    repo::keys::insert_document_key(
        &mut *tx,
        &v,
        &o,
        epoch,
        "",
        &req.vault_envelope,
        &who.user_id,
        now,
    )
    .await?;
    for e in &req.envelopes {
        repo::keys::insert_document_key(
            &mut *tx,
            &v,
            &o,
            epoch,
            &e.user_id,
            &e.envelope,
            &who.user_id,
            now,
        )
        .await?;
    }
    if let Some(link) = &req.chain_link {
        repo::keys::insert_document_chain_link(&mut *tx, &v, &o, epoch, link).await?;
    }
    repo::shares::set_document_epoch(&mut *tx, &v, &o, epoch, &req.encrypted_meta, now).await?;
    if let Some(snap) = &req.snapshot {
        if !db::valid_id(&snap.snapshot_id)
            || snap.payload.len() as u64 > state.config.max_snapshot_bytes()
        {
            return Err(bad("invalid snapshot"));
        }
        sync::insert_snapshot(
            &mut tx,
            &state.config,
            &who.user_id,
            &v,
            &o,
            &snap.snapshot_id,
            snap.covers_seq.max(0),
            &snap.payload,
            now,
        )
        .await?;
    }
    tx.commit().await?;
    drop(_w);
    let first = current.is_none();
    publish(
        &state,
        &v,
        &o,
        if first {
            ObjectEventKind::Shared
        } else {
            ObjectEventKind::KeyRotated
        },
    );
    db::audit(
        &state.db,
        Some(&who.user_id),
        None,
        Some(&v),
        if first {
            "object.share_start"
        } else {
            "object.key_rotate"
        },
        Some(&o),
        serde_json::json!({ "epoch": req.epoch }),
    )
    .await;
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
    let Some(cur) = object_key_epoch(&state, &v, &o).await? else {
        return Err(AppError::NotFound);
    };
    if req.encrypted_meta.is_empty() || req.encrypted_meta.len() > MAX_META_BYTES {
        return Err(bad("invalid metadata"));
    }
    let e = epoch_of(&req.encrypted_meta)?;
    if !is_doc_epoch(e) || e as i64 != cur {
        return Err(AppError::StaleKey(
            "encrypt the title with the note's current key".into(),
        ));
    }
    repo::shares::set_title(&state.db, &v, &o, &req.encrypted_meta).await?;
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
    let rows = repo::shares::recipients(&state.db, &v, &o, cur).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(ObjectShareInfo {
            user: public_user(&state.db, &r.user_id).await?,
            role: r.role,
            added_at: r.added_at,
            added_by: r.added_by,
            has_current_key: r.has_current_key,
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
    let target = repo::users::principal(&state.db, &req.user_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if target.disabled
        || (target.is_agent() && target.owner_id.as_deref() != Some(who.user_id.as_str()))
    {
        return Err(AppError::NotFound);
    }
    if access::member_role_opt(&state.db, &v, &req.user_id)
        .await?
        .is_some()
    {
        return Err(conflict("this person already has access through the vault"));
    }
    let _w = state.write_lock.lock().await;
    let Some(cur) = object_key_epoch(&state, &v, &o).await? else {
        return Err(conflict(
            "start sharing the note (set its document key) first",
        ));
    };
    let env = &req.envelope;
    if env.user_id != req.user_id
        || env.epoch as i64 != cur
        || env.envelope.is_empty()
        || env.envelope.len() > MAX_ENVELOPE_BYTES
    {
        return Err(AppError::StaleKey(
            "the key envelope must be for this person at the note's current key epoch".into(),
        ));
    }
    let now = db::now();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    if !repo::shares::insert(&mut *tx, &v, &o, &req.user_id, req.role, &who.user_id, now).await? {
        return Err(conflict("the note is already shared with this person"));
    }
    repo::keys::put_document_key(
        &mut *tx,
        &v,
        &o,
        cur,
        &req.user_id,
        &env.envelope,
        &who.user_id,
        now,
    )
    .await?;
    tx.commit().await?;
    drop(_w);
    db::audit(
        &state.db,
        Some(&who.user_id),
        None,
        Some(&v),
        "object.share",
        Some(&o),
        serde_json::json!({ "user": req.user_id, "role": req.role }),
    )
    .await;
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
    if repo::shares::set_role(&state.db, &v, &o, &u, req.role).await? == 0 {
        return Err(AppError::NotFound);
    }
    db::audit(
        &state.db,
        Some(&who.user_id),
        None,
        Some(&v),
        "object.share_role",
        Some(&o),
        serde_json::json!({ "user": u, "role": req.role }),
    )
    .await;
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
    } else if access::share_role(&state.db, &v, &o, &who.user_id)
        .await?
        .is_none()
    {
        return Err(AppError::NotFound);
    }
    {
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        if repo::shares::delete(&mut *tx, &v, &o, &u).await? == 0 {
            return Err(if u == who.user_id {
                AppError::NotFound
            } else {
                forbidden("the note is not shared with this person")
            });
        }
        repo::keys::delete_document_keys_of(&mut *tx, &v, &o, &u).await?;
        tx.commit().await?;
    }
    db::audit(
        &state.db,
        Some(&who.user_id),
        None,
        Some(&v),
        "object.unshare",
        Some(&o),
        serde_json::json!({ "user": u }),
    )
    .await;
    state
        .hub
        .control(&u, Control::RevokeObject(v.clone(), o.clone()));
    publish(&state, &v, &o, ObjectEventKind::SharesChanged);
    notify_user(&state, &u, UserEventKind::SharesChanged);
    Ok(StatusCode::NO_CONTENT)
}
