//! Vaults, membership and vault-key distribution.
//!
//! The server stores each member's HPKE-encrypted copy of the vault key and
//! enforces roles. It cannot read the keys, the vault name or any content.

use std::collections::HashSet;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use notekpr_core::protocol::{
    AddMemberRequest, AuditEntry, CreateVaultRequest, GrantKeysRequest, MemberInfo,
    ObjectEventKind, RemoveMemberRequest, Role, RotateKeysRequest, UpdateMemberRequest,
    UpdateVaultRequest, UserEventKind, VaultEventKind, VaultInfo, VaultKeysResponse, VaultKind,
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
/// Vault admins delete things for good: people only, never an agent.
const AGENT_NOT_ADMIN: &str = "agents can't be vault admins";
/// A role this server doesn't know: apps keep roles from newer releases
/// as written ([`Role::Other`]), but nothing may give one.
const UNKNOWN_ROLE: &str = "unknown role";

pub async fn list(
    State(state): State<AppState>,
    who: Principal,
) -> AppResult<Json<Vec<VaultInfo>>> {
    let limits = state.instance.settings();
    let mut list = repo::vaults::list_for_member(&state.db, &who.user_id, &limits).await?;
    for v in &mut list {
        with_note_limit(&state, v).await?;
        with_pending_rotations(&state, v).await?;
    }
    Ok(Json(list))
}

/// Quick notes carry how many notes they may hold (their owner's limit).
async fn with_note_limit(state: &AppState, v: &mut VaultInfo) -> AppResult<()> {
    if v.is_quick_notes() {
        v.note_limit = state
            .instance
            .quick_notes_limit(&state.db, &v.owner_id)
            .await?;
    }
    Ok(())
}

/// The owner learns which notes' keys their apps are to rotate.
async fn with_pending_rotations(state: &AppState, v: &mut VaultInfo) -> AppResult<()> {
    if v.role == Role::Owner {
        v.notes_rotation_pending = repo::shares::rotation_pending(&state.db, &v.id).await?;
    }
    Ok(())
}

async fn load(state: &AppState, who: &Principal, vault_id: &str) -> AppResult<VaultInfo> {
    let limits = state.instance.settings();
    let mut v = repo::vaults::get_for_member(&state.db, &who.user_id, vault_id, &limits)
        .await?
        .ok_or(AppError::NotFound)?;
    with_note_limit(state, &mut v).await?;
    with_pending_rotations(state, &mut v).await?;
    Ok(v)
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
        match req.kind {
            VaultKind::Regular => {
                state
                    .instance
                    .check_new_vault(&mut tx, &who.user_id)
                    .await?;
            }
            // Everyone may have quick notes (they count against no vault
            // limit), once.
            VaultKind::QuickNotes => {
                if repo::vaults::quick_notes_of(&mut *tx, &who.user_id)
                    .await?
                    .is_some()
                {
                    return Err(conflict("you have quick notes already"));
                }
            }
        }
        if !repo::vaults::insert(
            &mut *tx,
            &req.id,
            &who.user_id,
            &req.encrypted_meta,
            req.kind,
            now,
        )
        .await?
        {
            return Err(conflict("vault id already exists"));
        }
        repo::vaults::insert_member(
            &mut *tx,
            &req.id,
            &who.user_id,
            &Role::Owner,
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
        serde_json::json!({ "kind": req.kind.as_str() }),
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
    // Their name, archive flag and comments setting are fixed.
    if repo::vaults::is_quick_notes(&state.db, &v).await? {
        return Err(bad("quick notes keep their name and settings"));
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
        if let Some(allowed) = req.viewers_comment {
            repo::vaults::set_viewers_comment(&mut *tx, &v, allowed, db::now()).await?;
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
        serde_json::json!({
            "archived": req.archived,
            "meta": req.encrypted_meta.is_some(),
            "viewers_comment": req.viewers_comment,
        }),
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
    access::require_shareable(&state.db, &v).await?;
    if !req.role.is_known() {
        return Err(bad(UNKNOWN_ROLE));
    }
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
    if target.is_agent() && req.role == Role::Admin {
        return Err(bad(AGENT_NOT_ADMIN));
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
        if !repo::vaults::insert_member(&mut *tx, &v, &req.user_id, &req.role, &who.user_id, now)
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
                role: &req.role,
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
    if !req.role.is_known() {
        return Err(bad(UNKNOWN_ROLE));
    }
    if u == who.user_id || req.role == Role::Owner {
        return Err(bad("the owner's role cannot be changed"));
    }
    {
        // Under the write lock like every membership change, so a push or
        // snapshot checked under it can't land after a demotion.
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        access::require_owner(&mut *tx, &v, &who.user_id).await?;
        if repo::vaults::set_member_role(&mut *tx, &v, &u, &req.role).await? == 0 {
            return Err(AppError::NotFound);
        }
        // Only for members (anyone else is 404 above), so whether an id is
        // an agent's doesn't show.
        if req.role == Role::Admin
            && repo::users::principal(&mut *tx, &u)
                .await?
                .is_some_and(|p| p.is_agent())
        {
            return Err(bad(AGENT_NOT_ADMIN));
        }
        member_log::check_and_append(
            &mut tx,
            &v,
            &who,
            req.log_entry.as_ref(),
            Expect::Role {
                user_id: &u,
                role: &req.role,
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

/// Remove a member (owner), or leave a vault (any non-owner member). The
/// owner's removal carries the vault's new key, stored in the same
/// transaction, so nothing is ever written under a key the removed member
/// knows. Someone leaving can't renew the key: the vault is marked for its
/// owner's apps to rotate it.
pub async fn remove_member(
    State(state): State<AppState>,
    who: Principal,
    Path((v, u)): Path<(String, String)>,
    req: Option<Json<RemoveMemberRequest>>,
) -> AppResult<StatusCode> {
    let req = req.map(|Json(r)| r).unwrap_or_default();
    let leaving = u == who.user_id;
    // Owners remove others; anyone but the owner may leave.
    let may_remove = |role: Role| {
        if leaving {
            if role == Role::Owner {
                return Err(bad("the owner cannot leave; delete the vault instead"));
            }
        } else if !role.can_manage() {
            return Err(forbidden("only the vault owner can remove members"));
        }
        Ok(())
    };
    may_remove(access::member_role(&state.db, &v, &who.user_id).await?)?;
    match (&req.rotation, leaving) {
        (Some(_), true) => return Err(bad("only the vault's owner rotates its key")),
        (None, false) => {
            return Err(bad(
                "send the vault's new key with the removal (update the app)",
            ));
        }
        (Some(r), false) => {
            who.require_human()?;
            check_rotation(r)?;
        }
        (None, true) => {}
    }
    let now = db::now();
    {
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        may_remove(access::member_role(&mut *tx, &v, &who.user_id).await?)?;
        if repo::vaults::delete_member(&mut *tx, &v, &u).await? == 0 {
            return Err(AppError::NotFound);
        }
        let had_keys = repo::keys::delete_vault_keys_of(&mut *tx, &v, &u).await?;
        let was_full = state.instance.vault_full(&mut *tx, &v).await?;
        member_log::check_and_append(
            &mut tx,
            &v,
            &who,
            req.log_entry.as_ref(),
            Expect::Remove { user_id: &u },
            now,
        )
        .await?;
        let shared = match &req.rotation {
            Some(r) => Some(rotate_in(&state, &mut tx, &who, &v, r, now).await?),
            None => {
                if had_keys {
                    repo::vaults::set_rotation_pending(&mut *tx, &v).await?;
                }
                None
            }
        };
        tx.commit().await?;
        state.hub.control(&u, Control::RevokeVault(v.clone()));
        if let Some(shared) = &shared {
            published_rotation(&state, &v, shared, Some(&u));
            crate::sync::storage_changed(&state, &v, was_full).await;
        }
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
    if let Some(r) = &req.rotation {
        audit_rotation(&state, &who, &v, r.new_epoch).await;
    }
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
    check_rotation(&req)?;
    let _w = state.write_lock.lock().await;
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    access::require_owner(&mut *tx, &v, &who.user_id).await?;
    // Keys rotate in a full vault too (the notes' new snapshots may make room).
    let was_full = state.instance.vault_full(&mut *tx, &v).await?;
    let shared = rotate_in(&state, &mut tx, &who, &v, &req, db::now()).await?;
    tx.commit().await?;
    published_rotation(&state, &v, &shared, None);
    crate::sync::storage_changed(&state, &v, was_full).await;
    drop(_w);
    audit_rotation(&state, &who, &v, req.new_epoch).await;
    Ok(StatusCode::NO_CONTENT)
}

/// What a rotation request must look like before anything is locked.
fn check_rotation(req: &RotateKeysRequest) -> AppResult<()> {
    if req.encrypted_meta.is_empty()
        || req.encrypted_meta.len() > MAX_META_BYTES
        || req.chain_link.is_empty()
        || req.chain_link.len() > MAX_ENVELOPE_BYTES
    {
        return Err(bad("invalid rotation payload"));
    }
    Ok(())
}

/// Check a rotation of the vault key and store it, inside the caller's
/// write transaction (the caller checked there that `who` owns the vault).
/// Returns the notes shared individually, whose recipients reload keys.
async fn rotate_in(
    state: &AppState,
    tx: &mut sqlx::Transaction<'static, sqlx::Sqlite>,
    who: &Principal,
    v: &str,
    req: &RotateKeysRequest,
    now: i64,
) -> AppResult<Vec<String>> {
    let epoch = current_epoch(&mut **tx, v).await?;
    if req.new_epoch != epoch + 1 {
        return Err(conflict(format!("expected new epoch {}", epoch + 1)));
    }
    let need: HashSet<String> = repo::vaults::member_ids_with_account_keys(&mut **tx, v)
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
    // Every note's key moves on with the vault key: removed members knew
    // them all (R2) — also of notes deleted for good that can be undone.
    let keyed: HashSet<String> = repo::shares::keyed_in_vault(&mut **tx, v)
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
            state,
            tx,
            who,
            true,
            true,
            v,
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
    repo::keys::insert_vault_chain_link(&mut **tx, v, req.new_epoch as i64, &req.chain_link)
        .await?;
    for e in &req.envelopes {
        repo::keys::insert_vault_key(
            &mut **tx,
            v,
            e.epoch as i64,
            &e.user_id,
            &e.envelope,
            &who.user_id,
            now,
        )
        .await?;
    }
    member_log::check_and_append(
        tx,
        v,
        who,
        req.log_entry.as_ref(),
        Expect::Rotate {
            epoch: req.new_epoch,
        },
        now,
    )
    .await?;
    repo::vaults::set_key_epoch(&mut **tx, v, req.new_epoch as i64, &req.encrypted_meta, now)
        .await?;
    Ok(shared_notes)
}

/// Tell subscribers about a committed rotation (still under the write lock),
/// but not someone it removed: their subscriptions may not have ended yet.
fn published_rotation(state: &AppState, v: &str, shared_notes: &[String], removed: Option<&str>) {
    let rotated = VaultEvent::Event(VaultEventKind::KeyRotated);
    match removed {
        Some(u) => state.hub.publish_except(v, rotated, u),
        None => state.hub.publish(v, rotated),
    }
    // People single notes are shared with reload those notes' keys.
    for o in shared_notes {
        let rotated = ObjectEventKind::KeyRotated;
        match removed {
            Some(u) => crate::routes::shares::publish_except(state, v, o, rotated, u),
            None => crate::routes::shares::publish(state, v, o, rotated),
        }
    }
}

async fn audit_rotation(state: &AppState, who: &Principal, v: &str, epoch: u32) {
    db::audit(
        &state.db,
        Some(&who.user_id),
        None,
        Some(v),
        "keys.rotate",
        None,
        serde_json::json!({ "epoch": epoch }),
    )
    .await;
}

pub async fn audit(
    State(state): State<AppState>,
    who: Principal,
    Path(v): Path<String>,
) -> AppResult<Json<Vec<AuditEntry>>> {
    access::require_admin(&state.db, &v, &who.user_id).await?;
    Ok(Json(repo::audit::for_vault(&state.db, &v).await?))
}
