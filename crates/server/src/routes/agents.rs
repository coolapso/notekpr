//! Agent (service) identities owned by a person.
//!
//! An agent is a separate principal with its own key pair, generated on the
//! machine running the agent gateway. It only sees vaults its owner explicitly
//! shares with it, with the role chosen at share time, and authenticates with
//! revocable OAuth 2.0 client credentials.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use notekpr_core::protocol::{
    AgentInfo, CreateAgentRequest, CreateAgentResponse, CreatedCredential, ObjectEventKind,
    UserEventKind, VaultEventKind,
};

use crate::auth::Principal;
use crate::db;
use crate::error::{AppError, AppResult, bad};
use crate::hub::{Control, VaultEvent};
use crate::repo;
use crate::routes::me::notify_user;
use crate::routes::users::account_key;
use crate::state::AppState;

async fn load(state: &AppState, owner: &str, agent_id: &str) -> AppResult<AgentInfo> {
    let (id, display_name, created_at) = repo::agents::get(&state.db, owner, agent_id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(AgentInfo {
        id,
        display_name,
        created_at,
        account_key: account_key(&state.db, agent_id).await?,
        credentials: repo::agents::credentials(&state.db, agent_id).await?,
    })
}

async fn new_credential(state: &AppState, agent_id: &str) -> AppResult<CreatedCredential> {
    let client_id = format!("agent_{}", &notekpr_core::random_id()[..16]);
    let (secret, hash) = db::new_secret("notekpr_cs_");
    repo::agents::insert_credential(&state.db, &client_id, agent_id, &hash, db::now()).await?;
    Ok(CreatedCredential {
        agent_id: agent_id.to_string(),
        client_id,
        client_secret: secret,
    })
}

pub async fn list(
    State(state): State<AppState>,
    who: Principal,
) -> AppResult<Json<Vec<AgentInfo>>> {
    who.require_human()?;
    let ids = repo::agents::ids(&state.db, &who.user_id).await?;
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
    let name: String = req
        .name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    if name.is_empty() {
        return Err(bad("agent name must not be empty"));
    }
    let id = db::new_id();
    {
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        state
            .instance
            .check_new_agent(&mut tx, &who.user_id)
            .await?;
        repo::agents::insert(&mut *tx, &id, &name, &who.user_id, db::now()).await?;
        tx.commit().await?;
    }
    let credential = new_credential(&state, &id).await?;
    db::audit(
        &state.db,
        Some(&who.user_id),
        Some(&who.user_id),
        None,
        "agent.create",
        Some(&id),
        serde_json::json!({ "name": name }),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(CreateAgentResponse {
            agent: load(&state, &who.user_id, &id).await?,
            credential,
        }),
    ))
}

pub async fn delete(
    State(state): State<AppState>,
    who: Principal,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    who.require_human()?;
    load(&state, &who.user_id, &id).await?;
    // What to revoke is read in the same transaction as the delete, so
    // access granted meanwhile doesn't keep a live connection open.
    let (vaults, notes, sessions) = {
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        if repo::agents::get(&mut *tx, &who.user_id, &id)
            .await?
            .is_none()
        {
            return Err(AppError::NotFound);
        }
        let vaults = repo::vaults::ids_of_member(&mut *tx, &id).await?;
        let notes = repo::shares::of_user(&mut *tx, &id).await?;
        let sessions = repo::sessions::unrevoked_ids(&mut *tx, &id).await?;
        // The keys it held are rotated by the owner's apps (the caller's).
        repo::vaults::set_rotation_pending_for_holder(&mut *tx, &id).await?;
        repo::shares::set_rotation_pending_for_holder(&mut *tx, &id).await?;
        repo::agents::delete(&mut *tx, &id, &who.user_id).await?;
        tx.commit().await?;
        (vaults, notes, sessions)
    };
    // End its live connections now, not at the next session check.
    for v in &vaults {
        state.hub.control(&id, Control::RevokeVault(v.clone()));
    }
    for (v, o) in &notes {
        state
            .hub
            .control(&id, Control::RevokeObject(v.clone(), o.clone()));
    }
    // The owner's apps then renew the keys it held.
    for v in &vaults {
        state
            .hub
            .publish(v, VaultEvent::Event(VaultEventKind::MembersChanged));
    }
    for (v, o) in &notes {
        crate::routes::shares::publish(&state, v, o, ObjectEventKind::SharesChanged);
    }
    for s in sessions {
        state.hub.control(&id, Control::SessionRevoked(s));
    }
    db::audit(
        &state.db,
        Some(&who.user_id),
        Some(&who.user_id),
        None,
        "agent.delete",
        Some(&id),
        serde_json::json!({}),
    )
    .await;
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
    db::audit(
        &state.db,
        Some(&who.user_id),
        Some(&who.user_id),
        None,
        "agent.credential_create",
        Some(&c.client_id),
        serde_json::json!({}),
    )
    .await;
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
    if repo::agents::revoke_credential(&state.db, &client_id, &id, now).await? == 0 {
        return Err(AppError::NotFound);
    }
    let sessions = repo::sessions::revoke_for_credential(&state.db, &id, &client_id, now).await?;
    for s in sessions {
        state.hub.control(&id, Control::SessionRevoked(s));
    }
    db::audit(
        &state.db,
        Some(&who.user_id),
        Some(&who.user_id),
        None,
        "agent.credential_revoke",
        Some(&client_id),
        serde_json::json!({}),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
