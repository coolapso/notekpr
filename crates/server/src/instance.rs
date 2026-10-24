//! Instance settings that change while the server runs (limits, read-only
//! mode), and whether the server accepts changes.
//!
//! The settings live in the database (`repo::settings`) and here in memory;
//! a change takes effect at once. Administrators change them through
//! `routes::admin`: people with the admin role (none on an `--adminless`
//! instance) and admin clients with the `settings` scope
//! (`admin_clients.rs`).
//!
//! Read-only mode has two causes, kept apart so that lifting one doesn't
//! lift the other: the administrators' switch (`InstanceSettings::read_only`)
//! and a full database (`max_storage_mb` reached; [`Instance::refresh_storage`]
//! measures it periodically and after each settings change). While either
//! holds, the routes that change something are refused with
//! [`AppError::ReadOnly`] (`routes::read_only_guard`, `sync::push`), and live
//! connections are told ([`Instance::subscribe`], `ws.rs`) so clients keep
//! their changes and send them the moment the server is writable again.
//!
//! Limits never take anything away: they are checked when something new
//! would be created (an account, a vault, an agent), inside the transaction
//! that creates it, so two requests at once can't both slip past them.

use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use notekpr_core::protocol::{InstanceSettings, InstanceSettingsUpdate, Limit, ReadOnly};
use sqlx::SqliteConnection;
use tokio::sync::watch;

use crate::db::{self, Db};
use crate::error::{AppError, AppResult};
use crate::repo;

pub struct Instance {
    settings: RwLock<InstanceSettings>,
    storage_full: AtomicBool,
    mode: watch::Sender<Option<ReadOnly>>,
}

impl Instance {
    pub async fn load(db: &Db) -> anyhow::Result<Instance> {
        let settings = repo::settings::load(db).await?;
        let instance = Instance {
            settings: RwLock::new(settings),
            storage_full: AtomicBool::new(false),
            mode: watch::Sender::new(None),
        };
        instance.update_mode();
        instance.refresh_storage(db).await?;
        Ok(instance)
    }

    pub fn settings(&self) -> InstanceSettings {
        self.settings.read().unwrap().clone()
    }

    /// Whether, and why, the server refuses changes now.
    pub fn read_only(&self) -> Option<ReadOnly> {
        *self.mode.borrow()
    }

    pub fn require_writable(&self) -> AppResult<()> {
        match self.read_only() {
            Some(why) => Err(AppError::ReadOnly(why)),
            None => Ok(()),
        }
    }

    /// Follow the read-only state (the current value first).
    pub fn subscribe(&self) -> watch::Receiver<Option<ReadOnly>> {
        self.mode.subscribe()
    }

    /// Store a change and apply it. The caller holds the write lock.
    pub async fn update(
        &self,
        db: &Db,
        change: &InstanceSettingsUpdate,
    ) -> AppResult<InstanceSettings> {
        let mut next = self.settings();
        next.apply(change);
        repo::settings::save(db, &next, db::now()).await?;
        *self.settings.write().unwrap() = next.clone();
        self.update_mode();
        self.refresh_storage(db).await?;
        Ok(next)
    }

    /// Measure the database and turn read-only on or off by its limit.
    /// Returns the bytes in use.
    pub async fn refresh_storage(&self, db: &Db) -> AppResult<u64> {
        let used = repo::settings::used_bytes(db).await?;
        let full = match self.settings().max_storage_mb {
            Limit::Unlimited => false,
            Limit::Max(mb) => used >= mb.saturating_mul(1024 * 1024),
        };
        if self.storage_full.swap(full, Ordering::Relaxed) != full {
            if full {
                tracing::warn!(
                    used_bytes = used,
                    "storage limit reached: the server is read-only"
                );
            } else {
                tracing::info!(used_bytes = used, "below the storage limit again");
            }
        }
        self.update_mode();
        metrics::gauge!("notekpr_storage_used_bytes").set(used as f64);
        Ok(used)
    }

    fn update_mode(&self) {
        let mode = if self.settings.read().unwrap().read_only {
            Some(ReadOnly::Manual)
        } else if self.storage_full.load(Ordering::Relaxed) {
            Some(ReadOnly::StorageFull)
        } else {
            None
        };
        self.mode.send_if_modified(|m| {
            let changed = *m != mode;
            *m = mode;
            changed
        });
        metrics::gauge!("notekpr_read_only").set(if mode.is_some() { 1.0 } else { 0.0 });
    }

    /// Refuse a new account (or enabling a disabled one) at the accounts
    /// limit. Runs inside the transaction that adds it.
    pub async fn check_new_person(&self, tx: &mut SqliteConnection) -> AppResult<()> {
        let limit = self.settings().max_users;
        let n = repo::settings::active_people(&mut *tx).await?;
        if !limit.allows_one_more(n as u64) {
            return Err(AppError::LimitReached(format!(
                "this server has reached its limit of {} accounts",
                limit.max().unwrap_or_default()
            )));
        }
        Ok(())
    }

    /// Refuse a vault `owner` would create beyond the server's or their own
    /// limit. Runs inside the creating transaction.
    pub async fn check_new_vault(&self, tx: &mut SqliteConnection, owner: &str) -> AppResult<()> {
        let s = self.settings();
        let total = repo::settings::vaults(&mut *tx).await?;
        if !s.max_vaults.allows_one_more(total as u64) {
            return Err(AppError::LimitReached(format!(
                "this server has reached its limit of {} vaults",
                s.max_vaults.max().unwrap_or_default()
            )));
        }
        let (allowance, _) = repo::users::allowances(&mut *tx, owner).await?;
        let mine = allowance.resolve(s.vaults_per_user);
        let owned = repo::users::owned_vaults(&mut *tx, owner).await?;
        if !mine.allows_one_more(owned as u64) {
            return Err(AppError::LimitReached(
                match mine.max().unwrap_or_default() {
                    0 => "you can't create vaults on this server".to_string(),
                    1 => "you can own 1 vault on this server".to_string(),
                    n => format!("you can own {n} vaults on this server"),
                },
            ));
        }
        Ok(())
    }

    /// Refuse an agent `owner` would create beyond the server's or their
    /// own limit. Runs inside the creating transaction.
    pub async fn check_new_agent(&self, tx: &mut SqliteConnection, owner: &str) -> AppResult<()> {
        let s = self.settings();
        let total = repo::settings::agents(&mut *tx).await?;
        if !s.max_agents.allows_one_more(total as u64) {
            return Err(AppError::LimitReached(format!(
                "this server has reached its limit of {} agents",
                s.max_agents.max().unwrap_or_default()
            )));
        }
        let (_, allowance) = repo::users::allowances(&mut *tx, owner).await?;
        let mine = allowance.resolve(s.agents_per_user);
        let owned = repo::agents::count(&mut *tx, owner).await?;
        if !mine.allows_one_more(owned as u64) {
            return Err(AppError::LimitReached(
                match mine.max().unwrap_or_default() {
                    0 => "you can't create agents on this server".to_string(),
                    1 => "you can have 1 agent on this server".to_string(),
                    n => format!("you can have {n} agents on this server"),
                },
            ));
        }
        Ok(())
    }
}
