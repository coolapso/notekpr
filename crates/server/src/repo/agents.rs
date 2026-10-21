//! Agent identities (users of kind `agent`, owned by a person) and their
//! OAuth client credentials (only secret hashes are stored).

use notekpr_core::protocol::CredentialInfo;
use sqlx::{Row, SqliteExecutor};

use crate::error::AppResult;

/// `(id, display name, created at)` of one of the owner's agents.
pub async fn get(
    db: impl SqliteExecutor<'_>,
    owner_id: &str,
    agent_id: &str,
) -> AppResult<Option<(String, String, i64)>> {
    Ok(sqlx::query_as(
        "SELECT id, display_name, created_at FROM users WHERE id = ? AND kind = 'agent' AND owner_id = ?",
    )
    .bind(agent_id)
    .bind(owner_id)
    .fetch_optional(db)
    .await?)
}

/// The owner's agents, oldest first.
pub async fn ids(db: impl SqliteExecutor<'_>, owner_id: &str) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM users WHERE kind = 'agent' AND owner_id = ? ORDER BY created_at",
    )
    .bind(owner_id)
    .fetch_all(db)
    .await?)
}

pub async fn count(db: impl SqliteExecutor<'_>, owner_id: &str) -> AppResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE kind = 'agent' AND owner_id = ?")
            .bind(owner_id)
            .fetch_one(db)
            .await?,
    )
}

pub async fn insert(
    db: impl SqliteExecutor<'_>,
    id: &str,
    name: &str,
    owner_id: &str,
    now: i64,
) -> AppResult<()> {
    sqlx::query("INSERT INTO users (id, kind, display_name, owner_id, created_at) VALUES (?, 'agent', ?, ?, ?)")
        .bind(id)
        .bind(name)
        .bind(owner_id)
        .bind(now)
        .execute(db)
        .await?;
    Ok(())
}

/// Delete one of the owner's agents with everything it has (foreign keys
/// cascade: memberships, shares, sessions, credentials, keys).
pub async fn delete(db: impl SqliteExecutor<'_>, agent_id: &str, owner_id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM users WHERE id = ? AND kind = 'agent' AND owner_id = ?")
        .bind(agent_id)
        .bind(owner_id)
        .execute(db)
        .await?;
    Ok(())
}

// --- Client credentials ---

pub async fn credentials(
    db: impl SqliteExecutor<'_>,
    agent_id: &str,
) -> AppResult<Vec<CredentialInfo>> {
    Ok(sqlx::query(
        "SELECT client_id, created_at, revoked_at, last_used_at FROM agent_credentials WHERE agent_id = ? ORDER BY created_at",
    )
    .bind(agent_id)
    .fetch_all(db)
    .await?
    .iter()
    .map(|r| CredentialInfo {
        client_id: r.get("client_id"),
        created_at: r.get("created_at"),
        revoked_at: r.get("revoked_at"),
        last_used_at: r.get("last_used_at"),
    })
    .collect())
}

pub async fn insert_credential(
    db: impl SqliteExecutor<'_>,
    client_id: &str,
    agent_id: &str,
    secret_hash: &[u8],
    now: i64,
) -> AppResult<()> {
    sqlx::query("INSERT INTO agent_credentials (client_id, agent_id, secret_hash, created_at) VALUES (?, ?, ?, ?)")
        .bind(client_id)
        .bind(agent_id)
        .bind(secret_hash)
        .bind(now)
        .execute(db)
        .await?;
    Ok(())
}

/// Revoke one of an agent's live credentials; rows affected.
pub async fn revoke_credential(
    db: impl SqliteExecutor<'_>,
    client_id: &str,
    agent_id: &str,
    now: i64,
) -> AppResult<u64> {
    Ok(sqlx::query("UPDATE agent_credentials SET revoked_at = ? WHERE client_id = ? AND agent_id = ? AND revoked_at IS NULL")
        .bind(now)
        .bind(client_id)
        .bind(agent_id)
        .execute(db)
        .await?
        .rows_affected())
}

/// What the token endpoint checks for a client credential.
pub struct CredentialLogin {
    pub agent_id: String,
    pub secret_hash: Vec<u8>,
    pub revoked_at: Option<i64>,
    pub agent_disabled: bool,
    /// `None` if the agent has no owner row (it always should).
    pub owner_disabled: Option<bool>,
}

pub async fn credential_login(
    db: impl SqliteExecutor<'_>,
    client_id: &str,
) -> AppResult<Option<CredentialLogin>> {
    Ok(sqlx::query(
        "SELECT c.agent_id, c.secret_hash, c.revoked_at, a.disabled AS agent_disabled, o.disabled AS owner_disabled
         FROM agent_credentials c JOIN users a ON a.id = c.agent_id LEFT JOIN users o ON o.id = a.owner_id
         WHERE c.client_id = ?",
    )
    .bind(client_id)
    .fetch_optional(db)
    .await?
    .map(|row| CredentialLogin {
        agent_id: row.get("agent_id"),
        secret_hash: row.get("secret_hash"),
        revoked_at: row.get("revoked_at"),
        agent_disabled: row.get("agent_disabled"),
        owner_disabled: row.get("owner_disabled"),
    }))
}

pub async fn touch_credential(
    db: impl SqliteExecutor<'_>,
    client_id: &str,
    now: i64,
) -> AppResult<()> {
    sqlx::query("UPDATE agent_credentials SET last_used_at = ? WHERE client_id = ?")
        .bind(now)
        .bind(client_id)
        .execute(db)
        .await?;
    Ok(())
}
