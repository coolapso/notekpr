//! notekpr headless synchronization server.
//!
//! The server coordinates; clients own the data and the keys. It authenticates
//! principals (OIDC users, agents), enforces vault roles, orders and relays
//! encrypted CRDT updates, stores encrypted snapshots and attachments, and
//! distributes encrypted key envelopes. It never holds a key that can decrypt
//! vault content.

pub mod access;
pub mod admin_clients;
pub mod apps;
pub mod auth;
pub mod blobs;
pub mod config;
pub mod db;
pub mod error;
pub mod export;
pub mod hub;
pub mod instance;
pub mod oidc;
pub mod process;
pub mod ratelimit;
pub mod repo;
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
    let state = AppState::new(db, config, metrics).await?;
    if !state.config.dev_mode {
        end_dev_sessions(&state).await?;
    }
    spawn_maintenance(state.clone());
    spawn_storage_check(state.clone());
    Ok((routes::router(state.clone()), state))
}

/// Sessions of accounts created in development mode stop working once the
/// server runs without it (e.g. a dev database reused for real use).
async fn end_dev_sessions(state: &AppState) -> anyhow::Result<()> {
    let n =
        repo::sessions::revoke_for_issuer(&state.db, notekpr_core::protocol::DEV_ISSUER, db::now())
            .await?;
    if n > 0 {
        tracing::warn!(
            sessions = n,
            "ended sessions of development-mode accounts (development mode is off)"
        );
    }
    Ok(())
}

/// Measure the database every few seconds, so a full one turns the server
/// read-only soon after it reaches its limit (and writable again after), and
/// count the full vaults for `/metrics`.
fn spawn_storage_check(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        loop {
            tick.tick().await;
            if let Err(e) = state.instance.refresh_storage(&state.db).await {
                tracing::warn!(error = %e, "could not measure the database");
            }
        }
    });
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
            let db = &state.db;
            let _ = repo::sessions::delete_expired_login_codes(db, now).await;
            let _ = repo::sessions::delete_expired_oidc_states(db, now).await;
            let _ = repo::sessions::delete_device_grants_expired_before(db, now - 86_400_000).await;
            let _ = repo::sessions::forget_device_grant_addresses(db, now).await;
            match sync::erase_expired(&state).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(n, "erased what was deleted for good and no longer kept"),
                Err(e) => tracing::warn!(error = %e, "could not erase what was deleted for good"),
            }
            let _ = repo::sessions::delete_ended_before(db, now - month).await;
        }
    });
}
