//! Bearer-token authentication and identity provisioning.
//!
//! Interactive users authenticate only via OIDC (or the explicit development
//! login). The server then issues its own opaque session tokens; only their
//! SHA-256 hashes are stored. Agents use OAuth 2.0 client credentials.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use coolpsonotes_core::protocol::{PrincipalKind, UserInfo};
use sqlx::Row;

use crate::db::{self, Db};
use crate::error::{AppError, AppResult, forbidden};
use crate::state::AppState;

pub const SESSION_PREFIX: &str = "cpn_s_";

#[derive(Debug, Clone)]
pub struct Principal {
    pub user_id: String,
    pub kind: PrincipalKind,
    pub display_name: String,
    pub is_admin: bool,
    pub session_id: String,
    pub session_kind: String,
    pub device_id: Option<String>,
    pub owner_id: Option<String>,
}

impl Principal {
    pub fn is_human(&self) -> bool {
        self.kind == PrincipalKind::Human
    }

    /// Account, device, sharing and agent management is reserved for people.
    pub fn require_human(&self) -> AppResult<()> {
        if self.is_human() {
            Ok(())
        } else {
            Err(forbidden("not available to agent identities"))
        }
    }

    pub fn require_admin(&self) -> AppResult<()> {
        if self.is_admin && self.is_human() {
            Ok(())
        } else {
            Err(forbidden("administrator access required"))
        }
    }
}

pub fn bearer(parts: &Parts) -> Option<&str> {
    let v = parts
        .headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = v.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

impl FromRequestParts<AppState> for Principal {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = bearer(parts).ok_or(AppError::Unauthorized)?;
        authenticate(state, token).await
    }
}

pub async fn authenticate(state: &AppState, token: &str) -> AppResult<Principal> {
    if token.len() > 256 {
        return Err(AppError::Unauthorized);
    }
    let hash = db::sha256(token.as_bytes());
    let row = sqlx::query(
        "SELECT s.id, s.user_id, s.device_id, s.kind AS skind, s.expires_at, s.revoked_at, s.last_used_at,
                u.kind AS ukind, u.display_name, u.is_admin, u.disabled, u.owner_id,
                COALESCE(o.disabled, 0) AS owner_disabled
         FROM sessions s JOIN users u ON u.id = s.user_id
         LEFT JOIN users o ON o.id = u.owner_id
         WHERE s.token_hash = ?",
    )
    .bind(&hash)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::Unauthorized)?;
    let now = db::now();
    let revoked: Option<i64> = row.get("revoked_at");
    let expires: i64 = row.get("expires_at");
    // An agent stops working when its owner's account is disabled.
    let disabled: bool = row.get::<bool, _>("disabled") || row.get::<bool, _>("owner_disabled");
    if revoked.is_some() || expires <= now || disabled {
        return Err(AppError::Unauthorized);
    }
    let p = Principal {
        session_id: row.get("id"),
        user_id: row.get("user_id"),
        device_id: row.get("device_id"),
        session_kind: row.get("skind"),
        kind: PrincipalKind::parse(row.get("ukind")).unwrap_or(PrincipalKind::Human),
        display_name: row.get("display_name"),
        is_admin: row.get("is_admin"),
        owner_id: row.get("owner_id"),
    };
    let last_used: Option<i64> = row.get("last_used_at");
    if last_used.is_none_or(|t| now - t > 60_000) {
        let _ = sqlx::query("UPDATE sessions SET last_used_at = ? WHERE id = ?")
            .bind(now)
            .bind(&p.session_id)
            .execute(&state.db)
            .await;
        if let Some(d) = &p.device_id {
            let _ = sqlx::query("UPDATE devices SET last_seen_at = ? WHERE id = ?")
                .bind(now)
                .bind(d)
                .execute(&state.db)
                .await;
        }
    }
    Ok(p)
}

pub struct NewSession {
    pub token: String,
    pub id: String,
    pub expires_at: i64,
}

pub async fn create_session(
    db: &Db,
    user_id: &str,
    kind: &str,
    client_name: Option<&str>,
    ttl_ms: i64,
) -> AppResult<NewSession> {
    let (token, hash) = db::new_secret(SESSION_PREFIX);
    let id = db::new_id();
    let now = db::now();
    let expires_at = now + ttl_ms;
    sqlx::query(
        "INSERT INTO sessions (id, token_hash, user_id, kind, client_name, created_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&hash)
    .bind(user_id)
    .bind(kind)
    .bind(client_name)
    .bind(now)
    .bind(expires_at)
    .execute(db)
    .await?;
    Ok(NewSession {
        token,
        id,
        expires_at,
    })
}

/// Find or create the user for an authenticated OIDC identity (`iss` + `sub`).
/// Authentication never grants access to any vault by itself.
pub async fn provision_user(
    state: &AppState,
    issuer: &str,
    subject: &str,
    display_name: &str,
    email: Option<&str>,
) -> AppResult<String> {
    let _w = state.write_lock.lock().await;
    let now = db::now();
    let configured_admin = state.config.is_admin_identity(issuer, subject);
    let existing: Option<String> =
        sqlx::query_scalar("SELECT user_id FROM identities WHERE issuer = ? AND subject = ?")
            .bind(issuer)
            .bind(subject)
            .fetch_optional(&state.db)
            .await?;
    let user_id = if let Some(id) = existing {
        let disabled: bool = sqlx::query_scalar("SELECT disabled FROM users WHERE id = ?")
            .bind(&id)
            .fetch_one(&state.db)
            .await?;
        if disabled {
            return Err(forbidden("this account has been disabled"));
        }
        sqlx::query(
            "UPDATE users SET display_name = ?, email = COALESCE(?, email), last_login_at = ?,
             is_admin = CASE WHEN ? THEN 1 ELSE is_admin END WHERE id = ?",
        )
        .bind(display_name)
        .bind(email)
        .bind(now)
        .bind(configured_admin)
        .bind(&id)
        .execute(&state.db)
        .await?;
        id
    } else {
        // "First user" counts real accounts and development-mode accounts
        // separately, so test users from dev mode never take the admin role
        // from the first real person signing in on the same database.
        let dev = issuer == coolpsonotes_core::protocol::DEV_ISSUER;
        let humans: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM users u WHERE u.kind = 'human' AND EXISTS
               (SELECT 1 FROM identities i WHERE i.user_id = u.id AND (i.issuer = ?) = ?)",
        )
        .bind(coolpsonotes_core::protocol::DEV_ISSUER)
        .bind(dev)
        .fetch_one(&state.db)
        .await?;
        let is_admin = configured_admin || (state.config.first_user_admin && humans == 0);
        let id = db::new_id();
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "INSERT INTO users (id, kind, display_name, email, is_admin, created_at, last_login_at)
             VALUES (?, 'human', ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(display_name)
        .bind(email)
        .bind(is_admin)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO identities (issuer, subject, user_id, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(issuer)
        .bind(subject)
        .bind(&id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        tracing::info!(user_id = %id, is_admin, "provisioned new user");
        id
    };
    db::audit(
        &state.db,
        Some(&user_id),
        Some(&user_id),
        None,
        "login",
        None,
        serde_json::json!({}),
    )
    .await;
    Ok(user_id)
}

pub async fn user_info(db: &Db, user_id: &str) -> AppResult<UserInfo> {
    let row = sqlx::query(
        "SELECT id, display_name, email, kind, is_admin, created_at FROM users WHERE id = ?",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(UserInfo {
        id: row.get("id"),
        display_name: row.get("display_name"),
        email: row.get("email"),
        kind: PrincipalKind::parse(row.get("kind")).unwrap_or(PrincipalKind::Human),
        is_admin: row.get("is_admin"),
        created_at: row.get("created_at"),
    })
}
