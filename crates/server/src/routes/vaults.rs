//! Vaults, membership and vault-key distribution.
//!
//! The server stores each member's HPKE-encrypted copy of the vault key and
//! enforces roles. It cannot read the keys, the vault name or any content.

use std::collections::HashSet;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use coolpsonotes_core::protocol::{
    AddMemberRequest, AuditEntry, ChainLink, ObjectChainLink, ObjectKeyEnvelope, CreateVaultRequest, GrantKeysRequest, KeyEnvelope, MemberInfo, Role,
    RotateKeysRequest, UpdateMemberRequest, UpdateVaultRequest, UserEventKind, VaultEventKind, VaultInfo,
    VaultKeysResponse,
};
use sqlx::Row;

use crate::access;
use crate::auth::Principal;
use crate::db;
use crate::error::{AppError, AppResult, bad, conflict, forbidden};
use crate::hub::{Control, VaultEvent};
use crate::routes::me::{audit_row, notify_user};
use crate::routes::users::{account_key, public_user};
use crate::state::AppState;

const MAX_META_BYTES: usize = 16 * 1024;
const MAX_ENVELOPE_BYTES: usize = 1024;

fn vault_row(r: &sqlx::sqlite::SqliteRow) -> VaultInfo {
    VaultInfo {
        id: r.get("id"),
        owner_id: r.get("owner_id"),
        role: Role::parse(r.get("role")).unwrap_or(Role::Viewer),
        encrypted_meta: r.get("encrypted_meta"),
        key_epoch: r.get::<i64, _>("key_epoch") as u32,
        head_seq: r.get("head_seq"),
        archived: r.get("archived"),
        member_count: r.get::<i64, _>("member_count") as u32,
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    }
}

pub async fn list(State(state): State<AppState>, who: Principal) -> AppResult<Json<Vec<VaultInfo>>> {
    let rows = sqlx::query(
        "SELECT v.id, v.owner_id, m.role, v.encrypted_meta, v.key_epoch, v.head_seq, v.archived,
                v.created_at, v.updated_at,
                (SELECT COUNT(*) FROM vault_members m2 WHERE m2.vault_id = v.id) AS member_count
         FROM vaults v JOIN vault_members m ON m.vault_id = v.id AND m.user_id = ?
         ORDER BY v.created_at",
    )
    .bind(&who.user_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.iter().map(vault_row).collect()))
}

async fn load(state: &AppState, who: &Principal, vault_id: &str) -> AppResult<VaultInfo> {
    let row = sqlx::query(
        "SELECT v.id, v.owner_id, m.role, v.encrypted_meta, v.key_epoch, v.head_seq, v.archived,
                v.created_at, v.updated_at,
                (SELECT COUNT(*) FROM vault_members m2 WHERE m2.vault_id = v.id) AS member_count
         FROM vaults v JOIN vault_members m ON m.vault_id = v.id AND m.user_id = ?
         WHERE v.id = ?",
    )
    .bind(&who.user_id)
    .bind(vault_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(vault_row(&row))
}

pub async fn get(State(state): State<AppState>, who: Principal, Path(v): Path<String>) -> AppResult<Json<VaultInfo>> {
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
    if env.user_id != who.user_id || env.epoch != 1 || env.envelope.is_empty() || env.envelope.len() > MAX_ENVELOPE_BYTES {
        return Err(bad("the initial key envelope must be for yourself at epoch 1"));
    }
    if account_key(&state.db, &who.user_id).await?.is_none() {
        return Err(conflict("set up account keys first"));
    }
    {
        let _w = state.write_lock.lock().await;
        let now = db::now();
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        let r = sqlx::query(
            "INSERT INTO vaults (id, owner_id, encrypted_meta, key_epoch, head_seq, created_at, updated_at)
             VALUES (?, ?, ?, 1, 0, ?, ?)",
        )
        .bind(&req.id)
        .bind(&who.user_id)
        .bind(&req.encrypted_meta)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await;
        if let Err(sqlx::Error::Database(e)) = &r
            && e.is_unique_violation() {
                return Err(conflict("vault id already exists"));
            }
        r?;
        sqlx::query("INSERT INTO vault_members (vault_id, user_id, role, added_by, added_at) VALUES (?, ?, 'owner', ?, ?)")
            .bind(&req.id)
            .bind(&who.user_id)
            .bind(&who.user_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO vault_keys (vault_id, epoch, user_id, envelope, created_by, created_at) VALUES (?, 1, ?, ?, ?, ?)",
        )
        .bind(&req.id)
        .bind(&who.user_id)
        .bind(&env.envelope)
        .bind(&who.user_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }
    db::audit(&state.db, Some(&who.user_id), None, Some(&req.id), "vault.create", None, serde_json::json!({})).await;
    notify_user(&state, &who.user_id, UserEventKind::VaultsChanged);
    Ok((StatusCode::CREATED, Json(load(&state, &who, &req.id).await?)))
}

pub async fn update(
    State(state): State<AppState>,
    who: Principal,
    Path(v): Path<String>,
    Json(req): Json<UpdateVaultRequest>,
) -> AppResult<Json<VaultInfo>> {
    access::require_owner(&state.db, &v, &who.user_id).await?;
    if let Some(meta) = &req.encrypted_meta {
        if meta.is_empty() || meta.len() > MAX_META_BYTES {
            return Err(bad("invalid vault metadata"));
        }
        sqlx::query("UPDATE vaults SET encrypted_meta = ?, updated_at = ? WHERE id = ?")
            .bind(meta)
            .bind(db::now())
            .bind(&v)
            .execute(&state.db)
            .await?;
    }
    if let Some(archived) = req.archived {
        sqlx::query("UPDATE vaults SET archived = ?, updated_at = ? WHERE id = ?")
            .bind(archived)
            .bind(db::now())
            .bind(&v)
            .execute(&state.db)
            .await?;
    }
    db::audit(&state.db, Some(&who.user_id), None, Some(&v), "vault.update", None, serde_json::json!({ "archived": req.archived, "meta": req.encrypted_meta.is_some() })).await;
    state.hub.publish(&v, VaultEvent::Event(VaultEventKind::MetaChanged));
    Ok(Json(load(&state, &who, &v).await?))
}

pub async fn delete(State(state): State<AppState>, who: Principal, Path(v): Path<String>) -> AppResult<StatusCode> {
    access::require_owner(&state.db, &v, &who.user_id).await?;
    let members: Vec<String> = sqlx::query_scalar("SELECT user_id FROM vault_members WHERE vault_id = ?")
        .bind(&v)
        .fetch_all(&state.db)
        .await?;
    let recipients: Vec<(String, String)> = sqlx::query_as("SELECT user_id, object_id FROM object_shares WHERE vault_id = ?")
        .bind(&v)
        .fetch_all(&state.db)
        .await?;
    {
        let _w = state.write_lock.lock().await;
        sqlx::query("DELETE FROM vaults WHERE id = ?").bind(&v).execute(&state.db).await?;
    }
    for (u, o) in recipients {
        state.hub.control(&u, Control::RevokeObject(v.clone(), o));
        notify_user(&state, &u, UserEventKind::SharesChanged);
    }
    state.hub.publish(&v, VaultEvent::Event(VaultEventKind::Deleted));
    for m in members {
        state.hub.control(&m, Control::RevokeVault(v.clone()));
        notify_user(&state, &m, UserEventKind::VaultsChanged);
    }
    db::audit(&state.db, Some(&who.user_id), Some(&who.user_id), Some(&v), "vault.delete", None, serde_json::json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn keys(State(state): State<AppState>, who: Principal, Path(v): Path<String>) -> AppResult<Json<VaultKeysResponse>> {
    access::member_role(&state.db, &v, &who.user_id).await?;
    let current: i64 = sqlx::query_scalar("SELECT key_epoch FROM vaults WHERE id = ?")
        .bind(&v)
        .fetch_one(&state.db)
        .await?;
    let envelopes = sqlx::query("SELECT epoch, envelope FROM vault_keys WHERE vault_id = ? AND user_id = ? ORDER BY epoch")
        .bind(&v)
        .bind(&who.user_id)
        .fetch_all(&state.db)
        .await?
        .iter()
        .map(|r| KeyEnvelope {
            user_id: who.user_id.clone(),
            epoch: r.get::<i64, _>("epoch") as u32,
            envelope: r.get("envelope"),
        })
        .collect();
    let chain = sqlx::query("SELECT epoch, link FROM vault_key_chain WHERE vault_id = ? ORDER BY epoch")
        .bind(&v)
        .fetch_all(&state.db)
        .await?
        .iter()
        .map(|r| ChainLink { epoch: r.get::<i64, _>("epoch") as u32, link: r.get("link") })
        .collect();
    // Document keys of individually shared notes, wrapped under vault keys.
    let objects = sqlx::query("SELECT object_id, epoch, envelope FROM object_keys WHERE vault_id = ? AND user_id = '' ORDER BY object_id, epoch")
        .bind(&v)
        .fetch_all(&state.db)
        .await?
        .iter()
        .map(|r| ObjectKeyEnvelope { object_id: r.get("object_id"), epoch: r.get::<i64, _>("epoch") as u32, envelope: r.get("envelope") })
        .collect();
    let object_chain = sqlx::query("SELECT object_id, epoch, link FROM object_key_chain WHERE vault_id = ? ORDER BY object_id, epoch")
        .bind(&v)
        .fetch_all(&state.db)
        .await?
        .iter()
        .map(|r| ObjectChainLink { object_id: r.get("object_id"), epoch: r.get::<i64, _>("epoch") as u32, link: r.get("link") })
        .collect();
    Ok(Json(VaultKeysResponse { current_epoch: current as u32, envelopes, chain, objects, object_chain }))
}

pub async fn members(State(state): State<AppState>, who: Principal, Path(v): Path<String>) -> AppResult<Json<Vec<MemberInfo>>> {
    access::member_role(&state.db, &v, &who.user_id).await?;
    let rows = sqlx::query(
        "SELECT m.user_id, m.role, m.added_at, m.added_by,
                EXISTS(SELECT 1 FROM vault_keys k WHERE k.vault_id = m.vault_id AND k.user_id = m.user_id
                       AND k.epoch = v.key_epoch) AS has_key
         FROM vault_members m JOIN vaults v ON v.id = m.vault_id
         WHERE m.vault_id = ? ORDER BY m.added_at",
    )
    .bind(&v)
    .fetch_all(&state.db)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let uid: String = r.get("user_id");
        out.push(MemberInfo {
            user: public_user(&state.db, &uid).await?,
            role: Role::parse(r.get("role")).unwrap_or(Role::Viewer),
            has_current_key: r.get("has_key"),
            added_at: r.get("added_at"),
            added_by: r.get("added_by"),
        });
    }
    Ok(Json(out))
}

async fn current_epoch(state: &AppState, v: &str) -> AppResult<u32> {
    let e: i64 = sqlx::query_scalar("SELECT key_epoch FROM vaults WHERE id = ?")
        .bind(v)
        .fetch_optional(&state.db)
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
    let target = sqlx::query("SELECT kind, owner_id, disabled FROM users WHERE id = ?")
        .bind(&req.user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    let kind: String = target.get("kind");
    let owner: Option<String> = target.get("owner_id");
    let disabled: bool = target.get("disabled");
    if disabled {
        return Err(AppError::NotFound);
    }
    // Agents are private to their owner; only the owner can grant them access.
    if kind == "agent" && owner.as_deref() != Some(who.user_id.as_str()) {
        return Err(AppError::NotFound);
    }
    let epoch = current_epoch(&state, &v).await?;
    if let Some(env) = &req.key_envelope
        && (env.user_id != req.user_id || env.epoch != epoch || env.envelope.is_empty() || env.envelope.len() > MAX_ENVELOPE_BYTES) {
            return Err(bad("key envelope must be for the new member at the current epoch"));
        }
    {
        let _w = state.write_lock.lock().await;
        let now = db::now();
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        let r = sqlx::query("INSERT INTO vault_members (vault_id, user_id, role, added_by, added_at) VALUES (?, ?, ?, ?, ?)")
            .bind(&v)
            .bind(&req.user_id)
            .bind(req.role.as_str())
            .bind(&who.user_id)
            .bind(now)
            .execute(&mut *tx)
            .await;
        if let Err(sqlx::Error::Database(e)) = &r
            && e.is_unique_violation() {
                return Err(conflict("already a member"));
            }
        r?;
        if let Some(env) = &req.key_envelope {
            sqlx::query(
                "INSERT OR REPLACE INTO vault_keys (vault_id, epoch, user_id, envelope, created_by, created_at) VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(&v)
            .bind(env.epoch as i64)
            .bind(&req.user_id)
            .bind(&env.envelope)
            .bind(&who.user_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
    }
    db::audit(&state.db, Some(&who.user_id), None, Some(&v), "member.add", Some(&req.user_id), serde_json::json!({ "role": req.role })).await;
    state.hub.publish(&v, VaultEvent::Event(VaultEventKind::MembersChanged));
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
    let n = sqlx::query("UPDATE vault_members SET role = ? WHERE vault_id = ? AND user_id = ?")
        .bind(req.role.as_str())
        .bind(&v)
        .bind(&u)
        .execute(&state.db)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::NotFound);
    }
    db::audit(&state.db, Some(&who.user_id), None, Some(&v), "member.role", Some(&u), serde_json::json!({ "role": req.role })).await;
    state.hub.publish(&v, VaultEvent::Event(VaultEventKind::RoleChanged));
    Ok(StatusCode::NO_CONTENT)
}

/// Remove a member (owner), or leave a vault (any non-owner member). Owners
/// should rotate the vault key afterwards so the removed member cannot read
/// future content even if they obtained ciphertext some other way.
pub async fn remove_member(
    State(state): State<AppState>,
    who: Principal,
    Path((v, u)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    let role = access::member_role(&state.db, &v, &who.user_id).await?;
    if u == who.user_id {
        if role == Role::Owner {
            return Err(bad("the owner cannot leave; delete the vault instead"));
        }
    } else if !role.can_manage() {
        return Err(forbidden("only the vault owner can remove members"));
    }
    {
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        let n = sqlx::query("DELETE FROM vault_members WHERE vault_id = ? AND user_id = ? AND role != 'owner'")
            .bind(&v)
            .bind(&u)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if n == 0 {
            return Err(AppError::NotFound);
        }
        sqlx::query("DELETE FROM vault_keys WHERE vault_id = ? AND user_id = ?")
            .bind(&v)
            .bind(&u)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    db::audit(&state.db, Some(&who.user_id), None, Some(&v), "member.remove", Some(&u), serde_json::json!({})).await;
    state.hub.control(&u, Control::RevokeVault(v.clone()));
    state.hub.publish(&v, VaultEvent::Event(VaultEventKind::MembersChanged));
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
    let epoch = current_epoch(&state, &v).await?;
    let members: HashSet<String> = sqlx::query_scalar("SELECT user_id FROM vault_members WHERE vault_id = ?")
        .bind(&v)
        .fetch_all(&state.db)
        .await?
        .into_iter()
        .collect();
    for e in &req.envelopes {
        if e.epoch != epoch || !members.contains(&e.user_id) || e.envelope.is_empty() || e.envelope.len() > MAX_ENVELOPE_BYTES {
            return Err(bad("envelopes must be for current members at the current epoch"));
        }
    }
    {
        let _w = state.write_lock.lock().await;
        let now = db::now();
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        for e in &req.envelopes {
            sqlx::query(
                "INSERT OR REPLACE INTO vault_keys (vault_id, epoch, user_id, envelope, created_by, created_at) VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(&v)
            .bind(e.epoch as i64)
            .bind(&e.user_id)
            .bind(&e.envelope)
            .bind(&who.user_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
    }
    for e in &req.envelopes {
        notify_user(&state, &e.user_id, UserEventKind::VaultsChanged);
    }
    db::audit(&state.db, Some(&who.user_id), None, Some(&v), "keys.grant", None, serde_json::json!({ "count": req.envelopes.len() })).await;
    state.hub.publish(&v, VaultEvent::Event(VaultEventKind::MembersChanged));
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
    if req.encrypted_meta.is_empty() || req.encrypted_meta.len() > MAX_META_BYTES || req.chain_link.is_empty() || req.chain_link.len() > MAX_ENVELOPE_BYTES {
        return Err(bad("invalid rotation payload"));
    }
    let _w = state.write_lock.lock().await;
    let epoch = current_epoch(&state, &v).await?;
    if req.new_epoch != epoch + 1 {
        return Err(conflict(format!("expected new epoch {}", epoch + 1)));
    }
    let need: HashSet<String> = sqlx::query_scalar(
        "SELECT m.user_id FROM vault_members m JOIN account_keys k ON k.user_id = m.user_id WHERE m.vault_id = ?",
    )
    .bind(&v)
    .fetch_all(&state.db)
    .await?
    .into_iter()
    .collect();
    let got: HashSet<String> = req.envelopes.iter().map(|e| e.user_id.clone()).collect();
    if got != need || got.len() != req.envelopes.len() {
        return Err(conflict("envelopes must cover exactly the current members with keys"));
    }
    if req.envelopes.iter().any(|e| e.epoch != req.new_epoch || e.envelope.is_empty() || e.envelope.len() > MAX_ENVELOPE_BYTES) {
        return Err(bad("invalid envelope"));
    }
    let now = db::now();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("INSERT INTO vault_key_chain (vault_id, epoch, link) VALUES (?, ?, ?)")
        .bind(&v)
        .bind(req.new_epoch as i64)
        .bind(&req.chain_link)
        .execute(&mut *tx)
        .await?;
    for e in &req.envelopes {
        sqlx::query("INSERT INTO vault_keys (vault_id, epoch, user_id, envelope, created_by, created_at) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(&v)
            .bind(e.epoch as i64)
            .bind(&e.user_id)
            .bind(&e.envelope)
            .bind(&who.user_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE vaults SET key_epoch = ?, encrypted_meta = ?, updated_at = ? WHERE id = ?")
        .bind(req.new_epoch as i64)
        .bind(&req.encrypted_meta)
        .bind(now)
        .bind(&v)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    state.hub.publish(&v, VaultEvent::Event(VaultEventKind::KeyRotated));
    drop(_w);
    db::audit(&state.db, Some(&who.user_id), None, Some(&v), "keys.rotate", None, serde_json::json!({ "epoch": req.new_epoch })).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn audit(State(state): State<AppState>, who: Principal, Path(v): Path<String>) -> AppResult<Json<Vec<AuditEntry>>> {
    access::require_owner(&state.db, &v, &who.user_id).await?;
    let rows = sqlx::query(
        "SELECT a.id, a.at, a.actor_id, u.display_name, a.action, a.target, a.details
         FROM audit_log a LEFT JOIN users u ON u.id = a.actor_id
         WHERE a.vault_id = ? ORDER BY a.id DESC LIMIT 500",
    )
    .bind(&v)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.iter().map(audit_row).collect()))
}
