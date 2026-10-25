//! Usage counts for administrators (no content, names or ids).

use notekpr_core::protocol::AdminStats;

use crate::db::Db;
use crate::error::AppResult;

pub async fn admin_stats(db: &Db) -> AppResult<AdminStats> {
    let count =
        |q: &'static str| async move { sqlx::query_scalar::<_, i64>(q).fetch_one(db).await };
    Ok(AdminStats {
        users: count("SELECT COUNT(*) FROM users WHERE kind = 'human'").await?,
        agents: count("SELECT COUNT(*) FROM users WHERE kind = 'agent'").await?,
        vaults: count("SELECT COUNT(*) FROM vaults WHERE kind = 'regular'").await?,
        objects: count("SELECT COUNT(*) FROM objects WHERE purged_at IS NULL").await?,
        updates: count("SELECT COUNT(*) FROM updates").await?,
        snapshots: count("SELECT COUNT(*) FROM snapshots").await?,
        attachments: count("SELECT COUNT(*) FROM attachments").await?,
        storage_bytes: super::settings::used_bytes(db).await? as i64,
    })
}
