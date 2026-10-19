//! Finding people to share with and fetching their public keys.

use axum::extract::{Path, Query, State};
use axum::Json;
use coolpsonotes_core::protocol::{PrincipalKind, PublicAccountKey, PublicUser};
use serde::Deserialize;
use sqlx::Row;

use crate::auth::Principal;
use crate::config::UserDirectory;
use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub async fn account_key(db: &Db, user_id: &str) -> AppResult<Option<PublicAccountKey>> {
    Ok(sqlx::query("SELECT key_version, enc_public, sign_public, signature FROM account_keys WHERE user_id = ?")
        .bind(user_id)
        .fetch_optional(db)
        .await?
        .map(|r| PublicAccountKey {
            user_id: user_id.to_string(),
            key_version: r.get::<i64, _>("key_version") as u32,
            enc_public: r.get("enc_public"),
            sign_public: r.get("sign_public"),
            signature: r.get("signature"),
        }))
}

pub async fn public_user(db: &Db, user_id: &str) -> AppResult<PublicUser> {
    let r = sqlx::query("SELECT id, display_name, email, kind FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(PublicUser {
        id: r.get("id"),
        display_name: r.get("display_name"),
        email: r.get("email"),
        kind: PrincipalKind::parse(r.get("kind")).unwrap_or(PrincipalKind::Human),
        account_key: account_key(db, user_id).await?,
    })
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
    let like = format!("{}%", term.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    let ids: Vec<String> = match state.config.user_directory {
        UserDirectory::Search => {
            sqlx::query_scalar(
                "SELECT id FROM users WHERE id != ? AND disabled = 0 AND (
                    (kind = 'human' AND (lower(display_name) LIKE ? ESCAPE '\\' OR lower(email) = ?))
                    OR (kind = 'agent' AND owner_id = ? AND lower(display_name) LIKE ? ESCAPE '\\'))
                 ORDER BY kind, display_name LIMIT 10",
            )
            .bind(&who.user_id)
            .bind(&like)
            .bind(&term)
            .bind(&who.user_id)
            .bind(&like)
            .fetch_all(&state.db)
            .await?
        }
        UserDirectory::ExactEmail => {
            sqlx::query_scalar(
                "SELECT id FROM users WHERE id != ? AND disabled = 0 AND (
                    (kind = 'human' AND lower(email) = ?)
                    OR (kind = 'agent' AND owner_id = ? AND lower(display_name) LIKE ? ESCAPE '\\'))
                 LIMIT 10",
            )
            .bind(&who.user_id)
            .bind(&term)
            .bind(&who.user_id)
            .bind(&like)
            .fetch_all(&state.db)
            .await?
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
pub async fn get_user(State(state): State<AppState>, who: Principal, Path(id): Path<String>) -> AppResult<Json<PublicUser>> {
    let row = sqlx::query("SELECT kind, owner_id, disabled FROM users WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    let kind: String = row.get("kind");
    let owner: Option<String> = row.get("owner_id");
    let disabled: bool = row.get("disabled");
    let shares_vault: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM vault_members a JOIN vault_members b ON a.vault_id = b.vault_id
         WHERE a.user_id = ? AND b.user_id = ? LIMIT 1",
    )
    .bind(&who.user_id)
    .bind(&id)
    .fetch_optional(&state.db)
    .await?;
    // People connected through an individually shared note.
    let shares_note: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM object_shares s JOIN vault_members m ON m.vault_id = s.vault_id
         WHERE (s.user_id = ? AND m.user_id = ?) OR (s.user_id = ? AND m.user_id = ?) LIMIT 1",
    )
    .bind(&who.user_id)
    .bind(&id)
    .bind(&id)
    .bind(&who.user_id)
    .fetch_optional(&state.db)
    .await?;
    let allowed = id == who.user_id
        || shares_vault.is_some()
        || shares_note.is_some()
        || owner.as_deref() == Some(who.user_id.as_str())
        || (kind == "human" && !disabled && who.is_human() && state.config.user_directory == UserDirectory::Search);
    if !allowed {
        return Err(AppError::NotFound);
    }
    Ok(Json(public_user(&state.db, &id).await?))
}
