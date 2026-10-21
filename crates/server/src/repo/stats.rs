//! Usage counts for administrators (no content, names or ids).

use notekpr_core::protocol::AdminStats;

use crate::db::Db;
use crate::error::AppResult;

pub async fn admin_stats(db: &Db) -> AppResult<AdminStats> {
    let count =
        |q: &'static str| async move { sqlx::query_scalar::<_, i64>(q).fetch_one(db).await };
    let page_count: i64 = sqlx::query_scalar("PRAGMA page_count")
        .fetch_one(db)
        .await
        .unwrap_or(0);
    let page_size: i64 = sqlx::query_scalar("PRAGMA page_size")
        .fetch_one(db)
        .await
        .unwrap_or(0);
    Ok(AdminStats {
        users: count("SELECT COUNT(*) FROM users WHERE kind = 'human'").await?,
        agents: count("SELECT COUNT(*) FROM users WHERE kind = 'agent'").await?,
        vaults: count("SELECT COUNT(*) FROM vaults").await?,
        objects: count("SELECT COUNT(*) FROM objects WHERE purged_at IS NULL").await?,
        updates: count("SELECT COUNT(*) FROM updates").await?,
        snapshots: count("SELECT COUNT(*) FROM snapshots").await?,
        attachments: count("SELECT COUNT(*) FROM attachments").await?,
        storage_bytes: page_count * page_size,
    })
}
