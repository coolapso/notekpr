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
use notekpr_core::protocol::{
    ActivateDeviceRequest, DeviceEnvelope, DeviceInfo, DeviceRegistration, RenameRequest,
    UserEventKind,
};

use crate::auth::Principal;
use crate::db;
use crate::error::{AppError, AppResult, bad, conflict, forbidden};
use crate::hub::Control;
use crate::repo;
use crate::routes::me::{insert_device, notify_user};
use crate::routes::users::account_key;
use crate::state::AppState;

pub async fn list(
    State(state): State<AppState>,
    who: Principal,
) -> AppResult<Json<Vec<DeviceInfo>>> {
    who.require_human()?;
    Ok(Json(
        repo::devices::list(&state.db, &who.user_id, who.device_id.as_deref()).await?,
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
    let pending = repo::devices::count_pending(&state.db, &who.user_id).await?;
    if pending >= 10 {
        return Err(conflict(
            "too many pending devices; approve or remove some first",
        ));
    }
    {
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        insert_device(&mut tx, &who.user_id, &reg, None, db::now()).await?;
        repo::sessions::bind_device(&mut *tx, &who.session_id, &reg.device_id).await?;
        tx.commit().await?;
    }
    db::audit(
        &state.db,
        Some(&who.user_id),
        Some(&who.user_id),
        None,
        "device.register",
        Some(&reg.device_id),
        serde_json::json!({ "name": super::me::device_name(&reg.name) }),
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
    let (status, envelope) = repo::devices::envelope(&state.db, &id, &who.user_id)
        .await?
        .ok_or(AppError::DeviceRevoked)?;
    match status.as_str() {
        "active" => Ok(Json(DeviceEnvelope {
            account_envelope: envelope.ok_or_else(|| {
                AppError::Internal(anyhow::anyhow!("active device without an envelope"))
            })?,
        })),
        "pending" => Err(conflict("device is waiting for approval")),
        _ => Err(AppError::DeviceRevoked),
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
    // Checked and activated in one transaction, so a removal in between
    // can't be undone by the activation.
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let (status, dev_pub) = repo::devices::public_key(&mut *tx, &id, &who.user_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if status != "pending" {
        return Err(conflict("device is not pending"));
    }
    let dev_pub: [u8; 32] = dev_pub
        .as_slice()
        .try_into()
        .map_err(|_| bad("bad stored device key"))?;
    public
        .verify_device_approval(&who.user_id, &id, &dev_pub, &req.signature)
        .map_err(|_| forbidden("invalid device approval signature"))?;
    repo::devices::activate(&mut *tx, &id, &req.account_envelope, db::now()).await?;
    tx.commit().await?;
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
    if repo::devices::rename(&state.db, &id, &who.user_id, &name).await? == 0 {
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
    if repo::devices::revoke(&state.db, &id, &who.user_id, now).await? == 0 {
        return Err(AppError::NotFound);
    }
    let sessions = repo::sessions::revoke_for_device(&state.db, &id, now).await?;
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
