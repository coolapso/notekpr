//! Vaults, membership and vault-key distribution.
//!
//! The server stores each member's HPKE-encrypted copy of the vault key and
//! enforces roles. It cannot read the keys, the vault name or any content.

use std::collections::HashSet;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use coolpsonotes_core::protocol::{
    AddMemberRequest, AuditEntry, CreateVaultRequest, GrantKeysRequest, MemberInfo,
    ObjectEventKind, RemoveMemberRequest, Role, RotateKeysRequest, UpdateMemberRequest,
    UpdateVaultRequest, UserEventKind, VaultEventKind, VaultInfo, VaultKeysResponse,
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
use crate::routes::users::{account_key, public_user};
use crate::state::AppState;

const MAX_META_BYTES: usize = 16 * 1024;
const MAX_ENVELOPE_BYTES: usize = 1024;

pub async fn list(
    State(state): State<AppState>,
    who: Principal,
) -> AppResult<Json<Vec<VaultInfo>>> {
    Ok(Json(
        repo::vaults::list_for_member(&state.db, &who.user_id).await?,
    ))
}

async fn load(state: &AppState, who: &Principal, vault_id: &str) -> AppResult<VaultInfo> {
    repo::vaults::get_for_member(&state.db, &who.user_id, vault_id)
        .await?
        .ok_or(AppError::NotFound)
}

pub async fn get(
    State(state): State<AppState>,
    who: Principal,
    Path(v): Path<String>,
) -> AppResult<Json<VaultInfo>> {
    Ok(Json(load(&state, &who, &v).await?))
}

pub async fn create(
    State(state): State<AppState>,
    who: Principal,
    Json(req): Json<CreateVaultRequest>,
) -> AppResult<(StatusCode, Json<VaultInfo>)> {
    who.require_human()?;
    if !db::valid_id(&req.id) {
        return Err(bad("invalid vault id"));
    }
    if req.encrypted_meta.is_empty() || req.encrypted_meta.len() > MAX_META_BYTES {
        return Err(bad("invalid vault metadata"));
    }
    let env = &req.key_envelope;
    if env.user_id != who.user_id
        || env.epoch != 1
        || env.envelope.is_empty()
        || env.envelope.len() > MAX_ENVELOPE_BYTES
    {
        return Err(bad(
            "the initial key envelope must be for yourself at epoch 1",
        ));
    }
    if account_key(&state.db, &who.user_id).await?.is_none() {
        return Err(conflict("set up account keys first"));
    }
    {
        let _w = state.write_lock.lock().await;
        let now = db::now();
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        if !repo::vaults::insert(&mut *tx, &req.id, &who.user_id, &req.encrypted_meta, now).await? {
            return Err(conflict("vault id already exists"));
        }
        repo::vaults::insert_member(
            &mut *tx,
            &req.id,
            &who.user_id,
            Role::Owner,
            &who.user_id,
            now,
        )
        .await?;
        repo::keys::insert_vault_key(
            &mut *tx,
            &req.id,
            1,
            &who.user_id,
            &env.envelope,
            &who.user_id,
            now,
        )
        .await?;
        member_log::check_and_append(
            &mut tx,
            &req.id,
            &who,
            req.log_entry.as_ref(),
            Expect::Create,
            now,
        )
        .await?;
        tx.commit().await?;
    }
    db::audit(
        &state.db,
        Some(&who.user_id),
        None,
        Some(&req.id),
        "vault.create",
        None,
        serde_json::json!({}),
    )
    .await;
    notify_user(&state, &who.user_id, UserEventKind::VaultsChanged);
    Ok((
        StatusCode::CREATED,
        Json(load(&state, &who, &req.id).await?),
    ))
}

pub async fn update(
    State(state): State<AppState>,
    who: Principal,
    Path(v): Path<String>,
    Json(req): Json<UpdateVaultRequest>,
) -> AppResult<Json<VaultInfo>> {
    access::require_owner(&state.db, &v, &who.user_id).await?;
    if req
        .encrypted_meta
        .as_ref()
        .is_some_and(|meta| meta.is_empty() || meta.len() > MAX_META_BYTES)
    {
        return Err(bad("invalid vault metadata"));
    }
    {
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        access::require_owner(&mut *tx, &v, &who.user_id).await?;
        if let Some(meta) = &req.encrypted_meta {
            repo::vaults::set_meta(&mut *tx, &v, meta, db::now()).await?;
        }
        if let Some(archived) = req.archived {
            repo::vaults::set_archived(&mut *tx, &v, archived, db::now()).await?;
        }
        tx.commit().await?;
    }
    db::audit(
        &state.db,
        Some(&who.user_id),
        None,
        Some(&v),
        "vault.update",
        None,
        serde_json::json!({ "archived": req.archived, "meta": req.encrypted_meta.is_some() }),
    )
    .await;
    state
        .hub
        .publish(&v, VaultEvent::Event(VaultEventKind::MetaChanged));
    Ok(Json(load(&state, &who, &v).await?))
}

pub async fn delete(
    State(state): State<AppState>,
    who: Principal,
    Path(v): Path<String>,
) -> AppResult<StatusCode> {
    access::require_owner(&state.db, &v, &who.user_id).await?;
    // Who to notify is read in the same transaction as the delete, so
    // nobody added meanwhile keeps a live subscription.
    let (members, recipients) = {
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        access::require_owner(&mut *tx, &v, &who.user_id).await?;
        let members = repo::vaults::member_ids(&mut *tx, &v).await?;
        let recipients = repo::shares::in_vault(&mut *tx, &v).await?;
        repo::vaults::delete(&mut *tx, &v).await?;
        tx.commit().await?;
        (members, recipients)
    };
    for (u, o) in recipients {
        state.hub.control(&u, Control::RevokeObject(v.clone(), o));
        notify_user(&state, &u, UserEventKind::SharesChanged);
    }
    state
        .hub
        .publish(&v, VaultEvent::Event(VaultEventKind::Deleted));
    for m in members {
        state.hub.control(&m, Control::RevokeVault(v.clone()));
        notify_user(&state, &m, UserEventKind::VaultsChanged);
    }
    db::audit(
        &state.db,
        Some(&who.user_id),
        Some(&who.user_id),
        Some(&v),
        "vault.delete",
        None,
        serde_json::json!({}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn keys(
    State(state): State<AppState>,
    who: Principal,
    Path(v): Path<String>,
) -> AppResult<Json<VaultKeysResponse>> {
    access::member_role(&state.db, &v, &who.user_id).await?;
    Ok(Json(
        repo::keys::vault_keys(&state.db, &v, &who.user_id).await?,
    ))
}

pub async fn members(
    State(state): State<AppState>,
    who: Principal,
    Path(v): Path<String>,
) -> AppResult<Json<Vec<MemberInfo>>> {
    access::member_role(&state.db, &v, &who.user_id).await?;
    let rows = repo::vaults::members(&state.db, &v).await?;
    let mut out = Vec::with_capacity(rows.len());
    for m in rows {
        out.push(MemberInfo {
            user: public_user(&state.db, &m.user_id).await?,
            role: m.role,
            has_current_key: m.has_current_key,
            added_at: m.added_at,
            added_by: m.added_by,
        });
    }
    Ok(Json(out))
}

async fn current_epoch(db: impl SqliteExecutor<'_>, v: &str) -> AppResult<u32> {
    let e = repo::vaults::key_epoch(db, v)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(e as u32)
}

pub async fn add_member(
    State(state): State<AppState>,
    who: Principal,
    Path(v): Path<String>,
    Json(req): Json<AddMemberRequest>,
) -> AppResult<(StatusCode, Json<MemberInfo>)> {
    who.require_human()?;
    access::require_owner(&state.db, &v, &who.user_id).await?;
    if req.role == Role::Owner {
        return Err(bad("a vault has exactly one owner"));
    }
    let target = repo::users::principal(&state.db, &req.user_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if target.disabled {
        return Err(AppError::NotFound);
    }
    // Agents are private to their owner; only the owner can grant them access.
    if target.is_agent() && target.owner_id.as_deref() != Some(who.user_id.as_str()) {
        return Err(AppError::NotFound);
    }
    let check_envelope = |epoch: u32| match &req.key_envelope {
        Some(env)
            if env.user_id != req.user_id
                || env.epoch != epoch
                || env.envelope.is_empty()
                || env.envelope.len() > MAX_ENVELOPE_BYTES =>
        {
            Err(bad(
                "key envelope must be for the new member at the current epoch",
            ))
        }
        _ => Ok(()),
    };
    check_envelope(current_epoch(&state.db, &v).await?)?;
    {
        let _w = state.write_lock.lock().await;
        let now = db::now();
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        // Re-checked in the transaction: the checks above ran before the lock.
        access::require_owner(&mut *tx, &v, &who.user_id).await?;
        check_envelope(current_epoch(&mut *tx, &v).await?)?;
        if !repo::vaults::insert_member(&mut *tx, &v, &req.user_id, req.role, &who.user_id, now)
            .await?
        {
            return Err(conflict("already a member"));
        }
        if let Some(env) = &req.key_envelope {
            repo::keys::put_vault_key(
                &mut *tx,
                &v,
                env.epoch as i64,
                &req.user_id,
                &env.envelope,
                &who.user_id,
                now,
            )
            .await?;
        }
        // Individual note shares are superseded by membership: drop them, so
        // they don't silently keep access after a later removal.
        repo::shares::delete_in_vault_for(&mut *tx, &v, &req.user_id).await?;
        repo::keys::delete_vault_document_keys_of(&mut *tx, &v, &req.user_id).await?;
        member_log::check_and_append(
            &mut tx,
            &v,
            &who,
            req.log_entry.as_ref(),
            Expect::Add {
                user_id: &req.user_id,
                role: req.role,
            },
            now,
        )
        .await?;
        tx.commit().await?;
    }
    db::audit(
        &state.db,
        Some(&who.user_id),
        None,
        Some(&v),
        "member.add",
        Some(&req.user_id),
        serde_json::json!({ "role": req.role }),
    )
    .await;
    state
        .hub
        .publish(&v, VaultEvent::Event(VaultEventKind::MembersChanged));
    notify_user(&state, &req.user_id, UserEventKind::VaultsChanged);
    let info = MemberInfo {
        user: public_user(&state.db, &req.user_id).await?,
        role: req.role,
        has_current_key: req.key_envelope.is_some(),
        added_at: db::now(),
        added_by: Some(who.user_id.clone()),
    };
    Ok((StatusCode::CREATED, Json(info)))
}

pub async fn update_member(
    State(state): State<AppState>,
    who: Principal,
    Path((v, u)): Path<(String, String)>,
    Json(req): Json<UpdateMemberRequest>,
) -> AppResult<StatusCode> {
    who.require_human()?;
    access::require_owner(&state.db, &v, &who.user_id).await?;
    if u == who.user_id || req.role == Role::Owner {
        return Err(bad("the owner's role cannot be changed"));
    }
    {
        // Under the write lock like every membership change, so a push or
        // snapshot checked under it can't land after a demotion.
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        access::require_owner(&mut *tx, &v, &who.user_id).await?;
        if repo::vaults::set_member_role(&mut *tx, &v, &u, req.role).await? == 0 {
            return Err(AppError::NotFound);
        }
        member_log::check_and_append(
            &mut tx,
            &v,
            &who,
            req.log_entry.as_ref(),
            Expect::Role {
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
        "member.role",
        Some(&u),
        serde_json::json!({ "role": req.role }),
    )
    .await;
    state
        .hub
        .publish(&v, VaultEvent::Event(VaultEventKind::RoleChanged));
    Ok(StatusCode::NO_CONTENT)
}

/// Remove a member (owner), or leave a vault (any non-owner member). Owners
/// should rotate the vault key afterwards so the removed member cannot read
/// future content even if they obtained ciphertext some other way.
pub async fn remove_member(
    State(state): State<AppState>,
    who: Principal,
    Path((v, u)): Path<(String, String)>,
    req: Option<Json<RemoveMemberRequest>>,
) -> AppResult<StatusCode> {
    let req = req.map(|Json(r)| r).unwrap_or_default();
    // Owners remove others; anyone but the owner may leave.
    let may_remove = |role: Role| {
        if u == who.user_id {
            if role == Role::Owner {
                return Err(bad("the owner cannot leave; delete the vault instead"));
            }
        } else if !role.can_manage() {
            return Err(forbidden("only the vault owner can remove members"));
        }
        Ok(())
    };
    may_remove(access::member_role(&state.db, &v, &who.user_id).await?)?;
    {
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        may_remove(access::member_role(&mut *tx, &v, &who.user_id).await?)?;
        if repo::vaults::delete_member(&mut *tx, &v, &u).await? == 0 {
            return Err(AppError::NotFound);
        }
        repo::keys::delete_vault_keys_of(&mut *tx, &v, &u).await?;
        member_log::check_and_append(
            &mut tx,
            &v,
            &who,
            req.log_entry.as_ref(),
            Expect::Remove { user_id: &u },
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
        "member.remove",
        Some(&u),
        serde_json::json!({}),
    )
    .await;
    state.hub.control(&u, Control::RevokeVault(v.clone()));
    state
        .hub
        .publish(&v, VaultEvent::Event(VaultEventKind::MembersChanged));
    notify_user(&state, &u, UserEventKind::VaultsChanged);
    Ok(StatusCode::NO_CONTENT)
}

/// Fill in key envelopes for members who were added before they had keys.
pub async fn grant_keys(
    State(state): State<AppState>,
    who: Principal,
    Path(v): Path<String>,
    Json(req): Json<GrantKeysRequest>,
) -> AppResult<StatusCode> {
    who.require_human()?;
    access::require_owner(&state.db, &v, &who.user_id).await?;
    {
        // Members and epoch are read in the write transaction, so a member
        // removed (or a rotation) meanwhile can't get an envelope back.
        let _w = state.write_lock.lock().await;
        let now = db::now();
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        access::require_owner(&mut *tx, &v, &who.user_id).await?;
        let epoch = current_epoch(&mut *tx, &v).await?;
        let members: HashSet<String> = repo::vaults::member_ids(&mut *tx, &v)
            .await?
            .into_iter()
            .collect();
        for e in &req.envelopes {
            if e.epoch != epoch
                || !members.contains(&e.user_id)
                || e.envelope.is_empty()
                || e.envelope.len() > MAX_ENVELOPE_BYTES
            {
                return Err(bad(
                    "envelopes must be for current members at the current epoch",
                ));
            }
        }
        for e in &req.envelopes {
            repo::keys::put_vault_key(
                &mut *tx,
                &v,
                e.epoch as i64,
                &e.user_id,
                &e.envelope,
                &who.user_id,
                now,
            )
            .await?;
        }
        tx.commit().await?;
    }
    for e in &req.envelopes {
        notify_user(&state, &e.user_id, UserEventKind::VaultsChanged);
    }
    db::audit(
        &state.db,
        Some(&who.user_id),
        None,
        Some(&v),
        "keys.grant",
        None,
        serde_json::json!({ "count": req.envelopes.len() }),
    )
    .await;
    state
        .hub
        .publish(&v, VaultEvent::Event(VaultEventKind::MembersChanged));
    Ok(StatusCode::NO_CONTENT)
}

/// Move the vault to a new key epoch. Every current member that has account
/// keys must receive an envelope; content written afterwards uses the new key.
pub async fn rotate_keys(
    State(state): State<AppState>,
    who: Principal,
    Path(v): Path<String>,
    Json(req): Json<RotateKeysRequest>,
) -> AppResult<StatusCode> {
    who.require_human()?;
    access::require_owner(&state.db, &v, &who.user_id).await?;
    if req.encrypted_meta.is_empty()
        || req.encrypted_meta.len() > MAX_META_BYTES
        || req.chain_link.is_empty()
        || req.chain_link.len() > MAX_ENVELOPE_BYTES
    {
        return Err(bad("invalid rotation payload"));
    }
    let _w = state.write_lock.lock().await;
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    access::require_owner(&mut *tx, &v, &who.user_id).await?;
    let epoch = current_epoch(&mut *tx, &v).await?;
    if req.new_epoch != epoch + 1 {
        return Err(conflict(format!("expected new epoch {}", epoch + 1)));
    }
    let need: HashSet<String> = repo::vaults::member_ids_with_account_keys(&mut *tx, &v)
        .await?
        .into_iter()
        .collect();
    // Everyone gets the new key, except members whose key the owner left
    // out on purpose (it changed): listed, so none is forgotten by accident.
    if !super::covers_exactly(&need, &req.envelopes, &req.without) {
        return Err(conflict(
            "envelopes must cover exactly the current members with keys (or leave them out on purpose)",
        ));
    }
    if req.envelopes.iter().any(|e| {
        e.epoch != req.new_epoch || e.envelope.is_empty() || e.envelope.len() > MAX_ENVELOPE_BYTES
    }) {
        return Err(bad("invalid envelope"));
    }
    let now = db::now();
    // Every note's key moves on with the vault key: removed members knew
    // them all (R2).
    let keyed: HashSet<String> = repo::shares::keyed_in_vault(&mut *tx, &v)
        .await?
        .into_iter()
        .collect();
    let rotated: HashSet<String> = req.objects.iter().map(|c| c.object_id.clone()).collect();
    if rotated != keyed || rotated.len() != req.objects.len() {
        return Err(conflict(
            "rotate the key of every note that has one together with the vault key",
        ));
    }
    let mut shared_notes = Vec::new();
    for c in &req.objects {
        let kind = crate::routes::shares::apply_key_change(
            &state,
            &mut tx,
            &who,
            true,
            &v,
            &c.object_id,
            &c.update,
            req.new_epoch as i64,
            now,
        )
        .await?;
        if kind != crate::routes::shares::KeyChange::Rotation {
            return Err(bad("note keys rotate with a chain link"));
        }
        if !c.update.envelopes.is_empty() {
            shared_notes.push(c.object_id.clone());
        }
    }
    repo::keys::insert_vault_chain_link(&mut *tx, &v, req.new_epoch as i64, &req.chain_link)
        .await?;
    for e in &req.envelopes {
        repo::keys::insert_vault_key(
            &mut *tx,
            &v,
            e.epoch as i64,
            &e.user_id,
            &e.envelope,
            &who.user_id,
            now,
        )
        .await?;
    }
    member_log::check_and_append(
        &mut tx,
        &v,
        &who,
        req.log_entry.as_ref(),
        Expect::Rotate {
            epoch: req.new_epoch,
        },
        now,
    )
    .await?;
    repo::vaults::set_key_epoch(&mut *tx, &v, req.new_epoch as i64, &req.encrypted_meta, now)
        .await?;
    tx.commit().await?;
    state
        .hub
        .publish(&v, VaultEvent::Event(VaultEventKind::KeyRotated));
    // People single notes are shared with reload those notes' keys.
    for o in &shared_notes {
        crate::routes::shares::publish(&state, &v, o, ObjectEventKind::KeyRotated);
    }
    drop(_w);
    db::audit(
        &state.db,
        Some(&who.user_id),
        None,
        Some(&v),
        "keys.rotate",
        None,
        serde_json::json!({ "epoch": req.new_epoch }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn audit(
    State(state): State<AppState>,
    who: Principal,
    Path(v): Path<String>,
) -> AppResult<Json<Vec<AuditEntry>>> {
    access::require_owner(&state.db, &v, &who.user_id).await?;
    Ok(Json(repo::audit::for_vault(&state.db, &v).await?))
}
