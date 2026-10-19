//! Service administration. Administrators manage accounts and see usage
//! statistics; they gain no access to any vault or its keys.

use axum::Json;
use axum::extract::{Path, State};
use coolpsonotes_core::protocol::{AdminStats, AdminUpdateUser, AdminUser, PrincipalKind};
use sqlx::Row;

use crate::auth::Principal;
use crate::db;
use crate::error::{AppError, AppResult, bad};
use crate::hub::Control;
use crate::state::AppState;

pub async fn users(
    State(state): State<AppState>,
    who: Principal,
) -> AppResult<Json<Vec<AdminUser>>> {
    who.require_admin()?;
    let rows = sqlx::query(
        "SELECT u.id, u.display_name, u.email, u.kind, u.owner_id, u.is_admin, u.disabled, u.created_at, u.last_login_at,
                (SELECT COUNT(*) FROM vault_members m WHERE m.user_id = u.id) AS vault_count,
                (SELECT COUNT(*) FROM devices d WHERE d.user_id = u.id AND d.status = 'active') AS device_count
         FROM users u ORDER BY u.kind, u.created_at",
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        rows.iter()
            .map(|r| AdminUser {
                id: r.get("id"),
                display_name: r.get("display_name"),
                email: r.get("email"),
                kind: PrincipalKind::parse(r.get("kind")).unwrap_or(PrincipalKind::Human),
                owner_id: r.get("owner_id"),
                is_admin: r.get("is_admin"),
                disabled: r.get("disabled"),
                created_at: r.get("created_at"),
                last_login_at: r.get("last_login_at"),
                vault_count: r.get("vault_count"),
                device_count: r.get("device_count"),
            })
            .collect(),
    ))
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
    let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM users WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?;
    if exists.is_none() {
        return Err(AppError::NotFound);
    }
    if let Some(d) = req.disabled {
        sqlx::query("UPDATE users SET disabled = ? WHERE id = ?")
            .bind(d)
            .bind(&id)
            .execute(&state.db)
            .await?;
        if d {
            let sessions: Vec<String> =
                sqlx::query_scalar("UPDATE sessions SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL RETURNING id")
                    .bind(db::now())
                    .bind(&id)
                    .fetch_all(&state.db)
                    .await?;
            for s in sessions {
                state.hub.control(&id, Control::SessionRevoked(s));
            }
            // Their agents stop too (authentication checks the owner); close
            // the agents' live connections right away.
            let agent_sessions: Vec<(String, String)> = sqlx::query_as(
                "SELECT s.user_id, s.id FROM sessions s JOIN users u ON u.id = s.user_id
                 WHERE u.owner_id = ? AND s.revoked_at IS NULL",
            )
            .bind(&id)
            .fetch_all(&state.db)
            .await?;
            for (agent, s) in agent_sessions {
                state.hub.control(&agent, Control::SessionRevoked(s));
            }
        }
    }
    if let Some(a) = req.is_admin {
        sqlx::query("UPDATE users SET is_admin = ? WHERE id = ? AND kind = 'human'")
            .bind(a)
            .bind(&id)
            .execute(&state.db)
            .await?;
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
    let count = |q: &'static str| {
        let db = state.db.clone();
        async move { sqlx::query_scalar::<_, i64>(q).fetch_one(&db).await }
    };
    let page_count: i64 = sqlx::query_scalar("PRAGMA page_count")
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
    let page_size: i64 = sqlx::query_scalar("PRAGMA page_size")
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);
    Ok(Json(AdminStats {
        users: count("SELECT COUNT(*) FROM users WHERE kind = 'human'").await?,
        agents: count("SELECT COUNT(*) FROM users WHERE kind = 'agent'").await?,
        vaults: count("SELECT COUNT(*) FROM vaults").await?,
        objects: count("SELECT COUNT(*) FROM objects WHERE purged_at IS NULL").await?,
        updates: count("SELECT COUNT(*) FROM updates").await?,
        snapshots: count("SELECT COUNT(*) FROM snapshots").await?,
        attachments: count("SELECT COUNT(*) FROM attachments").await?,
        storage_bytes: page_count * page_size,
    }))
}
