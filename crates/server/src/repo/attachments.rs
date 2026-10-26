//! Encrypted attachment bytes stored in the database, for the SQLite store
//! of [`crate::blobs`] (routes go through `Blobs`, not these).

use sqlx::{Row, SqliteExecutor};

use crate::error::AppResult;

/// `Ok(false)` if the vault already has an attachment with this id.
pub async fn insert(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    id: &str,
    author_id: &str,
    data: &[u8],
    now: i64,
) -> AppResult<bool> {
    super::inserted(
        sqlx::query(
            "INSERT INTO attachments (vault_id, id, size, author_id, data, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(vault_id)
        .bind(id)
        .bind(data.len() as i64)
        .bind(author_id)
        .bind(data)
        .bind(now)
        .execute(db)
        .await,
    )
}

/// Size of an existing attachment (errors if it doesn't exist).
pub async fn size(db: impl SqliteExecutor<'_>, vault_id: &str, id: &str) -> AppResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT size FROM attachments WHERE vault_id = ? AND id = ?")
            .bind(vault_id)
            .bind(id)
            .fetch_one(db)
            .await?,
    )
}

pub async fn data(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    id: &str,
) -> AppResult<Option<Vec<u8>>> {
    Ok(sqlx::query_scalar(
        "SELECT data FROM attachments WHERE vault_id = ? AND id = ? AND purged_at IS NULL",
    )
    .bind(vault_id)
    .bind(id)
    .fetch_optional(db)
    .await?)
}

/// Mark an attachment deleted for good (kept for the undo period).
/// Whether it exists (marked now or before).
pub async fn purge(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    id: &str,
    now: i64,
) -> AppResult<bool> {
    Ok(sqlx::query(
        "UPDATE attachments SET purged_at = COALESCE(purged_at, ?) WHERE vault_id = ? AND id = ?",
    )
    .bind(now)
    .bind(vault_id)
    .bind(id)
    .execute(db)
    .await?
    .rows_affected()
        > 0)
}

/// Undo deleting an attachment for good, if that was at `since` or later.
/// Whether it was.
pub async fn restore(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    id: &str,
    since: i64,
) -> AppResult<bool> {
    Ok(sqlx::query(
        "UPDATE attachments SET purged_at = NULL WHERE vault_id = ? AND id = ? AND purged_at >= ?",
    )
    .bind(vault_id)
    .bind(id)
    .bind(since)
    .execute(db)
    .await?
    .rows_affected()
        > 0)
}

/// A vault's attachments deleted for good at `since` or later, as
/// `(id, purged_at)`.
pub async fn purged_since(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    since: i64,
) -> AppResult<Vec<(String, i64)>> {
    Ok(sqlx::query_as(
        "SELECT id, purged_at FROM attachments WHERE vault_id = ? AND purged_at >= ? ORDER BY purged_at DESC",
    )
    .bind(vault_id)
    .bind(since)
    .fetch_all(db)
    .await?)
}

/// Delete the attachments deleted for good before `before`. Rows deleted.
pub async fn erase_purged_before(db: impl SqliteExecutor<'_>, before: i64) -> AppResult<u64> {
    Ok(
        sqlx::query("DELETE FROM attachments WHERE purged_at IS NOT NULL AND purged_at < ?")
            .bind(before)
            .execute(db)
            .await?
            .rows_affected(),
    )
}

/// Rows affected.
pub async fn delete(db: impl SqliteExecutor<'_>, vault_id: &str, id: &str) -> AppResult<u64> {
    Ok(
        sqlx::query("DELETE FROM attachments WHERE vault_id = ? AND id = ?")
            .bind(vault_id)
            .bind(id)
            .execute(db)
            .await?
            .rows_affected(),
    )
}

/// Every attachment of a vault as `(id, bytes)`, by id (not those deleted
/// for good).
pub async fn all(db: impl SqliteExecutor<'_>, vault_id: &str) -> AppResult<Vec<(String, Vec<u8>)>> {
    Ok(sqlx::query(
        "SELECT id, data FROM attachments WHERE vault_id = ? AND purged_at IS NULL ORDER BY id",
    )
    .bind(vault_id)
    .fetch_all(db)
    .await?
    .iter()
    .map(|r| (r.get("id"), r.get("data")))
    .collect())
}
