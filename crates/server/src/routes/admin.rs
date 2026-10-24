//! Service administration: accounts, usage statistics, the instance
//! settings (`instance.rs`) and admin clients (`admin_clients.rs`).
//! Administrators — people with the admin role, and admin clients, each
//! limited to its scopes ([`Admin`]) — gain no access to any vault or its
//! keys.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use notekpr_core::protocol::{
    AdminClientInfo, AdminScope, AdminSettings, AdminStats, AdminUpdateUser, AdminUser,
    CreateAdminClient, CreatedAdminToken, InstanceSettingsUpdate, PrincipalKind, UpdateAdminClient,
};
use serde::Deserialize;

use crate::admin_clients;
use crate::auth::Admin;
use crate::db;
use crate::error::{AppError, AppResult, bad, forbidden};
use crate::hub::Control;
use crate::repo;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct UsersQuery {
    /// Only the account with this e-mail address (any case).
    email: Option<String>,
    /// Only the account signing in with this OIDC identity, written as in
    /// `NOTEKPR_ADMINS`: `issuer|subject`, or `dev:<name>` in development mode.
    identity: Option<String>,
}

/// An audit entry by an administrator (the admin client's name in its details).
async fn audit(
    state: &AppState,
    admin: &Admin,
    user_id: Option<&str>,
    action: &str,
    target: Option<&str>,
    mut details: serde_json::Value,
) {
    if let (Some(name), Some(d)) = (admin.client_name(), details.as_object_mut()) {
        d.insert("admin_client".into(), name.into());
    }
    db::audit(
        &state.db,
        Some(admin.actor_id()),
        user_id,
        None,
        action,
        target,
        details,
    )
    .await;
}

pub async fn users(
    State(state): State<AppState>,
    admin: Admin,
    Query(q): Query<UsersQuery>,
) -> AppResult<Json<Vec<AdminUser>>> {
    admin.require(AdminScope::Read)?;
    let identity = match q.identity.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(i) => Some(match i.strip_prefix("dev:") {
            Some(name) => (notekpr_core::protocol::DEV_ISSUER, name),
            None => i
                .split_once('|')
                .ok_or_else(|| bad("identity must be issuer|subject"))?,
        }),
    };
    let email = q.email.as_deref().map(str::trim).filter(|e| !e.is_empty());
    Ok(Json(
        repo::users::admin_list(&state.db, email, identity).await?,
    ))
}

pub async fn update_user(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    Json(req): Json<AdminUpdateUser>,
) -> AppResult<Json<serde_json::Value>> {
    if req.disabled.is_some() || req.vault_allowance.is_some() || req.agent_allowance.is_some() {
        admin.require(AdminScope::Accounts)?;
    }
    if req.is_admin.is_some() {
        admin.require(AdminScope::Admins)?;
        if state.config.adminless {
            return Err(bad(
                "this instance has no administrators among its people (--adminless)",
            ));
        }
    }
    if let Admin::Person { who, .. } = &admin
        && id == who.user_id
        && (req.disabled == Some(true) || req.is_admin == Some(false))
    {
        return Err(bad("you cannot disable or demote yourself"));
    }
    let allowances = req.vault_allowance.is_some() || req.agent_allowance.is_some();
    {
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        let Some((kind, disabled)) = repo::users::kind_and_disabled(&mut *tx, &id).await? else {
            return Err(AppError::NotFound);
        };
        let person = PrincipalKind::parse(&kind) == Some(PrincipalKind::Human);
        if allowances && !person {
            return Err(bad(
                "only people have allowances; agents count for their owner",
            ));
        }
        // Enabling an account counts against the accounts limit like a new one.
        if person && disabled && req.disabled == Some(false) {
            state.instance.check_new_person(&mut tx).await?;
        }
        if let Some(d) = req.disabled {
            repo::users::set_disabled(&mut *tx, &id, d).await?;
        }
        if let Some(a) = req.is_admin {
            repo::users::set_admin(&mut *tx, &id, a).await?;
        }
        if let Some(a) = req.vault_allowance {
            repo::users::set_vault_allowance(&mut *tx, &id, a).await?;
        }
        if let Some(a) = req.agent_allowance {
            repo::users::set_agent_allowance(&mut *tx, &id, a).await?;
        }
        tx.commit().await?;
    }
    if req.disabled == Some(true) {
        let sessions = repo::sessions::revoke_for_user(&state.db, &id, db::now()).await?;
        for s in sessions {
            state.hub.control(&id, Control::SessionRevoked(s));
        }
        // Their agents stop too (authentication checks the owner); close
        // the agents' live connections right away.
        let agent_sessions = repo::sessions::unrevoked_of_agents(&state.db, &id).await?;
        for (agent, s) in agent_sessions {
            state.hub.control(&agent, Control::SessionRevoked(s));
        }
    }
    audit(
        &state,
        &admin,
        Some(&id),
        "admin.update_user",
        Some(&id),
        serde_json::json!({
            "disabled": req.disabled,
            "is_admin": req.is_admin,
            "vault_allowance": req.vault_allowance,
            "agent_allowance": req.agent_allowance,
        }),
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn stats(State(state): State<AppState>, admin: Admin) -> AppResult<Json<AdminStats>> {
    admin.require(AdminScope::Read)?;
    Ok(Json(repo::stats::admin_stats(&state.db).await?))
}

pub async fn settings(
    State(state): State<AppState>,
    admin: Admin,
) -> AppResult<Json<AdminSettings>> {
    admin.require(AdminScope::Read)?;
    let storage_bytes = state.instance.refresh_storage(&state.db).await?;
    Ok(Json(AdminSettings {
        settings: state.instance.settings(),
        read_only: state.instance.read_only(),
        storage_bytes,
    }))
}

pub async fn update_settings(
    State(state): State<AppState>,
    admin: Admin,
    Json(req): Json<InstanceSettingsUpdate>,
) -> AppResult<Json<AdminSettings>> {
    admin.require(AdminScope::Settings)?;
    // Under the write lock: once read-only mode is on, no write that began
    // before is still landing.
    let settings = {
        let _w = state.write_lock.lock().await;
        state.instance.update(&state.db, &req).await?
    };
    tracing::info!(read_only = ?state.instance.read_only(), "instance settings changed");
    audit(
        &state,
        &admin,
        None,
        "admin.update_settings",
        None,
        serde_json::json!({ "change": req }),
    )
    .await;
    Ok(Json(AdminSettings {
        settings,
        read_only: state.instance.read_only(),
        storage_bytes: repo::settings::used_bytes(&state.db).await?,
    }))
}

// ------------------------------------------------------------ admin clients

pub async fn clients(
    State(state): State<AppState>,
    admin: Admin,
) -> AppResult<Json<Vec<AdminClientInfo>>> {
    admin.require(AdminScope::Read)?;
    Ok(Json(repo::admin_clients::list(&state.db).await?))
}

/// Managing a client takes the `clients` scope and every scope the client
/// has: nobody hands out, or takes from others, more than they have.
async fn manageable(state: &AppState, admin: &Admin, id: &str) -> AppResult<String> {
    admin.require(AdminScope::Clients)?;
    let (name, scopes) = repo::admin_clients::get(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !admin.covers(&scopes) {
        return Err(forbidden("this admin client has scopes you don't have"));
    }
    Ok(name)
}

pub async fn create_client(
    State(state): State<AppState>,
    admin: Admin,
    Json(req): Json<CreateAdminClient>,
) -> AppResult<(StatusCode, Json<CreatedAdminToken>)> {
    admin.require(AdminScope::Clients)?;
    let scopes = admin_clients::normalize(&req.scopes);
    if !admin.covers(&scopes) {
        return Err(forbidden("you can't give scopes you don't have"));
    }
    let made = {
        let _w = state.write_lock.lock().await;
        admin_clients::create(&state.db, &req.name, &scopes, Some(admin.actor_id())).await?
    };
    audit(
        &state,
        &admin,
        None,
        "admin.create_client",
        Some(&made.client.id),
        serde_json::json!({ "name": made.client.name, "scopes": scopes }),
    )
    .await;
    Ok((StatusCode::CREATED, Json(made)))
}

pub async fn update_client(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    Json(req): Json<UpdateAdminClient>,
) -> AppResult<Json<AdminClientInfo>> {
    let name = manageable(&state, &admin, &id).await?;
    if let Some(scopes) = &req.scopes {
        let scopes = admin_clients::normalize(scopes);
        if !admin.covers(&scopes) {
            return Err(forbidden("you can't give scopes you don't have"));
        }
        repo::admin_clients::set_scopes(&state.db, &id, &scopes).await?;
        audit(
            &state,
            &admin,
            None,
            "admin.update_client",
            Some(&id),
            serde_json::json!({ "name": name, "scopes": scopes }),
        )
        .await;
    }
    Ok(Json(admin_clients::info(&state.db, &id).await?))
}

pub async fn delete_client(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let name = manageable(&state, &admin, &id).await?;
    repo::admin_clients::delete(&state.db, &id).await?;
    audit(
        &state,
        &admin,
        None,
        "admin.delete_client",
        Some(&id),
        serde_json::json!({ "name": name }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn create_token(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> AppResult<(StatusCode, Json<CreatedAdminToken>)> {
    let name = manageable(&state, &admin, &id).await?;
    let made = admin_clients::new_token(&state.db, &id).await?;
    audit(
        &state,
        &admin,
        None,
        "admin.create_token",
        Some(&id),
        serde_json::json!({ "name": name }),
    )
    .await;
    Ok((StatusCode::CREATED, Json(made)))
}

pub async fn delete_token(
    State(state): State<AppState>,
    admin: Admin,
    Path((id, token_id)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    let name = manageable(&state, &admin, &id).await?;
    if repo::admin_clients::delete_token(&state.db, &id, &token_id).await? == 0 {
        return Err(AppError::NotFound);
    }
    audit(
        &state,
        &admin,
        None,
        "admin.delete_token",
        Some(&id),
        serde_json::json!({ "name": name }),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
