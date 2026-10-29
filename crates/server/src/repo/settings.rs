//! The instance settings row (`instance_settings`, one JSON document) and
//! the counts its limits are checked against.

use notekpr_core::protocol::InstanceSettings;
use sqlx::SqliteExecutor;

use crate::error::AppResult;

/// The stored settings; defaults when none were ever saved. Fields a
/// stored document lacks take their defaults too.
pub async fn load(db: impl SqliteExecutor<'_>) -> AppResult<InstanceSettings> {
    let json: Option<String> =
        sqlx::query_scalar("SELECT settings FROM instance_settings WHERE id = 1")
            .fetch_optional(db)
            .await?;
    match json {
        Some(j) => serde_json::from_str(&j)
            .map_err(|e| anyhow::anyhow!("stored instance settings are invalid: {e}").into()),
        None => Ok(InstanceSettings::default()),
    }
}

pub async fn save(
    db: impl SqliteExecutor<'_>,
    settings: &InstanceSettings,
    now: i64,
) -> AppResult<()> {
    let json = serde_json::to_string(settings).map_err(anyhow::Error::from)?;
    sqlx::query(
        "INSERT INTO instance_settings (id, settings, updated_at) VALUES (1, ?, ?)
         ON CONFLICT (id) DO UPDATE SET settings = excluded.settings, updated_at = excluded.updated_at",
    )
    .bind(json)
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

/// Accounts of people that aren't disabled (what `max_users` counts).
pub async fn active_people(db: impl SqliteExecutor<'_>) -> AppResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE kind = 'human' AND disabled = 0")
            .fetch_one(db)
            .await?,
    )
}

pub async fn agents(db: impl SqliteExecutor<'_>) -> AppResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE kind = 'agent'")
            .fetch_one(db)
            .await?,
    )
}

pub async fn vaults(db: impl SqliteExecutor<'_>) -> AppResult<i64> {
    // Quick notes don't count: everyone may have them.
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM vaults WHERE kind = 'regular'")
            .fetch_one(db)
            .await?,
    )
}

/// Bytes the database uses: its pages minus the free ones (deleting data
/// frees pages without shrinking the file).
pub async fn used_bytes(db: &crate::db::Db) -> AppResult<u64> {
    let pages: i64 = sqlx::query_scalar("PRAGMA page_count")
        .fetch_one(db)
        .await?;
    let free: i64 = sqlx::query_scalar("PRAGMA freelist_count")
        .fetch_one(db)
        .await?;
    let size: i64 = sqlx::query_scalar("PRAGMA page_size").fetch_one(db).await?;
    Ok((pages - free).max(0) as u64 * size.max(0) as u64)
}
