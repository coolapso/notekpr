//! Agent (service) identities owned by a person.
//!
//! An agent is a separate principal with its own key pair, generated on the
//! machine running the agent gateway. It only sees vaults its owner explicitly
//! shares with it, with the role chosen at share time, and authenticates with
//! revocable OAuth 2.0 client credentials.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use coolpsonotes_core::protocol::{
    AgentInfo, CreateAgentRequest, CreateAgentResponse, CreatedCredential, CredentialInfo, UserEventKind,
};
use sqlx::Row;

use crate::auth::Principal;
use crate::db;
use crate::error::{AppError, AppResult, bad};
use crate::hub::Control;
use crate::routes::me::notify_user;
use crate::routes::users::account_key;
use crate::state::AppState;

const MAX_AGENTS: i64 = 50;

async fn load(state: &AppState, owner: &str, agent_id: &str) -> AppResult<AgentInfo> {
    let row = sqlx::query("SELECT id, display_name, created_at FROM users WHERE id = ? AND kind = 'agent' AND owner_id = ?")
        .bind(agent_id)
        .bind(owner)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    let credentials = sqlx::query(
        "SELECT client_id, created_at, revoked_at, last_used_at FROM agent_credentials WHERE agent_id = ? ORDER BY created_at",
    )
    .bind(agent_id)
    .fetch_all(&state.db)
    .await?
    .iter()
    .map(|r| CredentialInfo {
        client_id: r.get("client_id"),
        created_at: r.get("created_at"),
        revoked_at: r.get("revoked_at"),
        last_used_at: r.get("last_used_at"),
    })
    .collect();
    Ok(AgentInfo {
        id: row.get("id"),
        display_name: row.get("display_name"),
        created_at: row.get("created_at"),
        account_key: account_key(&state.db, agent_id).await?,
        credentials,
    })
}

async fn new_credential(state: &AppState, agent_id: &str) -> AppResult<CreatedCredential> {
    let client_id = format!("agent_{}", &coolpsonotes_core::random_id()[..16]);
    let (secret, hash) = db::new_secret("cpn_cs_");
    sqlx::query("INSERT INTO agent_credentials (client_id, agent_id, secret_hash, created_at) VALUES (?, ?, ?, ?)")
        .bind(&client_id)
        .bind(agent_id)
        .bind(&hash)
        .bind(db::now())
        .execute(&state.db)
        .await?;
    Ok(CreatedCredential { agent_id: agent_id.to_string(), client_id, client_secret: secret })
}

pub async fn list(State(state): State<AppState>, who: Principal) -> AppResult<Json<Vec<AgentInfo>>> {
    who.require_human()?;
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM users WHERE kind = 'agent' AND owner_id = ? ORDER BY created_at")
        .bind(&who.user_id)
        .fetch_all(&state.db)
        .await?;
    let mut out = Vec::new();
    for id in ids {
        out.push(load(&state, &who.user_id, &id).await?);
    }
    Ok(Json(out))
}

pub async fn create(
    State(state): State<AppState>,
    who: Principal,
    Json(req): Json<CreateAgentRequest>,
) -> AppResult<(StatusCode, Json<CreateAgentResponse>)> {
    who.require_human()?;
    let name: String = req.name.trim().chars().filter(|c| !c.is_control()).take(80).collect();
    if name.is_empty() {
        return Err(bad("agent name must not be empty"));
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE kind = 'agent' AND owner_id = ?")
        .bind(&who.user_id)
        .fetch_one(&state.db)
        .await?;
    if count >= MAX_AGENTS {
        return Err(bad("too many agents"));
    }
    let id = db::new_id();
    sqlx::query("INSERT INTO users (id, kind, display_name, owner_id, created_at) VALUES (?, 'agent', ?, ?, ?)")
        .bind(&id)
        .bind(&name)
        .bind(&who.user_id)
        .bind(db::now())
        .execute(&state.db)
        .await?;
    let credential = new_credential(&state, &id).await?;
    db::audit(&state.db, Some(&who.user_id), Some(&who.user_id), None, "agent.create", Some(&id), serde_json::json!({ "name": name })).await;
    Ok((StatusCode::CREATED, Json(CreateAgentResponse { agent: load(&state, &who.user_id, &id).await?, credential })))
}

pub async fn delete(State(state): State<AppState>, who: Principal, Path(id): Path<String>) -> AppResult<StatusCode> {
    who.require_human()?;
    load(&state, &who.user_id, &id).await?;
    let vaults: Vec<String> = sqlx::query_scalar("SELECT vault_id FROM vault_members WHERE user_id = ?")
        .bind(&id)
        .fetch_all(&state.db)
        .await?;
    {
        let _w = state.write_lock.lock().await;
        sqlx::query("DELETE FROM users WHERE id = ? AND kind = 'agent' AND owner_id = ?")
            .bind(&id)
            .bind(&who.user_id)
            .execute(&state.db)
            .await?;
    }
    for v in vaults {
        state.hub.control(&id, Control::RevokeVault(v));
    }
    db::audit(&state.db, Some(&who.user_id), Some(&who.user_id), None, "agent.delete", Some(&id), serde_json::json!({})).await;
    notify_user(&state, &who.user_id, UserEventKind::VaultsChanged);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn create_credential(
    State(state): State<AppState>,
    who: Principal,
    Path(id): Path<String>,
) -> AppResult<(StatusCode, Json<CreatedCredential>)> {
    who.require_human()?;
    load(&state, &who.user_id, &id).await?;
    let c = new_credential(&state, &id).await?;
    db::audit(&state.db, Some(&who.user_id), Some(&who.user_id), None, "agent.credential_create", Some(&c.client_id), serde_json::json!({})).await;
    Ok((StatusCode::CREATED, Json(c)))
}

/// Revoking a credential also ends every session obtained with it.
pub async fn revoke_credential(
    State(state): State<AppState>,
    who: Principal,
    Path((id, client_id)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    who.require_human()?;
    load(&state, &who.user_id, &id).await?;
    let now = db::now();
    let n = sqlx::query("UPDATE agent_credentials SET revoked_at = ? WHERE client_id = ? AND agent_id = ? AND revoked_at IS NULL")
        .bind(now)
        .bind(&client_id)
        .bind(&id)
        .execute(&state.db)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::NotFound);
    }
    let sessions: Vec<String> = sqlx::query_scalar(
        "UPDATE sessions SET revoked_at = ? WHERE user_id = ? AND kind = 'agent' AND client_name = ? AND revoked_at IS NULL RETURNING id",
    )
    .bind(now)
    .bind(&id)
    .bind(&client_id)
    .fetch_all(&state.db)
    .await?;
    for s in sessions {
        state.hub.control(&id, Control::SessionRevoked(s));
    }
    db::audit(&state.db, Some(&who.user_id), Some(&who.user_id), None, "agent.credential_revoke", Some(&client_id), serde_json::json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}
