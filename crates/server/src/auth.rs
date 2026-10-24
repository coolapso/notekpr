//! Bearer-token authentication and identity provisioning.
//!
//! Interactive users authenticate only via OIDC (or the explicit development
//! login). The server then issues its own opaque session tokens; only their
//! SHA-256 hashes are stored. Agents use OAuth 2.0 client credentials.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use notekpr_core::protocol::{ADMIN_TOKEN_PREFIX, AdminScope, PrincipalKind, UserInfo};

use crate::db::{self, Db};
use crate::error::{AppError, AppResult, forbidden};
use crate::repo;
use crate::state::AppState;

pub const SESSION_PREFIX: &str = "notekpr_s_";

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

/// Who is administering: a person with the admin role, or an admin client
/// (`admin_clients.rs`) with its token. Admin clients are no accounts — no
/// user row, keys, sessions or memberships — and only this extractor knows
/// them: every other route takes a [`Principal`], which they can never be,
/// so they can't reach a vault, note or key by any route.
pub enum Admin {
    Person {
        who: Principal,
        scopes: Vec<AdminScope>,
    },
    Client {
        id: String,
        name: String,
        scopes: Vec<AdminScope>,
    },
}

impl Admin {
    pub fn scopes(&self) -> &[AdminScope] {
        match self {
            Admin::Person { scopes, .. } | Admin::Client { scopes, .. } => scopes,
        }
    }

    pub fn require(&self, scope: AdminScope) -> AppResult<()> {
        if self.scopes().contains(&scope) {
            Ok(())
        } else {
            Err(forbidden(format!(
                "this needs the {} scope",
                scope.as_str()
            )))
        }
    }

    /// Whether it may hand out (or manage a client holding) these scopes:
    /// only those it has itself.
    pub fn covers(&self, scopes: &[AdminScope]) -> bool {
        scopes.iter().all(|s| self.scopes().contains(s))
    }

    /// Who acted, for the audit log: an account or an admin client id.
    pub fn actor_id(&self) -> &str {
        match self {
            Admin::Person { who, .. } => &who.user_id,
            Admin::Client { id, .. } => id,
        }
    }

    /// The admin client's name, for the audit log's details.
    pub fn client_name(&self) -> Option<&str> {
        match self {
            Admin::Person { .. } => None,
            Admin::Client { name, .. } => Some(name),
        }
    }
}

impl FromRequestParts<AppState> for Admin {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = bearer(parts).ok_or(AppError::Unauthorized)?;
        if token.starts_with(ADMIN_TOKEN_PREFIX) {
            let c = crate::admin_clients::authenticate(&state.db, token)
                .await?
                .ok_or(AppError::Unauthorized)?;
            return Ok(Admin::Client {
                id: c.id,
                name: c.name,
                scopes: c.scopes,
            });
        }
        // People with the admin role may do everything (on an admin-less
        // instance nobody has it: `authenticate`).
        let who = authenticate(state, token).await?;
        who.require_admin()?;
        Ok(Admin::Person {
            who,
            scopes: AdminScope::ALL.to_vec(),
        })
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
    let row = repo::sessions::by_token_hash(&state.db, &hash)
        .await?
        .ok_or(AppError::Unauthorized)?;
    let now = db::now();
    // An agent stops working when its owner's account is disabled.
    if row.revoked_at.is_some() || row.expires_at <= now || row.disabled || row.owner_disabled {
        return Err(AppError::Unauthorized);
    }
    let p = Principal {
        session_id: row.id,
        user_id: row.user_id,
        device_id: row.device_id,
        session_kind: row.kind,
        kind: PrincipalKind::parse(&row.user_kind).unwrap_or(PrincipalKind::Human),
        display_name: row.display_name,
        // An admin-less instance has no administrators among its people.
        is_admin: row.is_admin && !state.config.adminless,
        owner_id: row.owner_id,
    };
    if row.last_used_at.is_none_or(|t| now - t > 60_000) {
        let _ = repo::sessions::touch(&state.db, &p.session_id, now).await;
        if let Some(d) = &p.device_id {
            let _ = repo::devices::touch(&state.db, d, now).await;
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
    repo::sessions::insert(db, &id, &hash, user_id, kind, client_name, now, expires_at).await?;
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
    let existing = repo::users::identity_user(&state.db, issuer, subject).await?;
    let user_id = if let Some(id) = existing {
        if repo::users::is_disabled(&state.db, &id).await? {
            return Err(forbidden("this account has been disabled"));
        }
        repo::users::record_login(&state.db, &id, display_name, email, configured_admin, now)
            .await?;
        id
    } else {
        // "First user" counts real accounts and development-mode accounts
        // separately, so test users from dev mode never take the admin role
        // from the first real person signing in on the same database.
        let dev = issuer == notekpr_core::protocol::DEV_ISSUER;
        let humans =
            repo::users::count_humans(&state.db, notekpr_core::protocol::DEV_ISSUER, dev).await?;
        let first = state.config.first_user_admin && !state.config.adminless && humans == 0;
        let is_admin = configured_admin || first;
        let id = db::new_id();
        // No new accounts while read-only or at the accounts limit; those
        // who have one still sign in.
        state.instance.require_writable()?;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        state.instance.check_new_person(&mut tx).await?;
        repo::users::insert_human(&mut *tx, &id, display_name, email, is_admin, now).await?;
        repo::users::insert_identity(&mut *tx, issuer, subject, &id, now).await?;
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

/// Someone's account as they see it (on an admin-less instance nobody is an
/// administrator, whatever the stored role says).
pub async fn user_info(state: &AppState, user_id: &str) -> AppResult<UserInfo> {
    let mut u = repo::users::info(&state.db, user_id)
        .await?
        .ok_or(AppError::NotFound)?;
    u.is_admin &= !state.config.adminless;
    Ok(u)
}
