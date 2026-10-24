//! Admin clients (`admin_clients`) and their tokens (`admin_tokens`: only
//! hashes). Who may do what with them is decided in `routes::admin` and
//! [`crate::admin_clients`].

use notekpr_core::protocol::{AdminClientInfo, AdminScope, AdminTokenInfo};
use sqlx::{Row, SqliteExecutor};

use crate::error::AppResult;

pub fn scopes_column(scopes: &[AdminScope]) -> String {
    scopes
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

pub fn parse_scopes(column: &str) -> Vec<AdminScope> {
    column.split(',').filter_map(AdminScope::parse).collect()
}

/// `(id, name, scopes, managed)` of the client with this name.
pub async fn by_name(
    db: impl SqliteExecutor<'_>,
    name: &str,
) -> AppResult<Option<(String, String, Vec<AdminScope>, bool)>> {
    let row: Option<(String, String, String, bool)> =
        sqlx::query_as("SELECT id, name, scopes, managed FROM admin_clients WHERE name = ?")
            .bind(name)
            .fetch_optional(db)
            .await?;
    Ok(row.map(|(id, name, scopes, managed)| (id, name, parse_scopes(&scopes), managed)))
}

/// `(name, scopes)` of a client.
pub async fn get(
    db: impl SqliteExecutor<'_>,
    id: &str,
) -> AppResult<Option<(String, Vec<AdminScope>)>> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT name, scopes FROM admin_clients WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await?;
    Ok(row.map(|(name, scopes)| (name, parse_scopes(&scopes))))
}

/// Every client with its tokens, by name.
pub async fn list(db: &crate::db::Db) -> AppResult<Vec<AdminClientInfo>> {
    let clients = sqlx::query(
        "SELECT id, name, scopes, managed, created_at FROM admin_clients ORDER BY name",
    )
    .fetch_all(db)
    .await?;
    let tokens = sqlx::query(
        "SELECT id, client_id, hint, created_at, last_used_at FROM admin_tokens ORDER BY created_at",
    )
    .fetch_all(db)
    .await?;
    Ok(clients
        .iter()
        .map(|c| {
            let id: String = c.get("id");
            AdminClientInfo {
                tokens: tokens
                    .iter()
                    .filter(|t| t.get::<String, _>("client_id") == id)
                    .map(|t| AdminTokenInfo {
                        id: t.get("id"),
                        hint: t.get("hint"),
                        created_at: t.get("created_at"),
                        last_used_at: t.get("last_used_at"),
                    })
                    .collect(),
                id,
                name: c.get("name"),
                scopes: parse_scopes(c.get("scopes")),
                managed: c.get("managed"),
                created_at: c.get("created_at"),
            }
        })
        .collect())
}

/// `Ok(false)` if the name is taken.
pub async fn insert(
    db: impl SqliteExecutor<'_>,
    id: &str,
    name: &str,
    scopes: &[AdminScope],
    managed: bool,
    created_by: Option<&str>,
    now: i64,
) -> AppResult<bool> {
    super::inserted(
        sqlx::query(
            "INSERT INTO admin_clients (id, name, scopes, managed, created_by, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(name)
        .bind(scopes_column(scopes))
        .bind(managed)
        .bind(created_by)
        .bind(now)
        .execute(db)
        .await,
    )
}

pub async fn set_scopes(
    db: impl SqliteExecutor<'_>,
    id: &str,
    scopes: &[AdminScope],
) -> AppResult<()> {
    sqlx::query("UPDATE admin_clients SET scopes = ? WHERE id = ?")
        .bind(scopes_column(scopes))
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn set_managed(db: impl SqliteExecutor<'_>, id: &str, managed: bool) -> AppResult<()> {
    sqlx::query("UPDATE admin_clients SET managed = ? WHERE id = ?")
        .bind(managed)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Deletes its tokens too.
pub async fn delete(db: impl SqliteExecutor<'_>, id: &str) -> AppResult<u64> {
    Ok(sqlx::query("DELETE FROM admin_clients WHERE id = ?")
        .bind(id)
        .execute(db)
        .await?
        .rows_affected())
}

/// `Ok(false)` if this token is stored already (for any client).
pub async fn insert_token(
    db: impl SqliteExecutor<'_>,
    id: &str,
    client_id: &str,
    hash: &[u8],
    hint: &str,
    now: i64,
) -> AppResult<bool> {
    super::inserted(
        sqlx::query(
            "INSERT INTO admin_tokens (id, client_id, token_hash, hint, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(client_id)
        .bind(hash)
        .bind(hint)
        .bind(now)
        .execute(db)
        .await,
    )
}

pub async fn delete_token(
    db: impl SqliteExecutor<'_>,
    client_id: &str,
    token_id: &str,
) -> AppResult<u64> {
    Ok(
        sqlx::query("DELETE FROM admin_tokens WHERE id = ? AND client_id = ?")
            .bind(token_id)
            .bind(client_id)
            .execute(db)
            .await?
            .rows_affected(),
    )
}

/// `(id, hash)` of a client's tokens.
pub async fn token_hashes(
    db: impl SqliteExecutor<'_>,
    client_id: &str,
) -> AppResult<Vec<(String, Vec<u8>)>> {
    Ok(
        sqlx::query_as("SELECT id, token_hash FROM admin_tokens WHERE client_id = ?")
            .bind(client_id)
            .fetch_all(db)
            .await?,
    )
}

/// The client a token belongs to.
pub struct TokenOwner {
    pub token_id: String,
    pub client_id: String,
    pub name: String,
    pub scopes: Vec<AdminScope>,
    pub last_used_at: Option<i64>,
}

pub async fn by_token_hash(
    db: impl SqliteExecutor<'_>,
    hash: &[u8],
) -> AppResult<Option<TokenOwner>> {
    let row = sqlx::query(
        "SELECT t.id AS token_id, t.last_used_at, c.id AS client_id, c.name, c.scopes
         FROM admin_tokens t JOIN admin_clients c ON c.id = t.client_id
         WHERE t.token_hash = ?",
    )
    .bind(hash)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| TokenOwner {
        token_id: r.get("token_id"),
        client_id: r.get("client_id"),
        name: r.get("name"),
        scopes: parse_scopes(r.get("scopes")),
        last_used_at: r.get("last_used_at"),
    }))
}

pub async fn touch_token(db: impl SqliteExecutor<'_>, token_id: &str, now: i64) -> AppResult<()> {
    sqlx::query("UPDATE admin_tokens SET last_used_at = ? WHERE id = ?")
        .bind(now)
        .bind(token_id)
        .execute(db)
        .await?;
    Ok(())
}
