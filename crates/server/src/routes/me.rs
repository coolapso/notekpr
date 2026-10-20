//! The signed-in principal: profile, account keys, recovery envelope, private prefs.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use coolpsonotes_core::keys::AccountPublic;
use coolpsonotes_core::protocol::{
    AccountSetupRequest, AuditEntry, BindDeviceRequest, MeResponse, PrefsBlob, PutPrefsRequest,
    RecoveryEnvelope, RecoveryUpdate, SessionInfo, UserEventKind,
};

use crate::auth::{self, Principal};
use crate::db;
use crate::error::{AppError, AppResult, bad, conflict, forbidden};
use crate::hub::Control;
use crate::repo;
use crate::routes::users::account_key;
use crate::state::AppState;

const MAX_PREFS_BYTES: usize = 512 * 1024;

/// Everything the caller can read, encrypted as stored, for offline
/// decryption with `coolpsonotes recover` (see docs).
pub async fn export(
    State(state): State<AppState>,
    who: Principal,
) -> AppResult<Json<coolpsonotes_core::protocol::EncryptedExport>> {
    Ok(Json(
        crate::export::user_export(&state.db, &state.blobs, &who.user_id).await?,
    ))
}

pub async fn me(State(state): State<AppState>, who: Principal) -> AppResult<Json<MeResponse>> {
    let user = auth::user_info(&state.db, &who.user_id).await?;
    let key = account_key(&state.db, &who.user_id).await?;
    let has_recovery = repo::users::has_recovery(&state.db, &who.user_id).await?;
    let expires_at = repo::sessions::expires_at(&state.db, &who.session_id).await?;
    Ok(Json(MeResponse {
        user,
        account_key: key,
        has_recovery,
        session: SessionInfo {
            id: who.session_id,
            kind: who.session_kind,
            device_id: who.device_id,
            expires_at,
        },
    }))
}

fn key32(b: &[u8], what: &str) -> AppResult<[u8; 32]> {
    b.try_into()
        .map_err(|_| bad(format!("{what} must be 32 bytes")))
}

/// One-time account key setup. Humans also register their first device and a
/// recovery envelope; nothing here is readable by the server.
pub async fn setup_account_key(
    State(state): State<AppState>,
    who: Principal,
    Json(req): Json<AccountSetupRequest>,
) -> AppResult<StatusCode> {
    if req.key.user_id != who.user_id {
        return Err(bad("key belongs to a different user"));
    }
    if req.key.key_version != 1 {
        return Err(bad("initial key_version must be 1"));
    }
    let public = req
        .key
        .verify()
        .map_err(|_| bad("invalid account key signature"))?;
    if who.is_human() {
        if req
            .recovery_envelope
            .as_ref()
            .is_none_or(|r| r.is_empty() || r.len() > 4096)
        {
            return Err(bad("a recovery envelope is required"));
        }
        let Some(dev) = &req.device else {
            return Err(bad("the first device must be registered"));
        };
        if !db::valid_id(&dev.registration.device_id) {
            return Err(bad("invalid device id"));
        }
        let dev_pub = key32(&dev.registration.enc_public, "device key")?;
        public
            .verify_device_approval(
                &who.user_id,
                &dev.registration.device_id,
                &dev_pub,
                &dev.approval_signature,
            )
            .map_err(|_| bad("invalid device approval signature"))?;
    }
    let _w = state.write_lock.lock().await;
    let now = db::now();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    if repo::users::has_account_key(&mut *tx, &who.user_id).await? {
        return Err(conflict("account keys are already set up"));
    }
    repo::users::insert_account_key(&mut *tx, &req.key, req.recovery_envelope.as_deref(), now)
        .await?;
    if let Some(dev) = &req.device
        && who.is_human()
    {
        insert_device(
            &mut tx,
            &who.user_id,
            &dev.registration,
            Some(&dev.account_envelope),
            now,
        )
        .await?;
        repo::sessions::bind_device(&mut *tx, &who.session_id, &dev.registration.device_id).await?;
    }
    tx.commit().await?;
    db::audit(
        &state.db,
        Some(&who.user_id),
        Some(&who.user_id),
        None,
        "account.keys_created",
        None,
        serde_json::json!({}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// A device name as stored and shown: trimmed, no control characters, at
/// most 100 characters.
pub fn device_name(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect()
}

pub async fn insert_device(
    tx: &mut sqlx::Transaction<'static, sqlx::Sqlite>,
    user_id: &str,
    reg: &coolpsonotes_core::protocol::DeviceRegistration,
    envelope: Option<&[u8]>,
    now: i64,
) -> AppResult<()> {
    if reg.enc_public.len() != 32 {
        return Err(bad("device key must be 32 bytes"));
    }
    let name = device_name(&reg.name);
    let name = if name.is_empty() {
        "Unnamed device".to_string()
    } else {
        name
    };
    if repo::devices::insert(
        &mut **tx,
        &reg.device_id,
        user_id,
        &name,
        &reg.enc_public,
        envelope,
        now,
    )
    .await?
    {
        Ok(())
    } else {
        Err(conflict("device id already registered"))
    }
}

pub async fn get_recovery(
    State(state): State<AppState>,
    who: Principal,
) -> AppResult<Json<RecoveryEnvelope>> {
    who.require_human()?;
    let env = repo::users::recovery_envelope(&state.db, &who.user_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(RecoveryEnvelope {
        recovery_envelope: env,
    }))
}

pub async fn put_recovery(
    State(state): State<AppState>,
    who: Principal,
    Json(req): Json<RecoveryUpdate>,
) -> AppResult<StatusCode> {
    who.require_human()?;
    if req.recovery_envelope.is_empty() || req.recovery_envelope.len() > 4096 {
        return Err(bad("invalid recovery envelope"));
    }
    let key = account_key(&state.db, &who.user_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let public: AccountPublic = key
        .verify()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("stored key invalid: {e}")))?;
    match req.version {
        Some(v) => public.verify_recovery_update_v2(
            &who.user_id,
            v,
            &req.recovery_envelope,
            &req.signature,
        ),
        None => public.verify_recovery_update(&who.user_id, &req.recovery_envelope, &req.signature),
    }
    .map_err(|_| forbidden("invalid signature"))?;
    // Versions only go up: an older signed update can't be replayed.
    if req.version.is_some_and(|v| v > i64::MAX as u64)
        || !repo::users::set_recovery_envelope(
            &state.db,
            &who.user_id,
            &req.recovery_envelope,
            req.version,
            db::now(),
        )
        .await?
    {
        return Err(conflict(
            "a newer recovery key is in place; update the app or retry",
        ));
    }
    db::audit(
        &state.db,
        Some(&who.user_id),
        Some(&who.user_id),
        None,
        "account.recovery_rotated",
        None,
        serde_json::json!({}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_prefs(
    State(state): State<AppState>,
    who: Principal,
) -> AppResult<Json<PrefsBlob>> {
    Ok(Json(
        match repo::users::prefs(&state.db, &who.user_id).await? {
            Some((ciphertext, version)) => PrefsBlob {
                ciphertext: Some(ciphertext),
                version,
            },
            None => PrefsBlob {
                ciphertext: None,
                version: 0,
            },
        },
    ))
}

/// Optimistic concurrency: `expected_version` must match the stored version.
pub async fn put_prefs(
    State(state): State<AppState>,
    who: Principal,
    Json(req): Json<PutPrefsRequest>,
) -> AppResult<Json<PrefsBlob>> {
    if req.ciphertext.len() > MAX_PREFS_BYTES {
        return Err(AppError::TooLarge);
    }
    let _w = state.write_lock.lock().await;
    let current = repo::users::prefs_version(&state.db, &who.user_id)
        .await?
        .unwrap_or(0);
    if current != req.expected_version {
        return Err(conflict("preferences were changed elsewhere"));
    }
    let version = current + 1;
    repo::users::put_prefs(&state.db, &who.user_id, &req.ciphertext, version, db::now()).await?;
    Ok(Json(PrefsBlob {
        ciphertext: Some(req.ciphertext),
        version,
    }))
}

/// Label the current session with the device it runs on (shown in device lists).
pub async fn bind_session_device(
    State(state): State<AppState>,
    who: Principal,
    Json(req): Json<BindDeviceRequest>,
) -> AppResult<StatusCode> {
    who.require_human()?;
    // One transaction: a device removed meanwhile has already had its
    // sessions revoked, so binding to it now would escape that.
    let _w = state.write_lock.lock().await;
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let owner = repo::devices::owner_unless_revoked(&mut *tx, &req.device_id).await?;
    if owner.as_deref() != Some(who.user_id.as_str()) {
        return Err(AppError::DeviceRevoked);
    }
    repo::sessions::bind_device(&mut *tx, &who.session_id, &req.device_id).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn my_audit(
    State(state): State<AppState>,
    who: Principal,
) -> AppResult<Json<Vec<AuditEntry>>> {
    Ok(Json(repo::audit::for_user(&state.db, &who.user_id).await?))
}

pub fn notify_user(state: &AppState, user_id: &str, kind: UserEventKind) {
    state.hub.control(user_id, Control::User(kind));
}
