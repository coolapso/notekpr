//! The signed-in principal: profile, account keys, recovery envelope, private prefs.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use coolpsonotes_core::keys::AccountPublic;
use coolpsonotes_core::protocol::{
    AccountSetupRequest, AuditEntry, BindDeviceRequest, MeResponse, PrefsBlob, PutPrefsRequest, RecoveryEnvelope,
    RecoveryUpdate, SessionInfo, UserEventKind,
};
use sqlx::Row;

use crate::auth::{self, Principal};
use crate::db;
use crate::error::{AppError, AppResult, bad, conflict, forbidden};
use crate::hub::Control;
use crate::routes::users::account_key;
use crate::state::AppState;

const MAX_PREFS_BYTES: usize = 512 * 1024;

pub async fn me(State(state): State<AppState>, who: Principal) -> AppResult<Json<MeResponse>> {
    let user = auth::user_info(&state.db, &who.user_id).await?;
    let key = account_key(&state.db, &who.user_id).await?;
    let has_recovery: bool =
        sqlx::query_scalar("SELECT recovery_envelope IS NOT NULL FROM account_keys WHERE user_id = ?")
            .bind(&who.user_id)
            .fetch_optional(&state.db)
            .await?
            .unwrap_or(false);
    let expires_at: i64 = sqlx::query_scalar("SELECT expires_at FROM sessions WHERE id = ?")
        .bind(&who.session_id)
        .fetch_one(&state.db)
        .await?;
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
    b.try_into().map_err(|_| bad(format!("{what} must be 32 bytes")))
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
    let public = req.key.verify().map_err(|_| bad("invalid account key signature"))?;
    if who.is_human() {
        if req.recovery_envelope.as_ref().is_none_or(|r| r.is_empty() || r.len() > 4096) {
            return Err(bad("a recovery envelope is required"));
        }
        let Some(dev) = &req.device else { return Err(bad("the first device must be registered")) };
        if !db::valid_id(&dev.registration.device_id) {
            return Err(bad("invalid device id"));
        }
        let dev_pub = key32(&dev.registration.enc_public, "device key")?;
        public
            .verify_device_approval(&who.user_id, &dev.registration.device_id, &dev_pub, &dev.approval_signature)
            .map_err(|_| bad("invalid device approval signature"))?;
    }
    let _w = state.write_lock.lock().await;
    let now = db::now();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM account_keys WHERE user_id = ?")
        .bind(&who.user_id)
        .fetch_optional(&mut *tx)
        .await?;
    if exists.is_some() {
        return Err(conflict("account keys are already set up"));
    }
    sqlx::query(
        "INSERT INTO account_keys (user_id, key_version, enc_public, sign_public, signature, recovery_envelope, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&who.user_id)
    .bind(req.key.key_version as i64)
    .bind(&req.key.enc_public)
    .bind(&req.key.sign_public)
    .bind(&req.key.signature)
    .bind(&req.recovery_envelope)
    .bind(now)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    if let Some(dev) = &req.device
        && who.is_human() {
            insert_device(&mut tx, &who.user_id, &dev.registration, Some(&dev.account_envelope), now).await?;
            sqlx::query("UPDATE sessions SET device_id = ? WHERE id = ?")
                .bind(&dev.registration.device_id)
                .bind(&who.session_id)
                .execute(&mut *tx)
                .await?;
        }
    tx.commit().await?;
    db::audit(&state.db, Some(&who.user_id), Some(&who.user_id), None, "account.keys_created", None, serde_json::json!({})).await;
    Ok(StatusCode::NO_CONTENT)
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
    let name: String = reg.name.trim().chars().filter(|c| !c.is_control()).take(100).collect();
    let status = if envelope.is_some() { "active" } else { "pending" };
    let r = sqlx::query(
        "INSERT INTO devices (id, user_id, name, enc_public, status, account_envelope, created_at, approved_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&reg.device_id)
    .bind(user_id)
    .bind(if name.is_empty() { "Unnamed device".to_string() } else { name })
    .bind(&reg.enc_public)
    .bind(status)
    .bind(envelope)
    .bind(now)
    .bind(envelope.map(|_| now))
    .execute(&mut **tx)
    .await;
    match r {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(conflict("device id already registered")),
        r => {
            r?;
            Ok(())
        }
    }
}

pub async fn get_recovery(State(state): State<AppState>, who: Principal) -> AppResult<Json<RecoveryEnvelope>> {
    who.require_human()?;
    let env: Option<Option<Vec<u8>>> =
        sqlx::query_scalar("SELECT recovery_envelope FROM account_keys WHERE user_id = ?")
            .bind(&who.user_id)
            .fetch_optional(&state.db)
            .await?;
    let env = env.flatten().ok_or(AppError::NotFound)?;
    Ok(Json(RecoveryEnvelope { recovery_envelope: env }))
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
    let key = account_key(&state.db, &who.user_id).await?.ok_or(AppError::NotFound)?;
    let public: AccountPublic = key.verify().map_err(|e| AppError::Internal(anyhow::anyhow!("stored key invalid: {e}")))?;
    public
        .verify_recovery_update(&who.user_id, &req.recovery_envelope, &req.signature)
        .map_err(|_| forbidden("invalid signature"))?;
    sqlx::query("UPDATE account_keys SET recovery_envelope = ?, updated_at = ? WHERE user_id = ?")
        .bind(&req.recovery_envelope)
        .bind(db::now())
        .bind(&who.user_id)
        .execute(&state.db)
        .await?;
    db::audit(&state.db, Some(&who.user_id), Some(&who.user_id), None, "account.recovery_rotated", None, serde_json::json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_prefs(State(state): State<AppState>, who: Principal) -> AppResult<Json<PrefsBlob>> {
    let row = sqlx::query("SELECT ciphertext, version FROM user_prefs WHERE user_id = ?")
        .bind(&who.user_id)
        .fetch_optional(&state.db)
        .await?;
    Ok(Json(match row {
        Some(r) => PrefsBlob { ciphertext: Some(r.get("ciphertext")), version: r.get("version") },
        None => PrefsBlob { ciphertext: None, version: 0 },
    }))
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
    let current: i64 = sqlx::query_scalar("SELECT version FROM user_prefs WHERE user_id = ?")
        .bind(&who.user_id)
        .fetch_optional(&state.db)
        .await?
        .unwrap_or(0);
    if current != req.expected_version {
        return Err(conflict("preferences were changed elsewhere"));
    }
    let version = current + 1;
    sqlx::query(
        "INSERT INTO user_prefs (user_id, ciphertext, version, updated_at) VALUES (?, ?, ?, ?)
         ON CONFLICT(user_id) DO UPDATE SET ciphertext = excluded.ciphertext, version = excluded.version, updated_at = excluded.updated_at",
    )
    .bind(&who.user_id)
    .bind(&req.ciphertext)
    .bind(version)
    .bind(db::now())
    .execute(&state.db)
    .await?;
    Ok(Json(PrefsBlob { ciphertext: Some(req.ciphertext), version }))
}

/// Label the current session with the device it runs on (shown in device lists).
pub async fn bind_session_device(
    State(state): State<AppState>,
    who: Principal,
    Json(req): Json<BindDeviceRequest>,
) -> AppResult<StatusCode> {
    who.require_human()?;
    let owner: Option<String> = sqlx::query_scalar("SELECT user_id FROM devices WHERE id = ? AND status != 'revoked'")
        .bind(&req.device_id)
        .fetch_optional(&state.db)
        .await?;
    if owner.as_deref() != Some(who.user_id.as_str()) {
        return Err(AppError::NotFound);
    }
    sqlx::query("UPDATE sessions SET device_id = ? WHERE id = ?")
        .bind(&req.device_id)
        .bind(&who.session_id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn my_audit(State(state): State<AppState>, who: Principal) -> AppResult<Json<Vec<AuditEntry>>> {
    let rows = sqlx::query(
        "SELECT a.id, a.at, a.actor_id, u.display_name, a.action, a.target, a.details
         FROM audit_log a LEFT JOIN users u ON u.id = a.actor_id
         WHERE a.user_id = ? ORDER BY a.id DESC LIMIT 200",
    )
    .bind(&who.user_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.iter().map(audit_row).collect()))
}

pub fn audit_row(r: &sqlx::sqlite::SqliteRow) -> AuditEntry {
    let details: String = r.get("details");
    AuditEntry {
        id: r.get("id"),
        at: r.get("at"),
        actor_id: r.get("actor_id"),
        actor_name: r.get("display_name"),
        action: r.get("action"),
        target: r.get("target"),
        details: serde_json::from_str(&details).unwrap_or(serde_json::Value::Null),
    }
}

pub fn notify_user(state: &AppState, user_id: &str, kind: UserEventKind) {
    state.hub.control(user_id, Control::User(kind));
}
