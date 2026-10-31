//! Attachments' rows, their bytes in the database store, and the file
//! store's files to delete, for [`crate::blobs`] (routes go through
//! `Blobs`, not these).

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

/// An attachment whose bytes are a file in the file store (`key`).
/// `Ok(false)` if the vault already has an attachment with this id.
pub async fn insert_file(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    id: &str,
    size: usize,
    author_id: &str,
    key: &str,
    now: i64,
) -> AppResult<bool> {
    super::inserted(
        sqlx::query(
            "INSERT INTO attachments (vault_id, id, size, author_id, data, file_key, created_at)
             VALUES (?, ?, ?, ?, X'', ?, ?)",
        )
        .bind(vault_id)
        .bind(id)
        .bind(size as i64)
        .bind(author_id)
        .bind(key)
        .bind(now)
        .execute(db)
        .await,
    )
}

/// Size of an attachment.
pub async fn size(db: impl SqliteExecutor<'_>, vault_id: &str, id: &str) -> AppResult<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT size FROM attachments WHERE vault_id = ? AND id = ?")
            .bind(vault_id)
            .bind(id)
            .fetch_optional(db)
            .await?,
    )
}

/// Where an attachment's bytes are: its file in the file store, else the
/// bytes themselves (not those deleted for good).
pub async fn stored(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    id: &str,
) -> AppResult<Option<(Option<String>, Vec<u8>)>> {
    Ok(sqlx::query_as(
        "SELECT file_key, data FROM attachments WHERE vault_id = ? AND id = ? AND purged_at IS NULL",
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

/// Every attachment of a vault as `(id, file, bytes)` (as [`stored`]), by
/// id (not those deleted for good).
pub async fn all(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
) -> AppResult<Vec<(String, Option<String>, Vec<u8>)>> {
    Ok(sqlx::query(
        "SELECT id, file_key, data FROM attachments WHERE vault_id = ? AND purged_at IS NULL ORDER BY id",
    )
    .bind(vault_id)
    .fetch_all(db)
    .await?
    .iter()
    .map(|r| (r.get("id"), r.get("file_key"), r.get("data")))
    .collect())
}

/// The files of the newest attachments in the file store.
pub async fn newest_file_keys(db: impl SqliteExecutor<'_>, n: i64) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT file_key FROM attachments WHERE file_key IS NOT NULL ORDER BY created_at DESC LIMIT ?",
    )
    .bind(n)
    .fetch_all(db)
    .await?)
}

/// Bytes of the attachments in the file store, those deleted for good but
/// kept included (the database's own pages count the others).
pub async fn file_bytes(db: impl SqliteExecutor<'_>) -> AppResult<u64> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(size), 0) FROM attachments WHERE file_key IS NOT NULL",
    )
    .fetch_one(db)
    .await?;
    Ok(n.max(0) as u64)
}

/// How many attachments keep their bytes in the database.
pub async fn count_in_database(db: impl SqliteExecutor<'_>) -> AppResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM attachments WHERE file_key IS NULL")
            .fetch_one(db)
            .await?,
    )
}

/// Some attachments that keep their bytes in the database, as `(vault, id)`.
pub async fn in_database(db: impl SqliteExecutor<'_>, n: i64) -> AppResult<Vec<(String, String)>> {
    Ok(
        sqlx::query_as("SELECT vault_id, id FROM attachments WHERE file_key IS NULL LIMIT ?")
            .bind(n)
            .fetch_all(db)
            .await?,
    )
}

/// An attachment's bytes kept in the database (deleted for good too: they
/// move with the rest).
pub async fn database_bytes(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    id: &str,
) -> AppResult<Option<Vec<u8>>> {
    Ok(sqlx::query_scalar(
        "SELECT data FROM attachments WHERE vault_id = ? AND id = ? AND file_key IS NULL",
    )
    .bind(vault_id)
    .bind(id)
    .fetch_optional(db)
    .await?)
}

/// The attachment's bytes are now the file `key`: they leave the database.
/// Whether it was still there, in the database.
pub async fn moved_to_file(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    id: &str,
    key: &str,
) -> AppResult<bool> {
    Ok(sqlx::query(
        "UPDATE attachments SET file_key = ?, data = X''
         WHERE vault_id = ? AND id = ? AND file_key IS NULL",
    )
    .bind(key)
    .bind(vault_id)
    .bind(id)
    .execute(db)
    .await?
    .rows_affected()
        > 0)
}

/// A file about to be written: deleted later unless a row claims it.
pub async fn record_upload(db: impl SqliteExecutor<'_>, key: &str, now: i64) -> AppResult<()> {
    sqlx::query("INSERT INTO file_deletions (file_key, since, upload) VALUES (?, ?, 1)")
        .bind(key)
        .bind(now)
        .execute(db)
        .await?;
    Ok(())
}

/// A row takes the uploaded file. Whether it was still waiting (not taken
/// to be deleted).
pub async fn claim_upload(db: impl SqliteExecutor<'_>, key: &str) -> AppResult<bool> {
    Ok(
        sqlx::query("DELETE FROM file_deletions WHERE file_key = ? AND upload = 1")
            .bind(key)
            .execute(db)
            .await?
            .rows_affected()
            > 0,
    )
}

/// Files due for deletion, as `(key, since, upload)`: uploads started
/// before `uploads_before`, files let go of before `deleted_before`.
pub async fn due_deletions(
    db: impl SqliteExecutor<'_>,
    uploads_before: i64,
    deleted_before: i64,
    n: i64,
) -> AppResult<Vec<(String, i64, bool)>> {
    Ok(sqlx::query_as(
        "SELECT file_key, since, upload FROM file_deletions
         WHERE (upload = 1 AND since < ?) OR (upload = 0 AND since < ?) LIMIT ?",
    )
    .bind(uploads_before)
    .bind(deleted_before)
    .bind(n)
    .fetch_all(db)
    .await?)
}

/// Take a file off the list to delete it. Whether it was still on it (an
/// upload may have been claimed meanwhile).
pub async fn take_deletion(
    db: impl SqliteExecutor<'_>,
    key: &str,
    upload: bool,
) -> AppResult<bool> {
    Ok(
        sqlx::query("DELETE FROM file_deletions WHERE file_key = ? AND upload = ?")
            .bind(key)
            .bind(upload)
            .execute(db)
            .await?
            .rows_affected()
            > 0,
    )
}

/// Put a file back on the list (deleting it failed): tried again later.
pub async fn record_deletion(
    db: impl SqliteExecutor<'_>,
    key: &str,
    since: i64,
    upload: bool,
) -> AppResult<()> {
    sqlx::query("INSERT OR IGNORE INTO file_deletions (file_key, since, upload) VALUES (?, ?, ?)")
        .bind(key)
        .bind(since)
        .bind(upload)
        .execute(db)
        .await?;
    Ok(())
}

/// A file deleted already.
pub async fn forget_deletion(db: impl SqliteExecutor<'_>, key: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM file_deletions WHERE file_key = ?")
        .bind(key)
        .execute(db)
        .await?;
    Ok(())
}
