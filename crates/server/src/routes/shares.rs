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
    ObjectMetaUpdate, ObjectShareInfo, RemoveMemberRequest, Role, SharedNote, SignGrantsRequest,
    UpdateMemberRequest, UserEventKind,
};
use sqlx::SqliteExecutor;

use crate::access;
use crate::auth::Principal;
use crate::db;
use crate::error::{AppError, AppResult, bad, conflict, forbidden};
use crate::hub::{Control, VaultEvent};
use crate::repo;
use crate::routes::me::notify_user;
use crate::routes::member_log::{self, Expect};
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

async fn object_key_epoch(db: impl SqliteExecutor<'_>, v: &str, o: &str) -> AppResult<Option<i64>> {
    match repo::shares::object_state(db, v, o).await? {
        Some((Some(_), _)) => Err(AppError::NotFound),
        Some((None, e)) => Ok(e),
        None => Ok(None),
    }
}

pub(crate) fn publish(state: &AppState, v: &str, o: &str, event: ObjectEventKind) {
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
    let (role, member) = access::object_access(&state.db, &v, &o, &who.user_id).await?;
    Ok(Json(
        repo::keys::document_keys(&state.db, &v, &o, &who.user_id, role, member).await?,
    ))
}

/// What a key change does (see `ObjectKeyUpdate`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum KeyChange {
    /// The note's first key, private: only vault members get it.
    Private,
    /// Sharing starts (first key, or a fresh one without a chain link):
    /// recipients get content from this epoch on.
    ShareStart,
    /// A new epoch linked to the previous one.
    Rotation,
}

/// Check one change of a note's document key and store it, inside the
/// caller's write transaction. `vault_epoch` is the vault key epoch the new
/// key must be wrapped under; `owner` whether the caller owns the vault
/// (otherwise they are a writer). Returns what kind of change it was.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_key_change(
    state: &AppState,
    tx: &mut sqlx::Transaction<'static, sqlx::Sqlite>,
    who: &Principal,
    owner: bool,
    v: &str,
    o: &str,
    req: &ObjectKeyUpdate,
    vault_epoch: i64,
    now: i64,
) -> AppResult<KeyChange> {
    check_object(o)?;
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
    if epoch_of(&req.vault_envelope)? as i64 != vault_epoch {
        return Err(AppError::StaleKey(
            "wrap the document key with the vault's current key".into(),
        ));
    }
    let current = object_key_epoch(&mut **tx, v, o).await?;
    let recipients: HashSet<String> =
        repo::shares::recipient_ids_with_account_keys(&mut **tx, v, o)
            .await?
            .into_iter()
            .collect();
    let has_recipients = !repo::shares::recipients(&mut **tx, v, o, 0)
        .await?
        .is_empty();
    let kind = match (current, &req.chain_link) {
        (None, Some(_)) => return Err(bad("the first document key has no chain link")),
        (None, None) if req.epoch != FIRST_DOC_EPOCH => {
            return Err(bad("the first document key has epoch FIRST_DOC_EPOCH"));
        }
        (None, None) if req.private => KeyChange::Private,
        (None, None) => KeyChange::ShareStart,
        (Some(cur), _) if req.epoch as i64 != cur + 1 => {
            return Err(conflict(format!("expected document key epoch {}", cur + 1)));
        }
        (Some(_), Some(link)) => {
            if epoch_of(link)? != req.epoch {
                return Err(bad("invalid chain link"));
            }
            KeyChange::Rotation
        }
        (Some(_), None) if req.private => {
            return Err(bad("a new epoch of a note's key needs a chain link"));
        }
        // A fresh key without a link to the earlier ones: sharing (re)starts.
        (Some(_), None) => {
            if has_recipients {
                return Err(conflict(
                    "the note is already shared; add people with its current key",
                ));
            }
            KeyChange::ShareStart
        }
    };
    if kind != KeyChange::Private {
        who.require_human()?;
        if !owner {
            return Err(forbidden(
                "only the vault's owner can share notes or rotate their keys",
            ));
        }
    }
    if kind != KeyChange::Rotation && !req.envelopes.is_empty() {
        return Err(bad("add people after sharing started"));
    }
    // A snapshot under the new key must cover everything stored so far
    // (recipients, or members after a private key, could not decrypt the
    // older content otherwise). A brand-new note needs none.
    let needs_snapshot = match kind {
        KeyChange::ShareStart => true,
        KeyChange::Private => repo::shares::has_content(&mut **tx, v, o).await?,
        KeyChange::Rotation => false,
    };
    if needs_snapshot {
        let Some(snap) = &req.snapshot else {
            return Err(bad("this needs a snapshot of the note under its new key"));
        };
        if epoch_of(&snap.payload)? != req.epoch {
            return Err(bad(
                "the snapshot must be encrypted with the new document key",
            ));
        }
        let last = repo::shares::last_update_seq(&mut **tx, v, o).await?;
        let newest = repo::shares::newest_snapshot_covers(&mut **tx, v, o).await?;
        // Read on another connection, but nothing can commit while this
        // transaction holds the write reservation.
        let head = sync::head_seq(&state.db, v).await?;
        if snap.covers_seq < last.unwrap_or(0).max(newest.unwrap_or(0)) || snap.covers_seq > head {
            return Err(AppError::StaleKey(
                "the snapshot does not cover the latest changes; catch up and retry".into(),
            ));
        }
    }
    // Envelopes must cover exactly the current recipients that have keys.
    let got: HashSet<String> = req.envelopes.iter().map(|e| e.user_id.clone()).collect();
    if kind == KeyChange::Rotation && (got != recipients || got.len() != req.envelopes.len()) {
        return Err(conflict(
            "envelopes must cover exactly the people the note is shared with",
        ));
    }
    if req.envelopes.iter().any(|e| {
        e.epoch != req.epoch || e.envelope.is_empty() || e.envelope.len() > MAX_ENVELOPE_BYTES
    }) {
        return Err(bad("invalid envelope"));
    }
    let epoch = req.epoch as i64;
    repo::shares::ensure_object(&mut **tx, v, o, &who.user_id, now).await?;
    repo::keys::insert_document_key(
        &mut **tx,
        v,
        o,
        epoch,
        "",
        &req.vault_envelope,
        &who.user_id,
        now,
    )
    .await?;
    for e in &req.envelopes {
        repo::keys::insert_document_key(
            &mut **tx,
            v,
            o,
            epoch,
            &e.user_id,
            &e.envelope,
            &who.user_id,
            now,
        )
        .await?;
        if let Some(sig) = &e.signature {
            repo::keys::set_grant_signature(&mut **tx, v, o, &e.user_id, epoch, sig).await?;
        }
    }
    if let Some(link) = &req.chain_link {
        repo::keys::insert_document_chain_link(&mut **tx, v, o, epoch, link).await?;
    }
    repo::shares::set_document_epoch(&mut **tx, v, o, epoch, &req.encrypted_meta, now).await?;
    if kind == KeyChange::ShareStart {
        repo::shares::set_share_from(&mut **tx, v, o, req.epoch).await?;
    }
    if let Some(snap) = &req.snapshot {
        if !db::valid_id(&snap.snapshot_id)
            || snap.payload.len() as u64 > state.config.max_snapshot_bytes()
            || epoch_of(&snap.payload)? != req.epoch
        {
            return Err(bad("invalid snapshot"));
        }
        sync::insert_snapshot(
            tx,
            &state.config,
            &who.user_id,
            v,
            o,
            &snap.snapshot_id,
            snap.covers_seq.max(0),
            &snap.payload,
            snap.signature.as_deref(),
            now,
            false,
        )
        .await?;
    }
    Ok(kind)
}

/// Give a note its first (private) key, start sharing it, or rotate its key.
/// Agents may only do the first (they write notes, they don't share them).
pub async fn set_key(
    State(state): State<AppState>,
    who: Principal,
    Path((v, o)): Path<(String, String)>,
    Json(req): Json<ObjectKeyUpdate>,
) -> AppResult<StatusCode> {
    check_object(&o)?;
    access::require_writer(&state.db, &v, &who.user_id).await?;
    let _w = state.write_lock.lock().await;
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let owner = access::require_writer(&mut *tx, &v, &who.user_id)
        .await?
        .can_manage();
    let vault_epoch = repo::vaults::key_epoch(&mut *tx, &v)
        .await?
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("vault without a key epoch")))?;
    let now = db::now();
    let kind =
        apply_key_change(&state, &mut tx, &who, owner, &v, &o, &req, vault_epoch, now).await?;
    tx.commit().await?;
    drop(_w);
    match kind {
        // Members' clients reload keys when the note's key changes.
        KeyChange::Private => publish(&state, &v, &o, ObjectEventKind::KeyRotated),
        KeyChange::ShareStart => publish(&state, &v, &o, ObjectEventKind::Shared),
        KeyChange::Rotation => publish(&state, &v, &o, ObjectEventKind::KeyRotated),
    }
    db::audit(
        &state.db,
        Some(&who.user_id),
        None,
        Some(&v),
        match kind {
            KeyChange::Private => "object.key_create",
            KeyChange::ShareStart => "object.share_start",
            KeyChange::Rotation => "object.key_rotate",
        },
        Some(&o),
        serde_json::json!({ "epoch": req.epoch }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Owner: sign the grants of keys people already hold (made before grants
/// were signed); people use only keys the owner signed giving them.
pub async fn sign_grants(
    State(state): State<AppState>,
    who: Principal,
    Path((v, o)): Path<(String, String)>,
    Json(req): Json<SignGrantsRequest>,
) -> AppResult<StatusCode> {
    who.require_human()?;
    access::require_owner(&state.db, &v, &who.user_id).await?;
    if req.grants.len() > 1000 || req.grants.iter().any(|g| g.signature.len() != 64) {
        return Err(bad("invalid grant signatures"));
    }
    let _w = state.write_lock.lock().await;
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    access::require_owner(&mut *tx, &v, &who.user_id).await?;
    for g in &req.grants {
        if !repo::keys::set_grant_signature(
            &mut *tx,
            &v,
            &o,
            &g.user_id,
            g.epoch as i64,
            &g.signature,
        )
        .await?
        {
            return Err(AppError::NotFound);
        }
    }
    tx.commit().await?;
    drop(_w);
    publish(&state, &v, &o, ObjectEventKind::SharesChanged);
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
    let Some(cur) = object_key_epoch(&state.db, &v, &o).await? else {
        return Err(AppError::NotFound);
    };
    if req.encrypted_meta.is_empty() || req.encrypted_meta.len() > MAX_META_BYTES {
        return Err(bad("invalid metadata"));
    }
    let e = epoch_of(&req.encrypted_meta)?;
    let check_epoch = |cur: i64| {
        if !is_doc_epoch(e) || e as i64 != cur {
            Err(AppError::StaleKey(
                "encrypt the title with the note's current key".into(),
            ))
        } else {
            Ok(())
        }
    };
    check_epoch(cur)?;
    {
        // Re-checked in the transaction: the writer may have been demoted,
        // or the note's key rotated, since the checks above.
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        access::require_writer(&mut *tx, &v, &who.user_id).await?;
        let Some(cur) = object_key_epoch(&mut *tx, &v, &o).await? else {
            return Err(AppError::NotFound);
        };
        check_epoch(cur)?;
        repo::shares::set_title(&mut *tx, &v, &o, &req.encrypted_meta).await?;
        tx.commit().await?;
    }
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
    let cur = object_key_epoch(&state.db, &v, &o).await?.unwrap_or(0);
    let rows = repo::shares::recipients(&state.db, &v, &o, cur).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(ObjectShareInfo {
            user: public_user(&state.db, &r.user_id).await?,
            role: r.role,
            added_at: r.added_at,
            added_by: r.added_by,
            has_current_key: r.has_current_key,
            key_signed: r.key_signed,
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
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    // Re-checked in the transaction: the person may have become a member
    // since the check above (membership supersedes shares).
    access::require_owner(&mut *tx, &v, &who.user_id).await?;
    if access::member_role_opt(&mut *tx, &v, &req.user_id)
        .await?
        .is_some()
    {
        return Err(conflict("this person already has access through the vault"));
    }
    let Some(cur) = object_key_epoch(&mut *tx, &v, &o).await? else {
        return Err(conflict(
            "start sharing the note (set its document key) first",
        ));
    };
    // A private key protects the note's history: sharing starts with a
    // fresh key first (see `apply_key_change`).
    if repo::shares::share_from(&mut *tx, &v, &o).await?.is_none() {
        return Err(conflict(
            "start sharing the note (give it a fresh key) first",
        ));
    }
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
    if let Some(sig) = &env.signature {
        repo::keys::set_grant_signature(&mut *tx, &v, &o, &req.user_id, cur, sig).await?;
    }
    member_log::check_and_append(
        &mut tx,
        &v,
        &who,
        req.log_entry.as_ref(),
        Expect::Share {
            object_id: &o,
            user_id: &req.user_id,
            role: req.role,
        },
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
        key_signed: false,
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
    {
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        access::require_owner(&mut *tx, &v, &who.user_id).await?;
        if repo::shares::set_role(&mut *tx, &v, &o, &u, req.role).await? == 0 {
            return Err(AppError::NotFound);
        }
        member_log::check_and_append(
            &mut tx,
            &v,
            &who,
            req.log_entry.as_ref(),
            Expect::Share {
                object_id: &o,
                user_id: &u,
                role: req.role,
            },
            db::now(),
        )
        .await?;
        tx.commit().await?;
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

/// The vault owner may stop sharing with anyone; a recipient may remove the
/// note from their own list.
async fn may_remove(
    db: impl SqliteExecutor<'_>,
    v: &str,
    o: &str,
    u: &str,
    caller: &str,
) -> AppResult<()> {
    if u != caller {
        access::require_owner(db, v, caller).await
    } else if access::share_role(db, v, o, caller).await?.is_none() {
        Err(AppError::NotFound)
    } else {
        Ok(())
    }
}

/// Stop sharing a note with someone (owner), or remove a note shared with
/// you from your list. Owners should rotate the note's key afterwards.
pub async fn remove(
    State(state): State<AppState>,
    who: Principal,
    Path((v, o, u)): Path<(String, String, String)>,
    req: Option<Json<RemoveMemberRequest>>,
) -> AppResult<StatusCode> {
    let req = req.map(|Json(r)| r).unwrap_or_default();
    may_remove(&state.db, &v, &o, &u, &who.user_id).await?;
    {
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        may_remove(&mut *tx, &v, &o, &u, &who.user_id).await?;
        if repo::shares::delete(&mut *tx, &v, &o, &u).await? == 0 {
            return Err(if u == who.user_id {
                AppError::NotFound
            } else {
                forbidden("the note is not shared with this person")
            });
        }
        repo::keys::delete_document_keys_of(&mut *tx, &v, &o, &u).await?;
        member_log::check_and_append(
            &mut tx,
            &v,
            &who,
            req.log_entry.as_ref(),
            Expect::Unshare {
                object_id: &o,
                user_id: &u,
            },
            db::now(),
        )
        .await?;
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
