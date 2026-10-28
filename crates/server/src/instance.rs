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
//! would be created (an account, a vault, an agent, a quick note), inside
//! the transaction that creates it, so two requests at once can't both slip
//! past them.
//!
//! Each vault has a storage limit too: its own, set by administrators, else
//! `storage_per_vault_mb`. Someone's quick notes have the person's instead
//! (`users.quick_notes_storage_allowance`, else `quick_notes_storage_mb`),
//! like how many they may keep. The database keeps each vault's size
//! (`vaults.storage_bytes`, see the triggers in the first migration). A
//! vault at its limit takes no new content — updates, snapshots, files,
//! undoing a deletion — ([`Instance::require_room`], [`AppError::VaultFull`])
//! until something in it is deleted for good or the limit is raised; the
//! rest (members, keys, deleting) goes on. Writes check it inside their
//! transaction, so a vault overshoots its limit by one write at most. Its
//! subscribers are told when it turns full or has room again
//! (`sync::storage_changed`).

use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use notekpr_core::protocol::{
    InstanceSettings, InstanceSettingsUpdate, Limit, MIB, ReadOnly, VaultKind,
};
use notekpr_core::version::Version;
use sqlx::{SqliteConnection, SqliteExecutor};
use tokio::sync::watch;

use crate::apps::OLDEST_APP_VERSION;
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

    /// The oldest version of the apps the server accepts: its build's, or
    /// the administrators' if higher.
    pub fn oldest_app(&self) -> Version {
        let set = Version::parse(&self.settings.read().unwrap().oldest_app_version);
        set.map_or(OLDEST_APP_VERSION, |v| v.max(OLDEST_APP_VERSION))
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
        if let Some(v) = change.oldest_app_version.as_deref().map(str::trim)
            && !v.is_empty()
            && Version::parse(v).is_none()
        {
            return Err(AppError::BadRequest(
                "oldest_app_version is a version like 0.6.0 (or empty)".into(),
            ));
        }
        let mut next = self.settings();
        next.apply(change);
        repo::settings::save(db, &next, db::now()).await?;
        *self.settings.write().unwrap() = next.clone();
        self.update_mode();
        self.refresh_storage(db).await?;
        Ok(next)
    }

    /// Measure the database and turn read-only on or off by its limit, and
    /// count the vaults at their own limits (for monitoring). Returns the
    /// bytes in use.
    pub async fn refresh_storage(&self, db: &Db) -> AppResult<u64> {
        let used = repo::settings::used_bytes(db).await?;
        let full = self.settings().max_storage_mb.reached_mb(used);
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
        let full_vaults = repo::vaults::full_count(db, &self.settings()).await?;
        metrics::gauge!("notekpr_vaults_full").set(full_vaults as f64);
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
        let allowance = repo::users::allowances(&mut *tx, owner).await?.vaults;
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
        let allowance = repo::users::allowances(&mut *tx, owner).await?.agents;
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

    /// The quick notes `owner` may keep: their own allowance, else the
    /// instance's default.
    pub async fn quick_notes_limit(
        &self,
        db: impl SqliteExecutor<'_>,
        owner: &str,
    ) -> AppResult<Limit> {
        let allowance = repo::users::allowances(db, owner).await?.quick_notes;
        Ok(allowance.resolve(self.settings().quick_notes_per_user))
    }

    /// Whether a vault reached its storage limit (`false` for no such vault):
    /// its own, else the instance's for its kind.
    pub async fn vault_full(&self, db: impl SqliteExecutor<'_>, vault_id: &str) -> AppResult<bool> {
        let settings = self.settings();
        Ok(repo::vaults::storage(db, vault_id).await?.is_some_and(|s| {
            s.allowance
                .resolve(settings.storage_default(s.kind))
                .reached_mb(s.bytes)
        }))
    }

    /// Refuse new content in a vault at its storage limit. Runs inside the
    /// writing transaction (or under the write lock).
    pub async fn require_room(&self, db: impl SqliteExecutor<'_>, vault_id: &str) -> AppResult<()> {
        if self.vault_full(db, vault_id).await? {
            return Err(AppError::VaultFull);
        }
        Ok(())
    }

    /// The vaults on the instance's storage limits (per vault, for quick
    /// notes) that a change from `old` settings to the current ones fills or
    /// frees, with whether they are full now.
    pub async fn storage_flips(
        &self,
        db: &Db,
        old: &InstanceSettings,
    ) -> AppResult<Vec<(String, bool)>> {
        let current = self.settings();
        let mut flips = Vec::new();
        for kind in [VaultKind::Regular, VaultKind::QuickNotes] {
            let (old, new) = (old.storage_default(kind), current.storage_default(kind));
            if old == new {
                continue;
            }
            let from = [old, new]
                .iter()
                .filter_map(|l| l.max())
                .min()
                .map_or(u64::MAX, |mb| mb.saturating_mul(MIB));
            flips.extend(
                repo::vaults::on_default_storage_from(db, kind, from)
                    .await?
                    .into_iter()
                    .filter(|(_, bytes)| old.reached_mb(*bytes) != new.reached_mb(*bytes))
                    .map(|(id, bytes)| (id, new.reached_mb(bytes))),
            );
        }
        Ok(flips)
    }

    /// For someone's quick notes: how many notes they may hold and how many
    /// they hold now. `None` for every other vault (no limit on notes).
    /// Runs inside the transaction that would add a note.
    pub async fn quick_note_room(
        &self,
        tx: &mut SqliteConnection,
        vault_id: &str,
    ) -> AppResult<Option<(Limit, u64)>> {
        let Some((VaultKind::QuickNotes, owner)) =
            repo::vaults::kind_and_owner(&mut *tx, vault_id).await?
        else {
            return Ok(None);
        };
        let limit = self.quick_notes_limit(&mut *tx, &owner).await?;
        let count = repo::vaults::note_count(&mut *tx, vault_id).await?;
        Ok(Some((limit, count)))
    }

    /// Refuse a new note (`object_id` not stored yet) in quick notes that
    /// hold as many as they may. Runs inside the creating transaction.
    pub async fn check_new_note(
        &self,
        tx: &mut SqliteConnection,
        vault_id: &str,
        object_id: &str,
    ) -> AppResult<()> {
        if repo::shares::object_state(&mut *tx, vault_id, object_id)
            .await?
            .is_some()
        {
            return Ok(());
        }
        match self.quick_note_room(tx, vault_id).await? {
            Some((limit, count)) if !limit.allows_one_more(count) => Err(AppError::LimitReached(
                notekpr_core::quick::full_message(limit),
            )),
            _ => Ok(()),
        }
    }
}
