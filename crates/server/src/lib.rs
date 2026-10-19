//! coolpsonotes headless synchronization server.
//!
//! The server coordinates; clients own the data and the keys. It authenticates
//! principals (OIDC users, agents), enforces vault roles, orders and relays
//! encrypted CRDT updates, stores encrypted snapshots and attachments, and
//! distributes encrypted key envelopes. It never holds a key that can decrypt
//! vault content.

pub mod access;
pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod export;
pub mod hub;
pub mod oidc;
pub mod ratelimit;
pub mod routes;
pub mod state;
pub mod sync;
pub mod ws;

use std::time::Duration;

use metrics_exporter_prometheus::PrometheusHandle;

use crate::config::Config;
use crate::state::AppState;

/// Open the database, apply migrations and build the application.
pub async fn build(
    config: Config,
    metrics: Option<PrometheusHandle>,
) -> anyhow::Result<(axum::Router, AppState)> {
    let path = config.db_path();
    let db = db::connect(&path).await?;
    db::migrate(&db, &path).await?;
    let state = AppState::new(db, config, metrics)?;
    if !state.config.dev_mode {
        end_dev_sessions(&state).await?;
    }
    spawn_maintenance(state.clone());
    Ok((routes::router(state.clone()), state))
}

/// Sessions of accounts created in development mode stop working once the
/// server runs without it (e.g. a dev database reused for real use).
async fn end_dev_sessions(state: &AppState) -> anyhow::Result<()> {
    let n = sqlx::query(
        "UPDATE sessions SET revoked_at = ? WHERE revoked_at IS NULL AND user_id IN
           (SELECT user_id FROM identities WHERE issuer = ?)",
    )
    .bind(db::now())
    .bind(coolpsonotes_core::protocol::DEV_ISSUER)
    .execute(&state.db)
    .await?
    .rows_affected();
    if n > 0 {
        tracing::warn!(
            sessions = n,
            "ended sessions of development-mode accounts (development mode is off)"
        );
    }
    Ok(())
}

/// Periodic cleanup of expired short-lived records and rate-limiter state.
fn spawn_maintenance(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(300));
        loop {
            tick.tick().await;
            state.limiters.cleanup();
            let now = db::now();
            let month = 30 * 24 * 3600 * 1000;
            for q in [
                "DELETE FROM login_codes WHERE expires_at < ?",
                "DELETE FROM oidc_states WHERE expires_at < ?",
            ] {
                let _ = sqlx::query(q).bind(now).execute(&state.db).await;
            }
            let _ = sqlx::query("DELETE FROM device_grants WHERE expires_at < ?")
                .bind(now - 86_400_000)
                .execute(&state.db)
                .await;
            let _ = sqlx::query("DELETE FROM sessions WHERE expires_at < ? OR revoked_at < ?")
                .bind(now - month)
                .bind(now - month)
                .execute(&state.db)
                .await;
        }
    });
}
