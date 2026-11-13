//! One server writes to a database at a time: the one holding the *writer
//! lease*, a lock on a file next to the database (`notekpr.db-writer`).
//!
//! What the rest of the server builds on stays true while an update runs
//! the next version next to this one, on the same database
//! (docs/operations.md, "Updates without downtime"): writes are serialised
//! in one process ([`WriteLock::lock`], the write lock of `state.rs`), and
//! that process publishes every live event, so each vault's stream stays
//! gapless and in order (`hub.rs`).
//!
//! **Waiting.** A server that starts while another holds the lease serves
//! the web app and reads. Requests that may change something and live
//! connections wait for the lease ([`WriteLock::admit`],
//! [`WriteLock::writing`]), at most [`HOLD`], and are then told to try again
//! ([`AppError::Restarting`]: HTTP 503 with `Retry-After`, or the WebSocket
//! closed with `CLOSE_RESTARTING`). Writing in the background starts once
//! it holds the lease. When it gets the lease, it reads again what the
//! other server may have changed meanwhile (`lib.rs`, `take_over`), then
//! writes ([`WriteLock::start_writing`]).
//!
//! **Handing over.** The server holding the lease gives it up when it stops
//! ([`WriteLock::hand_off`]): it admits no new changes, lets the requests
//! under way and the background work finish (at most [`FINISH_WAIT`]) and
//! the write in progress, then releases the lease; the next server takes it
//! within [`POLL`]. A request still under way then is refused when it
//! takes the write lock, before writing. Whatever a live connection must
//! hear about — the log, and access ending: sessions, vaults, notes — is
//! written under the write lock, so it lands before the next server
//! writes, and that one's connections, all newer, read it from the
//! database. Writes without the lock tell connections nothing they need (a
//! hint to refresh at most), and the background work's steps are each a
//! transaction of their own.
//!
//! While it waits, a server also holds a second lock (`notekpr.db-next`),
//! so the one stopping knows another takes over
//! ([`WriteLock::successor_waiting`]) and keeps answering reads for a moment
//! while proxies move to the next one (`main.rs`).
//!
//! The locks are `flock`s, which the kernel releases when the process ends,
//! however it ends. They hold between processes on one machine, containers
//! sharing a volume included — which is also what SQLite's WAL needs; not
//! over a network file system.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, MutexGuard, OwnedRwLockReadGuard, RwLock, watch};

use crate::error::{AppError, AppResult};

/// How long a change or a live connection waits for this server to write,
/// before it is told to try again.
pub const HOLD: Duration = Duration::from_secs(10);
/// How long handing over waits for the changes under way.
pub const FINISH_WAIT: Duration = Duration::from_secs(5);
/// How often a waiting server tries to take the lease.
pub const POLL: Duration = Duration::from_millis(20);

/// Where this server stands with the lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Another server holds the lease: this one takes it when it can.
    Waiting,
    /// This server writes.
    Writer,
    /// Handing over: no new changes; those under way finish.
    Leaving,
    /// Writes nothing (it handed over, or stopped before its turn).
    HandedOff,
}

/// The write lock of a server that may share its database with the next
/// version of itself. See the module docs.
pub struct WriteLock {
    role: watch::Sender<Role>,
    /// One write at a time in this process.
    serial: Mutex<()>,
    /// Requests that may change something hold it for reading while they
    /// run ([`Admitted`]); handing over takes it for writing to wait for
    /// them.
    under_way: Arc<RwLock<()>>,
    files: std::sync::Mutex<Files>,
    writer_path: PathBuf,
    next_path: PathBuf,
    hold: Duration,
}

/// The lock files this server holds.
#[derive(Default)]
struct Files {
    /// The lease, while writing.
    writer: Option<File>,
    /// While waiting for the lease.
    next: Option<File>,
}

/// A request admitted to change something: handing over waits for it.
pub struct Admitted(#[allow(dead_code)] OwnedRwLockReadGuard<()>);

impl WriteLock {
    /// The write lock of the database at `db`: this server writes if it
    /// can take the lease now, else it waits for it ([`Self::acquire`]).
    pub fn open(db: &Path) -> anyhow::Result<WriteLock> {
        Self::open_with(db, HOLD)
    }

    /// [`Self::open`], holding changes for `hold` while waiting.
    pub fn open_with(db: &Path, hold: Duration) -> anyhow::Result<WriteLock> {
        let writer_path = sidecar(db, "writer");
        let next_path = sidecar(db, "next");
        let writer = open_lock_file(&writer_path)?;
        let (role, files) = match writer.try_lock() {
            Ok(()) => (
                Role::Writer,
                Files {
                    writer: Some(writer),
                    ..Default::default()
                },
            ),
            Err(TryLockError::WouldBlock) => {
                // Taken if nobody else waits too (then one of us takes
                // over now, and the other later).
                let next = open_lock_file(&next_path)?;
                let next = next.try_lock().is_ok().then_some(next);
                (Role::Waiting, Files { writer: None, next })
            }
            Err(TryLockError::Error(e)) => {
                // A file system without locks: one server per database, as
                // before there were leases.
                tracing::warn!(
                    error = %e,
                    "cannot lock {}: this server writes without making sure it's the only one; run one server per database, and no update next to it",
                    writer_path.display()
                );
                (Role::Writer, Files::default())
            }
        };
        Ok(WriteLock {
            role: watch::Sender::new(role),
            serial: Mutex::new(()),
            under_way: Arc::new(RwLock::new(())),
            files: std::sync::Mutex::new(files),
            writer_path,
            next_path,
            hold,
        })
    }

    pub fn role(&self) -> Role {
        *self.role.borrow()
    }

    /// Whether this server writes and isn't handing over.
    pub fn is_writer(&self) -> bool {
        self.role() == Role::Writer
    }

    /// Whether this server is handing over, or has.
    pub fn stopping(&self) -> bool {
        matches!(self.role(), Role::Leaving | Role::HandedOff)
    }

    /// Wait (at most the hold) until a role `ok` accepts, or one that will
    /// never change into it.
    async fn wait_for(&self, ok: impl Fn(Role) -> bool) -> AppResult<Role> {
        let mut rx = self.role.subscribe();
        let wait = rx.wait_for(|r| *r != Role::Waiting);
        match tokio::time::timeout(self.hold, wait).await {
            Ok(Ok(r)) if ok(*r) => Ok(*r),
            _ => Err(AppError::Restarting),
        }
    }

    /// For a live connection: wait (at most the hold) until this server
    /// writes; `false` if it won't, or not soon.
    pub async fn writing(&self) -> bool {
        self.wait_for(|r| r == Role::Writer).await.is_ok()
    }

    /// Admit a request that may change something: once this server writes
    /// (waiting at most the hold), and not while it hands over.
    pub async fn admit(&self) -> AppResult<Admitted> {
        self.wait_for(|r| r == Role::Writer).await?;
        let guard = self.under_way.clone().read_owned().await;
        // Handing over may have begun meanwhile.
        if !self.is_writer() {
            return Err(AppError::Restarting);
        }
        Ok(Admitted(guard))
    }

    /// For work in the background while this server writes: like
    /// [`Self::admit`], without waiting. `None` once it hands over.
    pub fn work(&self) -> Option<Admitted> {
        if !self.is_writer() {
            return None;
        }
        let guard = self.under_way.clone().try_read_owned().ok()?;
        self.is_writer().then_some(Admitted(guard))
    }

    /// The write lock: one write at a time, and only on the server holding
    /// the lease. While it hands over, what is under way still writes.
    pub async fn lock(&self) -> AppResult<MutexGuard<'_, ()>> {
        let may = |r| matches!(r, Role::Writer | Role::Leaving);
        self.wait_for(may).await?;
        let guard = self.serial.lock().await;
        if !may(self.role()) {
            return Err(AppError::Restarting);
        }
        Ok(guard)
    }

    /// Wait until the lease is free and take it (a server waiting for its
    /// turn). `true`: this server holds it now, still [`Role::Waiting`]:
    /// the caller reads again what it needs, then
    /// [`Self::start_writing`]. `false`: it stopped meanwhile.
    pub async fn acquire(&self) -> bool {
        loop {
            match self.try_acquire() {
                Some(taken) => return taken,
                None => tokio::time::sleep(POLL).await,
            }
        }
    }

    /// One try: `Some(true)` taken, `Some(false)` never will be, `None` not
    /// yet.
    fn try_acquire(&self) -> Option<bool> {
        if self.role() != Role::Waiting {
            return Some(false);
        }
        let mut files = self.files.lock().unwrap();
        if files.writer.is_some() {
            return Some(true);
        }
        if files.next.is_none()
            && let Ok(f) = open_lock_file(&self.next_path)
            && f.try_lock().is_ok()
        {
            // The server that waited before this one took over meanwhile.
            files.next = Some(f);
        }
        let path = &self.writer_path;
        let file = match open_lock_file(path) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(error = %e, "cannot open {}", path.display());
                return None;
            }
        };
        match file.try_lock() {
            Ok(()) => {
                files.writer = Some(file);
                Some(true)
            }
            Err(TryLockError::WouldBlock) => None,
            Err(TryLockError::Error(e)) => {
                tracing::warn!(error = %e, "cannot lock {}", path.display());
                None
            }
        }
    }

    /// After [`Self::acquire`]: this server writes now. `false` if it
    /// began stopping meanwhile (the lease is given up again).
    pub fn start_writing(&self) -> bool {
        let started = self.role.send_if_modified(|r| {
            let waiting = *r == Role::Waiting;
            if waiting {
                *r = Role::Writer;
            }
            waiting
        });
        let mut files = self.files.lock().unwrap();
        files.next = None;
        if !started {
            files.writer = None;
        }
        started
    }

    /// Whether another server waits to take over from this one.
    pub fn successor_waiting(&self) -> bool {
        if self.files.lock().unwrap().next.is_some() {
            // This server is the one waiting.
            return false;
        }
        match open_lock_file(&self.next_path) {
            Ok(f) => matches!(f.try_lock_shared(), Err(TryLockError::WouldBlock)),
            Err(_) => false,
        }
    }

    /// Stop writing and give up the lease: no new changes are admitted,
    /// those under way finish (at most [`FINISH_WAIT`]), and the write in
    /// progress; then the next server takes over.
    pub async fn hand_off(&self) {
        let was = self.role.send_replace(Role::Leaving);
        if was != Role::Writer {
            self.role.send_replace(Role::HandedOff);
            self.release();
            return;
        }
        match tokio::time::timeout(FINISH_WAIT, self.under_way.write()).await {
            Ok(done) => drop(done),
            Err(_) => tracing::warn!(
                "handing over with changes still under way: they're told to try again"
            ),
        }
        self.role.send_replace(Role::HandedOff);
        // The write in progress finishes; those waiting for it give up
        // (`lock`).
        drop(self.serial.lock().await);
        self.release();
    }

    fn release(&self) {
        let mut files = self.files.lock().unwrap();
        files.writer = None;
        files.next = None;
    }
}

/// `notekpr.db` → `notekpr.db-<what>`, next to SQLite's `-wal` and `-shm`.
fn sidecar(db: &Path, what: &str) -> PathBuf {
    let mut name = db.file_name().unwrap_or_default().to_os_string();
    name.push(format!("-{what}"));
    db.with_file_name(name)
}

fn open_lock_file(path: &Path) -> anyhow::Result<File> {
    use anyhow::Context;
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("notekpr.db")
    }

    fn short(dir: &tempfile::TempDir) -> WriteLock {
        WriteLock::open_with(&db(dir), Duration::from_millis(200)).unwrap()
    }

    #[tokio::test]
    async fn one_server_writes_and_the_next_takes_over_when_it_hands_off() {
        let dir = tempfile::tempdir().unwrap();
        let old = short(&dir);
        assert_eq!(old.role(), Role::Writer);
        let new = short(&dir);
        assert_eq!(new.role(), Role::Waiting);
        assert!(old.successor_waiting());
        assert!(!new.successor_waiting(), "it is the one waiting");

        // The next server holds changes, then says to try again.
        assert!(matches!(new.admit().await, Err(AppError::Restarting)));
        assert!(matches!(new.lock().await, Err(AppError::Restarting)));
        assert!(!new.writing().await);
        drop(old.lock().await.unwrap());

        let taking = async {
            assert!(new.acquire().await);
            assert!(new.start_writing());
        };
        tokio::join!(taking, old.hand_off());
        assert_eq!(new.role(), Role::Writer);
        assert_eq!(old.role(), Role::HandedOff);
        assert!(old.stopping() && !new.stopping());
        drop(new.admit().await.unwrap());
        drop(new.lock().await.unwrap());
        // The one that handed over refuses at once.
        assert!(matches!(old.admit().await, Err(AppError::Restarting)));
        assert!(matches!(old.lock().await, Err(AppError::Restarting)));
        assert!(!new.successor_waiting());
    }

    #[tokio::test]
    async fn held_changes_go_through_once_the_server_writes() {
        let dir = tempfile::tempdir().unwrap();
        let old = WriteLock::open(&db(&dir)).unwrap();
        let new = Arc::new(WriteLock::open(&db(&dir)).unwrap());
        let held = tokio::spawn({
            let new = new.clone();
            async move { new.admit().await.is_ok() && new.lock().await.is_ok() }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!held.is_finished(), "held while the other server writes");
        old.hand_off().await;
        assert!(new.acquire().await);
        assert!(new.start_writing());
        assert!(held.await.unwrap());
    }

    #[tokio::test]
    async fn handing_off_waits_for_the_changes_under_way() {
        let dir = tempfile::tempdir().unwrap();
        let old = Arc::new(short(&dir));
        let new = short(&dir);
        let admitted = old.admit().await.unwrap();
        let handing = tokio::spawn({
            let old = old.clone();
            async move { old.hand_off().await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(old.role(), Role::Leaving);
        // Nothing new is admitted, but what is under way still writes.
        assert!(matches!(old.admit().await, Err(AppError::Restarting)));
        let w = old.lock().await.unwrap();
        assert!(new.try_acquire().is_none(), "the lease is still held");
        drop(admitted);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !handing.is_finished(),
            "the write in progress finishes first"
        );
        assert!(new.try_acquire().is_none());
        drop(w);
        handing.await.unwrap();
        assert_eq!(new.try_acquire(), Some(true));
    }

    #[tokio::test]
    async fn work_in_the_background_runs_only_while_writing_and_is_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let old = Arc::new(short(&dir));
        let new = short(&dir);
        assert!(new.work().is_none(), "not while waiting");
        let working = old.work().unwrap();
        let handing = tokio::spawn({
            let old = old.clone();
            async move { old.hand_off().await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(old.work().is_none(), "no new work while handing over");
        assert!(new.try_acquire().is_none(), "the round finishes first");
        drop(working);
        handing.await.unwrap();
        assert_eq!(new.try_acquire(), Some(true));
        assert!(new.start_writing());
        assert!(new.work().is_some());
    }

    #[tokio::test]
    async fn a_server_stopping_before_its_turn_gives_up_its_place() {
        let dir = tempfile::tempdir().unwrap();
        let old = short(&dir);
        let new = short(&dir);
        new.hand_off().await;
        assert_eq!(new.role(), Role::HandedOff);
        assert!(!old.successor_waiting());
        assert!(!new.acquire().await);
        // A third one waits in its place.
        let third = short(&dir);
        assert!(old.successor_waiting());
        old.hand_off().await;
        assert!(third.acquire().await);
        assert!(third.start_writing());
    }

    #[tokio::test]
    async fn a_third_server_waits_its_turn_too() {
        let dir = tempfile::tempdir().unwrap();
        let first = short(&dir);
        let second = short(&dir);
        let third = short(&dir);
        first.hand_off().await;
        // One of the two takes over; the other keeps waiting.
        let (a, b) = (second.try_acquire(), third.try_acquire());
        assert!(matches!((a, b), (Some(true), None) | (None, Some(true))));
        let (taker, waiter) = if a == Some(true) {
            (&second, &third)
        } else {
            (&third, &second)
        };
        assert!(taker.start_writing());
        // The other takes its place in line at its next try.
        assert_eq!(waiter.try_acquire(), None);
        assert!(taker.successor_waiting());
        taker.hand_off().await;
        assert!(waiter.acquire().await);
        assert!(waiter.start_writing());
    }
}
