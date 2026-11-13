//! The Markdown folder: the CLI's own sync and mirror (`notekpr sync
//! --dir`), run inside the app with a CLI profile of the app's
//! ([`crate::paths::Paths::folder`]) — never one of the person's CLI
//! profiles. It uses this computer's device and the window's session,
//! handed over once ([`notekpr_cli::account::adopt`]), and keeps running
//! while the window is hidden; at the next start it runs from what it kept.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use notekpr_cli::account::{self, Adopt};
use notekpr_cli::api::{Api, Credentials};
use notekpr_cli::hub::{Hub, HubOptions};
use notekpr_cli::mirror::{Mirror, MirrorOptions};
use notekpr_cli::profile::{Dirs, ProfileLock, ProfileStore};
use notekpr_cli::session::Session;
use notekpr_cli::{livesync, setup};
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;

use crate::status::{FolderStatus, Updates};

/// The name of the folder's CLI profile (in the app's own profile root).
pub const PROFILE: &str = "folder";

/// How long stopping may take: what is waiting goes up, the editing
/// session's revision is kept, the local copies are saved.
const STOP_WITHIN: Duration = Duration::from_secs(15);

struct Running {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

pub struct Folder {
    dirs: Dirs,
    updates: Arc<Updates>,
    running: Mutex<Option<Running>>,
}

/// What a folder may become a Markdown folder: refused folders say why.
#[derive(serde::Serialize, Debug, Default, PartialEq, Eq)]
pub struct Check {
    /// Why it can't be used (empty: it can).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    /// A cloud drive syncs it (Dropbox, …): asked before going on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloud: Option<String>,
}

impl Folder {
    pub fn new(dirs: Dirs, updates: Arc<Updates>) -> Folder {
        Folder {
            dirs,
            updates,
            running: Mutex::new(None),
        }
    }

    fn store(&self) -> Result<ProfileStore> {
        ProfileStore::new(&self.dirs, PROFILE)
    }

    /// The folder this app keeps in sync, when it is set up and signed in.
    pub fn configured(&self) -> Option<PathBuf> {
        let p = self.store().ok()?.load().ok()??;
        p.session_token.as_ref()?;
        p.user_id.as_ref()?;
        p.sync.mirror_dir
    }

    /// The folder this app keeps — signed in or not: what forgetting
    /// would take away, and so asks about.
    pub fn kept(&self) -> Option<PathBuf> {
        self.store().ok()?.load().ok()??.sync.mirror_dir
    }

    /// Whether `dir` can become the Markdown folder. `cli` holds the
    /// person's own CLI profiles: a folder one of them mirrors is theirs.
    pub fn check(&self, dir: &Path, cli: Option<&Dirs>) -> Check {
        match self.refusal(dir, cli) {
            Err(e) => Check {
                refused: Some(format!("{e:#}")),
                cloud: None,
            },
            Ok(()) => Check {
                refused: None,
                cloud: setup::cloud_synced(dir).map(str::to_string),
            },
        }
    }

    fn refusal(&self, dir: &Path, cli: Option<&Dirs>) -> Result<()> {
        if !dir.is_absolute() {
            bail!("choose a folder by its full path");
        }
        setup::check_folder(dir, &self.dirs)?;
        if let Some(cli) = cli {
            setup::check_folder(dir, cli)?;
            let dir = notekpr_cli::mirror::resolve_lenient(dir);
            for name in cli.profiles() {
                let Ok(Some(p)) = ProfileStore::new(cli, &name).and_then(|s| s.load()) else {
                    continue;
                };
                if let Some(theirs) = p.sync.mirror_dir {
                    let theirs = notekpr_cli::mirror::resolve_lenient(&theirs);
                    if theirs.starts_with(&dir) || dir.starts_with(&theirs) {
                        bail!(
                            "the CLI already keeps {} in sync (profile '{name}'): choose another folder, \
                             or stop that sync first (`notekpr --profile {name} sync --no-dir`)",
                            theirs.display()
                        );
                    }
                }
            }
        }
        if holds_links(dir) {
            bail!(
                "{} holds links or special files: choose an empty folder",
                setup::shown(dir)
            );
        }
        let c = setup::inspect(dir)?;
        if c.notes + c.files + c.folders > 0 {
            bail!(
                "{} already holds files: choose an empty folder (or one notekpr kept before). To bring \
                 notes in, import them in the app, or use `notekpr setup`",
                dir.display()
            );
        }
        Ok(())
    }

    /// Hand over the window's device and session (see the module docs),
    /// remember `dir` and start keeping it in sync.
    pub async fn adopt_and_start(&self, a: Adopt, dir: PathBuf) -> Result<()> {
        let mut running = self.running.lock().await;
        self.stop_locked(&mut running).await;
        // Another account (or server) than the folder had: start afresh.
        if let Some(p) = self.store()?.load()?
            && p.user_id.is_some()
            && (p.user_id.as_deref() != Some(a.user_id.as_str())
                || notekpr_cli::profile::normalize_server_url(&a.server, false)
                    .ok()
                    .as_deref()
                    != Some(p.server.as_str()))
        {
            account::forget_locally(&self.dirs, PROFILE)?;
        }
        account::adopt(&self.dirs, PROFILE, a).await?;
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        self.store()?.update(|p| {
            p.sync.confirm_plaintext(&dir);
            p.sync.mirror_dir = Some(dir.clone());
        })?;
        self.start_locked(&mut running)
    }

    /// Start keeping the remembered folder in sync (at launch, again after
    /// a new session, or after it stopped on its own). Nothing to do when
    /// there is none, or it runs.
    pub async fn start(&self) -> Result<()> {
        let mut running = self.running.lock().await;
        self.start_locked(&mut running)
    }

    // One change at a time: every operation holds `running` throughout.
    fn start_locked(&self, running: &mut Option<Running>) -> Result<()> {
        let Some(dir) = self.configured() else {
            return Ok(());
        };
        // Still running: nothing to do. One that stopped on its own (its
        // error shown) starts again.
        if running.as_ref().is_some_and(|r| !r.task.is_finished()) {
            return Ok(());
        }
        // A remembered folder that is gone is never re-created: every note
        // in it would look deleted (the CLI's rule).
        if dir.symlink_metadata().is_err() {
            let error = format!(
                "{} is gone (moved, renamed or not mounted?). Put it back, or choose the folder again",
                setup::shown(&dir)
            );
            self.updates.update(|s| {
                s.folder = FolderStatus::Stopped {
                    dir: setup::shown(&dir),
                    error,
                }
            });
            return Ok(());
        }
        let lock = ProfileLock::try_acquire(&self.dirs, PROFILE)?
            .ok_or_else(|| anyhow!("the Markdown folder is already being kept in sync"))?;
        let (stop, stop_rx) = watch::channel(false);
        self.updates.update(|s| {
            s.folder = FolderStatus::Starting {
                dir: setup::shown(&dir),
            }
        });
        let dirs = self.dirs.clone();
        let updates = self.updates.clone();
        let task = tokio::spawn(async move {
            let result = run(&dirs, &dir, stop_rx, &updates).await;
            drop(lock);
            updates.update(|s| {
                s.folder = match result {
                    Ok(()) => FolderStatus::Off,
                    Err(e) => FolderStatus::Stopped {
                        dir: setup::shown(&dir),
                        error: format!("{e:#}"),
                    },
                }
            });
        });
        *running = Some(Running { stop, task });
        Ok(())
    }

    /// Stop syncing, the way `notekpr sync` stops: what is waiting goes up
    /// first (for a while).
    pub async fn stop(&self) {
        let mut running = self.running.lock().await;
        self.stop_locked(&mut running).await;
    }

    /// Returns once nothing of the sync runs any more — it stopped, or was
    /// cut short after [`STOP_WITHIN`] — so its profile may go.
    async fn stop_locked(&self, running: &mut Option<Running>) {
        let Some(mut r) = running.take() else {
            return;
        };
        let _ = r.stop.send(true);
        if tokio::time::timeout(STOP_WITHIN, &mut r.task)
            .await
            .is_err()
        {
            tracing::warn!("the Markdown folder's sync took too long to stop: cut short");
            r.task.abort();
            let _ = (&mut r.task).await;
        }
        self.updates.update(|s| s.folder = FolderStatus::Off);
    }

    /// The window's session, each time it opens: the folder goes on with
    /// it — if it is another, once the server says it is the same person's,
    /// on the same device.
    pub async fn session_changed(&self, token: String, expires_at: i64) -> Result<()> {
        let mut running = self.running.lock().await;
        let store = self.store()?;
        let Some(p) = store.load()? else {
            return Ok(());
        };
        let (Some(user), Some(device)) = (p.user_id.clone(), p.device_id.clone()) else {
            return Ok(());
        };
        if p.session_token.as_deref() == Some(token.as_str()) {
            return self.start_locked(&mut running);
        }
        let me = Api::new(&p.server, Credentials::Session(token.clone()))?
            .me()
            .await
            .context("checking the new session")?;
        if me.user.id != user || me.session.device_id.as_deref() != Some(device.as_str()) {
            bail!("the new session isn't this device's");
        }
        // Stopping pushes what is waiting with the old session, if it still
        // works; then the sync starts again with the new one.
        self.stop_locked(&mut running).await;
        store.update(|p| {
            p.session_token = Some(token);
            p.session_expires_at = Some(expires_at);
        })?;
        self.start_locked(&mut running)
    }

    /// The window signed out: the folder stops, and forgets the session
    /// once the server says it ended (its keys and copies stay, as after
    /// `notekpr logout`). A window's word alone only stops it until the
    /// window opens again.
    pub async fn signed_out(&self) -> Result<()> {
        let mut running = self.running.lock().await;
        self.stop_locked(&mut running).await;
        if self.session_ended().await == Some(true) {
            account::drop_session(&self.dirs, PROFILE)?;
        }
        Ok(())
    }

    /// Whether the server no longer accepts the folder's session: signed
    /// out, or this device removed. `None` when it can't be asked (offline).
    /// No session at all counts as ended: only [`Folder::signed_out`] drops
    /// it, and only after the server said so.
    pub async fn session_ended(&self) -> Option<bool> {
        let p = self.store().ok()?.load().ok()??;
        let Some(token) = p.session_token else {
            return Some(true);
        };
        let api = Api::new(&p.server, Credentials::Session(token)).ok()?;
        match api.me().await {
            Ok(_) => Some(false),
            Err(e) if e.is_unauthorized() || e.is_device_revoked() => Some(true),
            Err(_) => None,
        }
    }

    /// Stop and forget the folder's keys and local copies. The Markdown
    /// files stay where they are.
    pub async fn forget(&self) -> Result<()> {
        let mut running = self.running.lock().await;
        self.stop_locked(&mut running).await;
        account::forget_locally(&self.dirs, PROFILE)?;
        self.updates.update(|s| s.folder = FolderStatus::Off);
        Ok(())
    }
}

/// Whether `dir` holds anything but plain files and folders (symbolic
/// links, devices, sockets…) — what could lead the notes elsewhere.
fn holds_links(dir: &Path) -> bool {
    walkdir::WalkDir::new(dir)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .any(|e| match e {
            Ok(e) => !(e.file_type().is_dir() || e.file_type().is_file()),
            Err(_) => true,
        })
}

/// A folder picked in the system's dialog, pinned: its real path (links
/// resolved) and which folder it is, so a folder swapped for another — or
/// for a link — while the person was asked is noticed, not written into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pinned {
    pub path: PathBuf,
    id: Option<(u64, u64)>,
}

impl Pinned {
    pub fn new(picked: &Path) -> Result<Pinned> {
        let path = std::fs::canonicalize(picked)
            .with_context(|| format!("opening {}", setup::shown(picked)))?;
        let id = Self::id_of(&path)?;
        Ok(Pinned { path, id })
    }

    fn id_of(path: &Path) -> Result<Option<(u64, u64)>> {
        let m = std::fs::symlink_metadata(path)?;
        if !m.file_type().is_dir() {
            bail!("{} is no longer a folder", setup::shown(path));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Some((m.dev(), m.ino())))
        }
        #[cfg(not(unix))]
        {
            Ok(None)
        }
    }

    /// Still the same folder, at the same path, not a link.
    pub fn still(&self) -> Result<()> {
        if Self::id_of(&self.path)? != self.id {
            bail!(
                "{} changed while you were asked: choose it again",
                setup::shown(&self.path)
            );
        }
        Ok(())
    }
}

/// A task that ends with its owner: dropped (an aborted `run`), it is
/// aborted too, so nothing of the sync outlives a stop.
struct Owned<T>(JoinHandle<T>);

impl<T> Drop for Owned<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Wait for `t` to end, for `within` — then cut it short.
async fn end<T>(mut t: Owned<T>, within: Duration) {
    if tokio::time::timeout(within, &mut t.0).await.is_err() {
        t.0.abort();
        let _ = (&mut t.0).await;
    }
}

/// `notekpr sync --dir` without a terminal: until `stop`, or until either
/// half stops on its own (then the error says why).
async fn run(
    dirs: &Dirs,
    dir: &Path,
    mut stop: watch::Receiver<bool>,
    updates: &Updates,
) -> Result<()> {
    // Starting can take a while (the server, a big folder): a stop meanwhile
    // ends it there.
    let started = async {
        let session = Session::open(dirs, PROFILE)?;
        let hub = Hub::start(
            Arc::new(session),
            HubOptions {
                vaults: vec![],
                read_only: false,
                offline_ok: true,
                catch_up: true,
                detached: false,
                quick_notes: false,
            },
        )
        .await?;
        let m = Mirror::with_options(hub.clone(), dir, MirrorOptions::default()).await?;
        anyhow::Ok((hub, m))
    };
    let (hub, mut m) = tokio::select! {
        r = started => r?,
        _ = stop.wait_for(|s| *s) => return Ok(()),
    };
    // Both halves stop on this one: on `stop`, or when the other ended.
    let (halt, halted) = watch::channel(false);
    let mut live = Owned(tokio::spawn(livesync::run(hub.clone(), halted.clone())));
    let first = tokio::select! {
        r = m.reconcile_all(false) => r.map(Some),
        _ = stop.wait_for(|s| *s) => Ok(None),
    };
    if !matches!(first, Ok(Some(_))) {
        let _ = halt.send(true);
        end(live, Duration::from_secs(5)).await;
        hub.save_all().await;
        return first.map(|_| ());
    }
    updates.update(|s| {
        s.folder = FolderStatus::Syncing {
            dir: setup::shown(dir),
            vaults: hub.vault_ids().len(),
        }
    });
    let mut mirror = Owned(tokio::spawn(m.run(halted)));
    let (result, live, mirror) = tokio::select! {
        _ = stop.wait_for(|s| *s) => (Ok(()), Some(live), Some(mirror)),
        r = &mut live.0 => (
            match r {
                Ok(Ok(())) => Err(anyhow!("the sync stopped unexpectedly")),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(anyhow!("the sync failed: {e}")),
            },
            None,
            Some(mirror),
        ),
        r = &mut mirror.0 => (
            match r {
                Ok(Ok(())) => Err(anyhow!("the Markdown folder stopped unexpectedly")),
                Ok(Err(e)) => Err(e.context("the Markdown folder failed")),
                Err(e) => Err(anyhow!("the Markdown folder failed: {e}")),
            },
            Some(live),
            None,
        ),
    };
    let _ = halt.send(true);
    // The mirror first: it takes in what changed in the folder as it stops.
    for t in [mirror, live].into_iter().flatten() {
        end(t, Duration::from_secs(5)).await;
    }
    finish(&hub).await;
    result
}

/// What `notekpr sync` does as it stops: push, keep a revision of what
/// changed, save.
async fn finish(hub: &Hub) {
    for id in hub.vault_ids() {
        let _ = tokio::time::timeout(Duration::from_secs(5), hub.push(&id)).await;
    }
    hub.snapshot_idle(Duration::ZERO, livesync::SESSION_MIN_UPDATES)
        .await;
    hub.save_all().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picked_folder_swapped_meanwhile_is_noticed() {
        let t = tempfile::tempdir().unwrap();
        let notes = t.path().join("Notes");
        std::fs::create_dir(&notes).unwrap();
        let pinned = Pinned::new(&notes).unwrap();
        pinned.still().unwrap();
        // Swapped for a link to another folder…
        std::fs::rename(&notes, t.path().join("Moved")).unwrap();
        let elsewhere = t.path().join("Elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&elsewhere, &notes).unwrap();
            assert!(pinned.still().is_err(), "a link where the folder was");
            std::fs::remove_file(&notes).unwrap();
            // …or for another folder of the same name.
            std::fs::create_dir(&notes).unwrap();
            assert!(pinned.still().is_err(), "another folder of the same name");
        }
        // A link picked is pinned as where it leads.
        #[cfg(unix)]
        {
            let link = t.path().join("Link");
            std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
            let p = Pinned::new(&link).unwrap();
            assert_eq!(p.path, std::fs::canonicalize(&elsewhere).unwrap());
            p.still().unwrap();
        }
    }

    #[test]
    fn links_inside_are_noticed() {
        let t = tempfile::tempdir().unwrap();
        assert!(!holds_links(t.path()));
        std::fs::create_dir_all(t.path().join("a/b")).unwrap();
        std::fs::write(t.path().join("a/b/note.md"), "x").unwrap();
        assert!(!holds_links(t.path()));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc", t.path().join("a/b/etc")).unwrap();
            assert!(holds_links(t.path()));
        }
    }
}
