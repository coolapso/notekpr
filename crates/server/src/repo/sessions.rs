//! Sessions (bearer tokens, stored as SHA-256 hashes) and the short-lived
//! records of the login flows: OIDC states, one-time login codes and
//! RFC 8628 device grants.

use notekpr_core::protocol::SessionEntry;
use sqlx::{Row, SqliteExecutor};

use crate::error::AppResult;

/// A session joined with its principal, for authenticating a request.
#[derive(Debug, Clone)]
pub struct SessionAuth {
    pub id: String,
    pub user_id: String,
    pub device_id: Option<String>,
    pub kind: String,
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
    pub last_used_at: Option<i64>,
    pub user_kind: String,
    pub display_name: String,
    pub is_admin: bool,
    pub owner_id: Option<String>,
    pub disabled: bool,
    /// For agents: whether the owning person's account is disabled.
    pub owner_disabled: bool,
}

pub async fn by_token_hash(
    db: impl SqliteExecutor<'_>,
    hash: &[u8],
) -> AppResult<Option<SessionAuth>> {
    Ok(sqlx::query(
        "SELECT s.id, s.user_id, s.device_id, s.kind AS skind, s.expires_at, s.revoked_at, s.last_used_at,
                u.kind AS ukind, u.display_name, u.is_admin, u.disabled, u.owner_id,
                COALESCE(o.disabled, 0) AS owner_disabled
         FROM sessions s JOIN users u ON u.id = s.user_id
         LEFT JOIN users o ON o.id = u.owner_id
         WHERE s.token_hash = ?",
    )
    .bind(hash)
    .fetch_optional(db)
    .await?
    .map(|row| SessionAuth {
        id: row.get("id"),
        user_id: row.get("user_id"),
        device_id: row.get("device_id"),
        kind: row.get("skind"),
        expires_at: row.get("expires_at"),
        revoked_at: row.get("revoked_at"),
        last_used_at: row.get("last_used_at"),
        user_kind: row.get("ukind"),
        display_name: row.get("display_name"),
        is_admin: row.get("is_admin"),
        owner_id: row.get("owner_id"),
        disabled: row.get("disabled"),
        owner_disabled: row.get("owner_disabled"),
    }))
}

pub async fn touch(db: impl SqliteExecutor<'_>, session_id: &str, now: i64) -> AppResult<()> {
    sqlx::query("UPDATE sessions SET last_used_at = ? WHERE id = ?")
        .bind(now)
        .bind(session_id)
        .execute(db)
        .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn insert(
    db: impl SqliteExecutor<'_>,
    id: &str,
    token_hash: &[u8],
    user_id: &str,
    kind: &str,
    client_name: Option<&str>,
    now: i64,
    expires_at: i64,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO sessions (id, token_hash, user_id, kind, client_name, created_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(token_hash)
    .bind(user_id)
    .bind(kind)
    .bind(client_name)
    .bind(now)
    .bind(expires_at)
    .execute(db)
    .await?;
    Ok(())
}

/// Expiry of an existing session (errors if it doesn't exist).
pub async fn expires_at(db: impl SqliteExecutor<'_>, session_id: &str) -> AppResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT expires_at FROM sessions WHERE id = ?")
            .bind(session_id)
            .fetch_one(db)
            .await?,
    )
}

pub async fn bind_device(
    db: impl SqliteExecutor<'_>,
    session_id: &str,
    device_id: &str,
) -> AppResult<()> {
    sqlx::query("UPDATE sessions SET device_id = ? WHERE id = ?")
        .bind(device_id)
        .bind(session_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Revoke one of a user's live sessions; returns the rows affected.
pub async fn revoke(
    db: impl SqliteExecutor<'_>,
    session_id: &str,
    user_id: &str,
    now: i64,
) -> AppResult<u64> {
    Ok(sqlx::query(
        "UPDATE sessions SET revoked_at = ? WHERE id = ? AND user_id = ? AND revoked_at IS NULL",
    )
    .bind(now)
    .bind(session_id)
    .bind(user_id)
    .execute(db)
    .await?
    .rows_affected())
}

/// A user's live sessions, newest first; `current` marks the caller's.
pub async fn list_active(
    db: impl SqliteExecutor<'_>,
    user_id: &str,
    current: &str,
    now: i64,
) -> AppResult<Vec<SessionEntry>> {
    let rows = sqlx::query(
        "SELECT id, kind, client_name, device_id, created_at, last_used_at, expires_at FROM sessions
         WHERE user_id = ? AND revoked_at IS NULL AND expires_at > ? ORDER BY created_at DESC",
    )
    .bind(user_id)
    .bind(now)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let id: String = r.get("id");
            SessionEntry {
                current: id == current,
                id,
                kind: r.get("kind"),
                client_name: r.get("client_name"),
                device_id: r.get("device_id"),
                created_at: r.get("created_at"),
                last_used_at: r.get("last_used_at"),
                expires_at: r.get("expires_at"),
            }
        })
        .collect())
}

/// Ids of a user's sessions that are not revoked (expired ones included).
pub async fn unrevoked_ids(db: impl SqliteExecutor<'_>, user_id: &str) -> AppResult<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM sessions WHERE user_id = ? AND revoked_at IS NULL")
            .bind(user_id)
            .fetch_all(db)
            .await?,
    )
}

/// Revoke every session bound to a device; returns their ids.
pub async fn revoke_for_device(
    db: impl SqliteExecutor<'_>,
    device_id: &str,
    now: i64,
) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "UPDATE sessions SET revoked_at = ? WHERE device_id = ? AND revoked_at IS NULL RETURNING id",
    )
    .bind(now)
    .bind(device_id)
    .fetch_all(db)
    .await?)
}

/// Revoke every session of a user; returns their ids.
pub async fn revoke_for_user(
    db: impl SqliteExecutor<'_>,
    user_id: &str,
    now: i64,
) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "UPDATE sessions SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL RETURNING id",
    )
    .bind(now)
    .bind(user_id)
    .fetch_all(db)
    .await?)
}

/// Revoke the sessions an agent obtained with one client credential.
pub async fn revoke_for_credential(
    db: impl SqliteExecutor<'_>,
    agent_id: &str,
    client_id: &str,
    now: i64,
) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "UPDATE sessions SET revoked_at = ? WHERE user_id = ? AND kind = 'agent' AND client_name = ? AND revoked_at IS NULL RETURNING id",
    )
    .bind(now)
    .bind(agent_id)
    .bind(client_id)
    .fetch_all(db)
    .await?)
}

/// `(agent id, session id)` of the unrevoked sessions of a person's agents.
pub async fn unrevoked_of_agents(
    db: impl SqliteExecutor<'_>,
    owner_id: &str,
) -> AppResult<Vec<(String, String)>> {
    Ok(sqlx::query_as(
        "SELECT s.user_id, s.id FROM sessions s JOIN users u ON u.id = s.user_id
         WHERE u.owner_id = ? AND s.revoked_at IS NULL",
    )
    .bind(owner_id)
    .fetch_all(db)
    .await?)
}

/// Revoke the sessions of accounts with an identity from `dev_issuer`.
pub async fn revoke_for_issuer(
    db: impl SqliteExecutor<'_>,
    dev_issuer: &str,
    now: i64,
) -> AppResult<u64> {
    Ok(sqlx::query(
        "UPDATE sessions SET revoked_at = ? WHERE revoked_at IS NULL AND user_id IN
           (SELECT user_id FROM identities WHERE issuer = ?)",
    )
    .bind(now)
    .bind(dev_issuer)
    .execute(db)
    .await?
    .rows_affected())
}

/// Forget sessions that expired or were revoked before `before`.
pub async fn delete_ended_before(db: impl SqliteExecutor<'_>, before: i64) -> AppResult<()> {
    sqlx::query("DELETE FROM sessions WHERE expires_at < ? OR revoked_at < ?")
        .bind(before)
        .bind(before)
        .execute(db)
        .await?;
    Ok(())
}

// --- OIDC login states ---

pub struct OidcState {
    pub pkce_verifier: String,
    pub nonce: String,
    pub return_to: String,
    pub expires_at: i64,
    pub browser_hash: Option<Vec<u8>>,
}

pub async fn insert_oidc_state(
    db: impl SqliteExecutor<'_>,
    state: &str,
    s: &OidcState,
) -> AppResult<()> {
    sqlx::query("INSERT INTO oidc_states (state, pkce_verifier, nonce, return_to, expires_at, browser_hash) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(state)
        .bind(&s.pkce_verifier)
        .bind(&s.nonce)
        .bind(&s.return_to)
        .bind(s.expires_at)
        .bind(&s.browser_hash)
        .execute(db)
        .await?;
    Ok(())
}

/// Remove and return a login state (states are single-use).
pub async fn take_oidc_state(
    db: impl SqliteExecutor<'_>,
    state: &str,
) -> AppResult<Option<OidcState>> {
    Ok(sqlx::query("DELETE FROM oidc_states WHERE state = ? RETURNING pkce_verifier, nonce, return_to, expires_at, browser_hash")
        .bind(state)
        .fetch_optional(db)
        .await?
        .map(|row| OidcState {
            pkce_verifier: row.get("pkce_verifier"),
            nonce: row.get("nonce"),
            return_to: row.get("return_to"),
            expires_at: row.get("expires_at"),
            browser_hash: row.get("browser_hash"),
        }))
}

pub async fn delete_expired_oidc_states(db: impl SqliteExecutor<'_>, now: i64) -> AppResult<()> {
    sqlx::query("DELETE FROM oidc_states WHERE expires_at < ?")
        .bind(now)
        .execute(db)
        .await?;
    Ok(())
}

// --- One-time login codes (OIDC callback → browser) ---

pub async fn insert_login_code(
    db: impl SqliteExecutor<'_>,
    code_hash: &[u8],
    user_id: &str,
    expires_at: i64,
) -> AppResult<()> {
    sqlx::query("INSERT INTO login_codes (code_hash, user_id, expires_at) VALUES (?, ?, ?)")
        .bind(code_hash)
        .bind(user_id)
        .bind(expires_at)
        .execute(db)
        .await?;
    Ok(())
}

/// Remove and return `(user id, expiry)` of a login code (single-use).
pub async fn take_login_code(
    db: impl SqliteExecutor<'_>,
    code_hash: &[u8],
) -> AppResult<Option<(String, i64)>> {
    Ok(
        sqlx::query_as("DELETE FROM login_codes WHERE code_hash = ? RETURNING user_id, expires_at")
            .bind(code_hash)
            .fetch_optional(db)
            .await?,
    )
}

pub async fn delete_expired_login_codes(db: impl SqliteExecutor<'_>, now: i64) -> AppResult<()> {
    sqlx::query("DELETE FROM login_codes WHERE expires_at < ?")
        .bind(now)
        .execute(db)
        .await?;
    Ok(())
}

// --- Device authorization grants (RFC 8628) ---

pub struct DeviceGrant {
    /// `pending`, `approved`, `denied` or `consumed`.
    pub status: String,
    /// The person who approved or denied it.
    pub user_id: Option<String>,
    pub client_name: String,
    pub expires_at: i64,
    pub last_poll_at: Option<i64>,
}

/// New pending grant; `Ok(false)` if the device or user code is taken.
/// `requested_from`: the address that asked for it, shown when approving.
pub async fn insert_device_grant(
    db: impl SqliteExecutor<'_>,
    device_code_hash: &[u8],
    user_code: &str,
    client_name: &str,
    requested_from: &str,
    now: i64,
    expires_at: i64,
) -> AppResult<bool> {
    super::inserted(
        sqlx::query(
            "INSERT INTO device_grants (device_code_hash, user_code, client_name, requested_from, status, created_at, expires_at)
             VALUES (?, ?, ?, ?, 'pending', ?, ?)",
        )
        .bind(device_code_hash)
        .bind(user_code)
        .bind(client_name)
        .bind(requested_from)
        .bind(now)
        .bind(expires_at)
        .execute(db)
        .await,
    )
}

pub async fn device_grant(
    db: impl SqliteExecutor<'_>,
    device_code_hash: &[u8],
) -> AppResult<Option<DeviceGrant>> {
    Ok(sqlx::query(
        "SELECT status, user_id, client_name, expires_at, last_poll_at FROM device_grants WHERE device_code_hash = ?",
    )
    .bind(device_code_hash)
    .fetch_optional(db)
    .await?
    .map(|row| DeviceGrant {
        status: row.get("status"),
        user_id: row.get("user_id"),
        client_name: row.get("client_name"),
        expires_at: row.get("expires_at"),
        last_poll_at: row.get("last_poll_at"),
    }))
}

pub async fn set_device_grant_polled(
    db: impl SqliteExecutor<'_>,
    device_code_hash: &[u8],
    now: i64,
) -> AppResult<()> {
    sqlx::query("UPDATE device_grants SET last_poll_at = ? WHERE device_code_hash = ?")
        .bind(now)
        .bind(device_code_hash)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn consume_device_grant(
    db: impl SqliteExecutor<'_>,
    device_code_hash: &[u8],
) -> AppResult<()> {
    sqlx::query("UPDATE device_grants SET status = 'consumed' WHERE device_code_hash = ?")
        .bind(device_code_hash)
        .execute(db)
        .await?;
    Ok(())
}

/// `(client name, created at, expires at, requested from)` of a pending,
/// unexpired grant.
pub async fn pending_device_grant(
    db: impl SqliteExecutor<'_>,
    user_code: &str,
    now: i64,
) -> AppResult<Option<(String, i64, i64, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT client_name, created_at, expires_at, requested_from FROM device_grants
         WHERE user_code = ? AND status = 'pending' AND expires_at > ?",
    )
    .bind(user_code)
    .bind(now)
    .fetch_optional(db)
    .await?)
}

/// Approve or deny a pending, unexpired grant (its address is forgotten);
/// returns the rows affected.
pub async fn decide_device_grant(
    db: impl SqliteExecutor<'_>,
    user_code: &str,
    approve: bool,
    user_id: &str,
    now: i64,
) -> AppResult<u64> {
    let status = if approve { "approved" } else { "denied" };
    Ok(sqlx::query(
        "UPDATE device_grants SET status = ?, user_id = ?, requested_from = NULL
         WHERE user_code = ? AND status = 'pending' AND expires_at > ?",
    )
    .bind(status)
    .bind(user_id)
    .bind(user_code)
    .bind(now)
    .execute(db)
    .await?
    .rows_affected())
}

/// Forget the addresses of expired grants (kept a while longer themselves,
/// for the token endpoint's answers).
pub async fn forget_device_grant_addresses(db: impl SqliteExecutor<'_>, now: i64) -> AppResult<()> {
    sqlx::query(
        "UPDATE device_grants SET requested_from = NULL WHERE expires_at <= ? AND requested_from IS NOT NULL",
    )
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn delete_device_grants_expired_before(
    db: impl SqliteExecutor<'_>,
    before: i64,
) -> AppResult<()> {
    sqlx::query("DELETE FROM device_grants WHERE expires_at < ?")
        .bind(before)
        .execute(db)
        .await?;
    Ok(())
}
