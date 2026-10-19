//! Device enrollment and removal.
//!
//! A new device registers its public key as *pending*. It becomes active once
//! someone holding the account secret (an existing device, or the new device
//! itself after entering the recovery key) uploads the account secret
//! encrypted to that device key, together with an account signature over the
//! device's id and public key. The server verifies that signature, so a
//! session alone (e.g. a stolen OIDC login) cannot enroll a device.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use coolpsonotes_core::protocol::{
    ActivateDeviceRequest, DeviceEnvelope, DeviceInfo, DeviceRegistration, DeviceStatus,
    RenameRequest, UserEventKind,
};
use sqlx::Row;

use crate::auth::Principal;
use crate::db;
use crate::error::{AppError, AppResult, bad, conflict, forbidden};
use crate::hub::Control;
use crate::routes::me::{insert_device, notify_user};
use crate::routes::users::account_key;
use crate::state::AppState;

pub async fn list(
    State(state): State<AppState>,
    who: Principal,
) -> AppResult<Json<Vec<DeviceInfo>>> {
    who.require_human()?;
    let rows = sqlx::query(
        "SELECT id, name, status, enc_public, created_at, approved_at, last_seen_at FROM devices
         WHERE user_id = ? AND status != 'revoked' ORDER BY created_at",
    )
    .bind(&who.user_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        rows.iter()
            .map(|r| {
                let id: String = r.get("id");
                DeviceInfo {
                    current: who.device_id.as_deref() == Some(id.as_str()),
                    id,
                    name: r.get("name"),
                    status: DeviceStatus::parse(r.get("status")).unwrap_or(DeviceStatus::Pending),
                    enc_public: r.get("enc_public"),
                    created_at: r.get("created_at"),
                    approved_at: r.get("approved_at"),
                    last_seen_at: r.get("last_seen_at"),
                }
            })
            .collect(),
    ))
}

pub async fn register(
    State(state): State<AppState>,
    who: Principal,
    Json(reg): Json<DeviceRegistration>,
) -> AppResult<StatusCode> {
    who.require_human()?;
    if !db::valid_id(&reg.device_id) {
        return Err(bad("invalid device id"));
    }
    if account_key(&state.db, &who.user_id).await?.is_none() {
        return Err(conflict("set up account keys first"));
    }
    let pending: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM devices WHERE user_id = ? AND status = 'pending'")
            .bind(&who.user_id)
            .fetch_one(&state.db)
            .await?;
    if pending >= 10 {
        return Err(conflict(
            "too many pending devices; approve or remove some first",
        ));
    }
    {
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        insert_device(&mut tx, &who.user_id, &reg, None, db::now()).await?;
        sqlx::query("UPDATE sessions SET device_id = ? WHERE id = ?")
            .bind(&reg.device_id)
            .bind(&who.session_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    db::audit(
        &state.db,
        Some(&who.user_id),
        Some(&who.user_id),
        None,
        "device.register",
        Some(&reg.device_id),
        serde_json::json!({ "name": reg.name }),
    )
    .await;
    notify_user(&state, &who.user_id, UserEventKind::DevicePending);
    Ok(StatusCode::CREATED)
}

pub async fn envelope(
    State(state): State<AppState>,
    who: Principal,
    Path(id): Path<String>,
) -> AppResult<Json<DeviceEnvelope>> {
    who.require_human()?;
    let row =
        sqlx::query("SELECT status, account_envelope FROM devices WHERE id = ? AND user_id = ?")
            .bind(&id)
            .bind(&who.user_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound)?;
    let status: String = row.get("status");
    match status.as_str() {
        "active" => Ok(Json(DeviceEnvelope {
            account_envelope: row.get("account_envelope"),
        })),
        "pending" => Err(conflict("device is waiting for approval")),
        _ => Err(AppError::NotFound),
    }
}

pub async fn activate(
    State(state): State<AppState>,
    who: Principal,
    Path(id): Path<String>,
    Json(req): Json<ActivateDeviceRequest>,
) -> AppResult<StatusCode> {
    who.require_human()?;
    if req.account_envelope.is_empty() || req.account_envelope.len() > 4096 {
        return Err(bad("invalid envelope"));
    }
    let key = account_key(&state.db, &who.user_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let public = key
        .verify()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("stored key invalid: {e}")))?;
    let _w = state.write_lock.lock().await;
    let row = sqlx::query("SELECT status, enc_public FROM devices WHERE id = ? AND user_id = ?")
        .bind(&id)
        .bind(&who.user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    let status: String = row.get("status");
    if status != "pending" {
        return Err(conflict("device is not pending"));
    }
    let dev_pub: Vec<u8> = row.get("enc_public");
    let dev_pub: [u8; 32] = dev_pub
        .as_slice()
        .try_into()
        .map_err(|_| bad("bad stored device key"))?;
    public
        .verify_device_approval(&who.user_id, &id, &dev_pub, &req.signature)
        .map_err(|_| forbidden("invalid device approval signature"))?;
    sqlx::query(
        "UPDATE devices SET status = 'active', account_envelope = ?, approved_at = ? WHERE id = ?",
    )
    .bind(&req.account_envelope)
    .bind(db::now())
    .bind(&id)
    .execute(&state.db)
    .await?;
    drop(_w);
    db::audit(
        &state.db,
        Some(&who.user_id),
        Some(&who.user_id),
        None,
        "device.activate",
        Some(&id),
        serde_json::json!({ "by_device": who.device_id }),
    )
    .await;
    notify_user(&state, &who.user_id, UserEventKind::DevicesChanged);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn rename(
    State(state): State<AppState>,
    who: Principal,
    Path(id): Path<String>,
    Json(req): Json<RenameRequest>,
) -> AppResult<StatusCode> {
    who.require_human()?;
    let name: String = req
        .name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect();
    if name.is_empty() {
        return Err(bad("name must not be empty"));
    }
    let n = sqlx::query(
        "UPDATE devices SET name = ? WHERE id = ? AND user_id = ? AND status != 'revoked'",
    )
    .bind(&name)
    .bind(&id)
    .bind(&who.user_id)
    .execute(&state.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(AppError::NotFound);
    }
    notify_user(&state, &who.user_id, UserEventKind::DevicesChanged);
    Ok(StatusCode::NO_CONTENT)
}

/// Remove a device: deletes its key envelope and revokes all of its sessions.
/// Data the device already decrypted cannot be revoked; for a compromised
/// device, rotate the keys of shared vaults afterwards.
pub async fn revoke(
    State(state): State<AppState>,
    who: Principal,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    who.require_human()?;
    let now = db::now();
    let n = sqlx::query(
        "UPDATE devices SET status = 'revoked', account_envelope = NULL, revoked_at = ?
         WHERE id = ? AND user_id = ? AND status != 'revoked'",
    )
    .bind(now)
    .bind(&id)
    .bind(&who.user_id)
    .execute(&state.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(AppError::NotFound);
    }
    let sessions: Vec<String> =
        sqlx::query_scalar("UPDATE sessions SET revoked_at = ? WHERE device_id = ? AND revoked_at IS NULL RETURNING id")
            .bind(now)
            .bind(&id)
            .fetch_all(&state.db)
            .await?;
    for s in sessions {
        state.hub.control(&who.user_id, Control::SessionRevoked(s));
    }
    db::audit(
        &state.db,
        Some(&who.user_id),
        Some(&who.user_id),
        None,
        "device.revoke",
        Some(&id),
        serde_json::json!({}),
    )
    .await;
    notify_user(&state, &who.user_id, UserEventKind::DevicesChanged);
    Ok(StatusCode::NO_CONTENT)
}
