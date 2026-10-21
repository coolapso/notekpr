//! Finding people to share with and fetching their public keys.

use axum::Json;
use axum::extract::{Path, Query, State};
use notekpr_core::protocol::{PublicAccountKey, PublicUser};
use serde::Deserialize;

use crate::auth::Principal;
use crate::config::UserDirectory;
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::repo;
use crate::state::AppState;

pub async fn account_key(db: &Db, user_id: &str) -> AppResult<Option<PublicAccountKey>> {
    repo::users::account_key(db, user_id).await
}

pub async fn public_user(db: &Db, user_id: &str) -> AppResult<PublicUser> {
    repo::users::public(db, user_id)
        .await?
        .ok_or(AppError::NotFound)
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
}

/// People the caller can share with: active humans matching the query, plus
/// the caller's own agents. Never lists vaults or any content.
pub async fn search(
    State(state): State<AppState>,
    who: Principal,
    Query(q): Query<SearchQuery>,
) -> AppResult<Json<Vec<PublicUser>>> {
    who.require_human()?;
    let term = q.q.trim().to_lowercase();
    if term.chars().count() < 2 {
        return Ok(Json(vec![]));
    }
    let like = format!(
        "{}%",
        term.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let ids = match state.config.user_directory {
        UserDirectory::Search => repo::users::search(&state.db, &who.user_id, &term, &like).await?,
        UserDirectory::ExactEmail => {
            repo::users::search_exact_email(&state.db, &who.user_id, &term, &like).await?
        }
    };
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(public_user(&state.db, &id).await?);
    }
    Ok(Json(out))
}

/// Public profile and key of a user the caller may interact with: themselves,
/// anyone sharing a vault with them, their own agents, or (with the search
/// directory) any active human.
pub async fn get_user(
    State(state): State<AppState>,
    who: Principal,
    Path(id): Path<String>,
) -> AppResult<Json<PublicUser>> {
    let target = repo::users::principal(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    let shares_vault = repo::users::share_a_vault(&state.db, &who.user_id, &id).await?;
    // People connected through an individually shared note.
    let shares_note = repo::users::share_a_note(&state.db, &who.user_id, &id).await?;
    let own_agent = target.owner_id.as_deref() == Some(who.user_id.as_str());
    let searchable = target.kind == "human"
        && !target.disabled
        && who.is_human()
        && state.config.user_directory == UserDirectory::Search;
    let allowed = id == who.user_id || shares_vault || shares_note || own_agent || searchable;
    if !allowed {
        return Err(AppError::NotFound);
    }
    let mut user = public_user(&state.db, &id).await?;
    // People connected only through an individually shared note get the
    // name and key needed to work together, not the e-mail address.
    if !(id == who.user_id || shares_vault || own_agent || searchable) {
        user.email = None;
    }
    Ok(Json(user))
}
