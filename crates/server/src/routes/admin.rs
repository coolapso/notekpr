//! Service administration. Administrators manage accounts and see usage
//! statistics; they gain no access to any vault or its keys.

use axum::Json;
use axum::extract::{Path, State};
use notekpr_core::protocol::{AdminStats, AdminUpdateUser, AdminUser};

use crate::auth::Principal;
use crate::db;
use crate::error::{AppError, AppResult, bad};
use crate::hub::Control;
use crate::repo;
use crate::state::AppState;

pub async fn users(
    State(state): State<AppState>,
    who: Principal,
) -> AppResult<Json<Vec<AdminUser>>> {
    who.require_admin()?;
    Ok(Json(repo::users::admin_list(&state.db).await?))
}

pub async fn update_user(
    State(state): State<AppState>,
    who: Principal,
    Path(id): Path<String>,
    Json(req): Json<AdminUpdateUser>,
) -> AppResult<Json<serde_json::Value>> {
    who.require_admin()?;
    if id == who.user_id && (req.disabled == Some(true) || req.is_admin == Some(false)) {
        return Err(bad("you cannot disable or demote yourself"));
    }
    if !repo::users::exists(&state.db, &id).await? {
        return Err(AppError::NotFound);
    }
    if let Some(d) = req.disabled {
        repo::users::set_disabled(&state.db, &id, d).await?;
        if d {
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
    }
    if let Some(a) = req.is_admin {
        repo::users::set_admin(&state.db, &id, a).await?;
    }
    db::audit(
        &state.db,
        Some(&who.user_id),
        Some(&id),
        None,
        "admin.update_user",
        Some(&id),
        serde_json::json!({ "disabled": req.disabled, "is_admin": req.is_admin }),
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn stats(State(state): State<AppState>, who: Principal) -> AppResult<Json<AdminStats>> {
    who.require_admin()?;
    Ok(Json(repo::stats::admin_stats(&state.db).await?))
}
