//! The audit log: who did what, when, to which vault or object. Entries
//! hold ids and small details — never content, titles, names or tokens.

use notekpr_core::protocol::AuditEntry;
use sqlx::{Row, SqliteExecutor};

use crate::db;
use crate::error::AppResult;

/// Record an entry. Best effort: a failure is logged, never returned, so an
/// audit problem can't fail (or half-fail) the request it describes.
pub async fn record(
    db: impl SqliteExecutor<'_>,
    actor_id: Option<&str>,
    user_id: Option<&str>,
    vault_id: Option<&str>,
    action: &str,
    target: Option<&str>,
    details: serde_json::Value,
) {
    let r = sqlx::query(
        "INSERT INTO audit_log (at, actor_id, user_id, vault_id, action, target, details) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(db::now())
    .bind(actor_id)
    .bind(user_id)
    .bind(vault_id)
    .bind(action)
    .bind(target)
    .bind(details.to_string())
    .execute(db)
    .await;
    if let Err(e) = r {
        tracing::warn!(error = %e, action, "failed to write audit log");
    }
}

fn entry(r: &sqlx::sqlite::SqliteRow) -> AuditEntry {
    let details: String = r.get("details");
    AuditEntry {
        id: r.get("id"),
        at: r.get("at"),
        actor_id: r.get("actor_id"),
        actor_name: r.get("display_name"),
        action: r.get("action"),
        target: r.get("target"),
        details: serde_json::from_str(&details).unwrap_or(serde_json::Value::Null),
    }
}

/// The newest 500 entries about a vault.
pub async fn for_vault(db: impl SqliteExecutor<'_>, vault_id: &str) -> AppResult<Vec<AuditEntry>> {
    let rows = sqlx::query(
        "SELECT a.id, a.at, a.actor_id, u.display_name, a.action, a.target, a.details
         FROM audit_log a LEFT JOIN users u ON u.id = a.actor_id
         WHERE a.vault_id = ? ORDER BY a.id DESC LIMIT 500",
    )
    .bind(vault_id)
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(entry).collect())
}

/// The newest 200 entries about a user's account.
pub async fn for_user(db: impl SqliteExecutor<'_>, user_id: &str) -> AppResult<Vec<AuditEntry>> {
    let rows = sqlx::query(
        "SELECT a.id, a.at, a.actor_id, u.display_name, a.action, a.target, a.details
         FROM audit_log a LEFT JOIN users u ON u.id = a.actor_id
         WHERE a.user_id = ? ORDER BY a.id DESC LIMIT 200",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(entry).collect())
}
