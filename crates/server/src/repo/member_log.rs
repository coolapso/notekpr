//! The vault membership log (`member_log`).

use notekpr_core::memberlog::LogEntry;
use sqlx::{Row, SqliteExecutor};

use crate::error::{AppError, AppResult};

/// Sequence number and hash of the last entry (`None`: no log yet).
pub async fn head(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
) -> AppResult<Option<(u64, Vec<u8>)>> {
    Ok(
        sqlx::query(
            "SELECT seq, hash FROM member_log WHERE vault_id = ? ORDER BY seq DESC LIMIT 1",
        )
        .bind(vault_id)
        .fetch_optional(db)
        .await?
        .map(|r| (r.get::<i64, _>("seq") as u64, r.get("hash"))),
    )
}

pub async fn append(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    entry: &LogEntry,
    now: i64,
) -> AppResult<()> {
    let json = serde_json::to_string(entry).map_err(|e| AppError::Internal(e.into()))?;
    sqlx::query(
        "INSERT INTO member_log (vault_id, seq, entry, hash, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(vault_id)
    .bind(entry.seq as i64)
    .bind(json)
    .bind(entry.hash().to_vec())
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

/// Entries with `seq >= since`, in order.
pub async fn since(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    since: u64,
) -> AppResult<Vec<LogEntry>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT entry FROM member_log WHERE vault_id = ? AND seq >= ? ORDER BY seq",
    )
    .bind(vault_id)
    .bind(since as i64)
    .fetch_all(db)
    .await?;
    rows.iter()
        .map(|j| serde_json::from_str(j).map_err(|e| AppError::Internal(e.into())))
        .collect()
}
