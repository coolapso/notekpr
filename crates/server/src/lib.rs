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
pub mod writer;
pub mod ws;

use std::time::{Duration, Instant};

use metrics_exporter_prometheus::PrometheusHandle;

use crate::config::Config;
use crate::state::AppState;
use crate::writer::Role;

/// Open the database, apply migrations and build the application.
///
/// If another server writes to the database (the version this one
/// replaces, `writer.rs`), this one serves reads and takes over writing
/// when that one stops.
pub async fn build(
    config: Config,
    metrics: Option<PrometheusHandle>,
) -> anyhow::Result<(axum::Router, AppState)> {
    let path = config.db_path();
    let db = db::connect(&path).await?;
    db::migrate(&db, &path).await?;
    let state = AppState::new(db, config, metrics).await?;
    if state.write_lock.role() == Role::Writer {
        prepare(&state).await?;
        spawn_background(&state);
    } else {
        tracing::info!(
            "another server writes to this database: serving reads, and taking over writing when it stops"
        );
        tokio::spawn(take_over(state.clone()));
    }
    Ok((routes::router(state.clone()), state))
}

/// What a server writes before anything else, holding the writer lease.
async fn prepare(state: &AppState) -> anyhow::Result<()> {
    if !state.config.dev_mode {
        end_dev_sessions(state).await?;
    }
    Ok(())
}

/// What a server does in the background while it writes.
fn spawn_background(state: &AppState) {
    spawn_maintenance(state.clone());
    spawn_storage_check(state.clone());
}

/// Wait for the server writing to the database to hand over, then write.
async fn take_over(state: AppState) {
    let waited = Instant::now();
    if !state.write_lock.acquire().await {
        return;
    }
    // Read at start, and an administrator may have changed them on the
    // other server meanwhile.
    if let Err(e) = state.instance.reload(&state.db).await {
        tracing::error!(error = %e, "could not read the instance settings again: using those read at start");
    }
    if let Err(e) = prepare(&state).await {
        tracing::error!(error = %e, "could not end the sessions of development mode");
    }
    if !state.write_lock.start_writing() {
        return;
    }
    tracing::info!(
        waited_ms = waited.elapsed().as_millis() as u64,
        "took over writing to the database"
    );
    spawn_background(&state);
}

/// [`build`] for tests, whose clients go faster than people: rate limits
/// they don't reach and no snapshot cooldown, set as an administrator
/// would. A test changes them further with `state.instance.update`.
pub async fn build_for_tests(config: Config) -> anyhow::Result<(axum::Router, AppState)> {
    let (app, state) = build(config, None).await?;
    let change = notekpr_core::protocol::InstanceSettingsUpdate {
        rate_limit_rps: Some(10_000),
        auth_rate_limit_per_min: Some(10_000),
        snapshot_cooldown_secs: Some(0),
        ..Default::default()
    };
    state.instance.update(&state.db, &change).await?;
    Ok((app, state))
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

/// Measure the database every few seconds, so that every vault is full
/// soon after it reaches the server's limit (and has room again after), and
/// count the full vaults for `/metrics`. While this server writes.
fn spawn_storage_check(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        loop {
            tick.tick().await;
            if !state.write_lock.is_writer() {
                return;
            }
            if let Err(e) = sync::check_storage(&state).await {
                tracing::warn!(error = %e, "could not measure the database");
            }
        }
    });
}

/// Periodic cleanup of expired short-lived records and rate-limiter state,
/// while this server writes. Handing over waits for a round, a few seconds
/// at most (`writer.rs`); a longer one may overlap the next server's first,
/// which is safe: each step is its own transaction.
fn spawn_maintenance(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(300));
        loop {
            tick.tick().await;
            let Some(_working) = state.write_lock.work() else {
                return;
            };
            state.instance.limiters().cleanup();
            let now = db::now();
            let month = 30 * 24 * 3600 * 1000;
            let db = &state.db;
            let _ = repo::sessions::delete_expired_login_codes(db, now).await;
            let _ = repo::sessions::delete_expired_oidc_states(db, now).await;
            let _ = repo::sessions::delete_device_grants_expired_before(db, now - 86_400_000).await;
            let _ = repo::sessions::forget_device_grant_addresses(db, now).await;
            let _ = repo::invitations::expire(db, now).await;
            match sync::erase_expired(&state).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(n, "erased what was deleted for good and no longer kept"),
                Err(e) => tracing::warn!(error = %e, "could not erase what was deleted for good"),
            }
            // Attachments kept in the database before a file store was set
            // up (the first tick is at once), and files the store is to
            // delete.
            if let Err(e) = state.blobs.move_out().await {
                tracing::warn!(error = %e, "could not move attachments to the file store");
            }
            let keep = state.instance.settings().keep_deleted_files_ms();
            match state.blobs.delete_due(db::now(), keep).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(n, "deleted files from the file store"),
                Err(e) => tracing::warn!(error = %e, "could not delete files from the file store"),
            }
            let _ = repo::sessions::delete_ended_before(db, now - month).await;
        }
    });
}
