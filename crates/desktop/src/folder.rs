//! The Markdown folder: the CLI's own sync and mirror (`notekpr sync
//! --dir`), run inside the app with a CLI profile of the app's
//! ([`crate::paths::Paths::folder`]) — never one of the person's CLI
//! profiles. It uses this computer's device and the window's session,
//! handed over once ([`notekpr_cli::account::adopt`]), and keeps running
//! while the window is hidden; at the next start it runs from what it kept.
//! One per account ([`crate::paths::Paths::folder`]): only the open
//! account's runs, and only for the server that account is on.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use notekpr_cli::account::{self, Adopt};
use notekpr_cli::api::{Api, Credentials};
use notekpr_cli::hub::{Hub, HubOptions};
use notekpr_cli::mirror::{Mirror, MirrorOptions};
use notekpr_cli::profile::{Dirs, Profile, ProfileLock, ProfileStore};
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
    /// The server its account is on (with notes only on this computer, the
    /// app's own): a profile kept for another never runs here.
    server: Option<String>,
    updates: Arc<Updates>,
    running: Mutex<Option<Running>>,
}

/// Whether two addresses are one server's, as the CLI writes them.
fn same_server(a: &str, b: &str) -> bool {
    let n = |s: &str| notekpr_cli::profile::normalize_server_url(s, true).ok();
    n(a).is_some() && n(a) == n(b)
}

/// The app's own server's address, as notes only on this computer keep it
/// (`http://127.0.0.1:<port>`).
fn is_own_server(s: &str) -> bool {
    url::Url::parse(s)
        .is_ok_and(|u| u.scheme() == "http" && u.host_str() == Some("127.0.0.1") && u.path() == "/")
}

/// Before accounts (0.17.0) the app kept one Markdown folder per app
/// profile (`legacy`). At the first start since, it becomes the open
/// account's (`target`), if it was kept for that account's server (only on
/// this computer, `server` none: the app's own); otherwise, or if that
/// account has one already, it stays where it was. True if it moved. Call
/// it once this is the only copy of the app running.
pub fn take_legacy(legacy: &Dirs, target: &Dirs, server: Option<&str>) -> Result<bool> {
    let (Some(from), Some(to)) = (legacy.config.parent(), target.config.parent()) else {
        return Ok(false);
    };
    if !legacy.profiles_dir().exists() {
        return Ok(false);
    }
    let Some(p) = ProfileStore::new(legacy, PROFILE)?.load()? else {
        return Ok(false);
    };
    let belongs = match server {
        Some(s) => same_server(&p.server, s),
        None => is_own_server(&p.server),
    };
    if !belongs {
        tracing::warn!(
            "the Markdown folder from before accounts was kept for another server than the open account's: left where it was"
        );
        return Ok(false);
    }
    if to.exists() {
        tracing::warn!(
            "the open account has a Markdown folder already: the one from before accounts is left where it was"
        );
        return Ok(false);
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(from, to).context("moving the Markdown folder's profile to its account")?;
    Ok(true)
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
    pub fn new(dirs: Dirs, server: Option<String>, updates: Arc<Updates>) -> Folder {
        Folder {
            dirs,
            server,
            updates,
            running: Mutex::new(None),
        }
    }

    /// Whether `p` was kept for this account's server.
    fn belongs(&self, p: &Profile) -> bool {
        self.server
            .as_deref()
            .is_some_and(|s| same_server(&p.server, s))
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

    /// Whether `dir` can become the Markdown folder: the rules the CLI's
    /// `setup` and `sync --dir` follow too ([`setup::check_new_folder`]),
    /// and — until the app brings notes in, as `notekpr setup` does —
    /// empty.
    pub fn check(&self, dir: &Path) -> Check {
        match self.refusal(dir) {
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

    fn refusal(&self, dir: &Path) -> Result<()> {
        if !dir.is_absolute() {
            bail!("choose a folder by its full path");
        }
        setup::check_new_folder(dir, &self.dirs, PROFILE)?;
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
        if !self
            .server
            .as_deref()
            .is_some_and(|s| same_server(&a.server, s))
        {
            bail!("the folder is this account's: its server only");
        }
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
        if let Some(p) = self.store()?.load()?
            && !self.belongs(&p)
        {
            let error = format!(
                "this folder was kept for {}, not this account's server: stop it here",
                p.server
            );
            self.updates.update(|s| {
                s.folder = FolderStatus::Stopped {
                    dir: setup::shown(&dir),
                    error,
                }
            });
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
        if !self.belongs(&p) {
            bail!("the folder was kept for another server");
        }
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

    /// Notes only on this computer: the app's own server has another
    /// address at each start, which the folder's profile follows before
    /// the folder starts or checks a session. Nothing else of it changes
    /// (the account, this device, its copies). Call it before anything
    /// else of the folder.
    pub fn follow_server(&self, origin: &str) -> Result<()> {
        let store = self.store()?;
        if store
            .load()?
            .is_some_and(|p| p.server != origin && is_own_server(&p.server))
        {
            store.update(|p| p.server = origin.to_string())?;
        }
        Ok(())
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
    use notekpr_cli::profile::Profile;
    use notekpr_core::protocol::PrincipalKind;

    #[test]
    fn follows_the_app_s_own_server_to_its_new_port() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = Folder::new(
            Dirs::under(tmp.path()),
            Some("http://127.0.0.1:52002".into()),
            Arc::new(Updates::default()),
        );
        // No folder yet: nothing to follow, nothing made.
        folder.follow_server("http://127.0.0.1:41001").unwrap();
        assert!(folder.store().unwrap().load().unwrap().is_none());

        let mut p = Profile::new("http://127.0.0.1:41001", PrincipalKind::Human, false);
        p.user_id = Some("u1".into());
        p.device_id = Some("d1".into());
        p.session_token = Some("t".into());
        p.sync.mirror_dir = Some(tmp.path().join("Notes"));
        folder.store().unwrap().save(&p).unwrap();

        folder.follow_server("http://127.0.0.1:52002").unwrap();
        let p = folder.store().unwrap().load().unwrap().unwrap();
        assert_eq!(p.server, "http://127.0.0.1:52002");
        assert_eq!(p.user_id.as_deref(), Some("u1"));
        assert_eq!(p.device_id.as_deref(), Some("d1"));
        assert_eq!(p.session_token.as_deref(), Some("t"));
        assert_eq!(folder.configured(), Some(tmp.path().join("Notes")));
    }

    fn kept(dirs: &Dirs, server: &str) {
        let mut p = Profile::new(server, PrincipalKind::Human, false);
        p.user_id = Some("u1".into());
        ProfileStore::new(dirs, PROFILE).unwrap().save(&p).unwrap();
    }

    #[test]
    fn the_folder_from_before_accounts_goes_to_its_own_account_only() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = Dirs::under(&tmp.path().join("folder"));
        let to = |id: &str| Dirs::under(&tmp.path().join("folders").join(id));
        // Kept for another server than the open account's: stays.
        kept(&legacy, "https://a.example.com");
        assert!(!take_legacy(&legacy, &to("b"), Some("https://b.example.com")).unwrap());
        assert!(!take_legacy(&legacy, &to("here"), None).unwrap());
        assert!(legacy.profiles_dir().exists());
        // Its own: moved, with everything it kept.
        assert!(take_legacy(&legacy, &to("a"), Some("https://a.example.com/")).unwrap());
        assert!(!legacy.profiles_dir().exists());
        let p = ProfileStore::new(&to("a"), PROFILE)
            .unwrap()
            .load()
            .unwrap()
            .unwrap();
        assert_eq!(p.user_id.as_deref(), Some("u1"));
        // Nothing left: nothing to do.
        assert!(!take_legacy(&legacy, &to("a"), Some("https://a.example.com")).unwrap());
        // Only on this computer: the app's own server's.
        kept(&legacy, "http://127.0.0.1:41001");
        assert!(!take_legacy(&legacy, &to("x"), Some("https://a.example.com")).unwrap());
        assert!(take_legacy(&legacy, &to("this-computer"), None).unwrap());
        // An account with a folder already keeps it.
        kept(&legacy, "https://a.example.com");
        assert!(!take_legacy(&legacy, &to("a"), Some("https://a.example.com")).unwrap());
    }

    #[tokio::test]
    async fn a_folder_runs_only_for_its_account_s_server() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(&tmp.path().join("folders/b"));
        let notes = tmp.path().join("Notes");
        std::fs::create_dir(&notes).unwrap();
        let mut p = Profile::new("https://a.example.com", PrincipalKind::Human, false);
        p.user_id = Some("u1".into());
        p.session_token = Some("t".into());
        p.sync.mirror_dir = Some(notes);
        ProfileStore::new(&dirs, PROFILE).unwrap().save(&p).unwrap();
        let updates = Arc::new(Updates::default());
        let folder = Folder::new(dirs, Some("https://b.example.com".into()), updates.clone());
        folder.start().await.unwrap();
        let (_, status) = updates.current();
        assert!(
            matches!(&status.folder, FolderStatus::Stopped { error, .. } if error.contains("a.example.com")),
            "{:?}",
            status.folder
        );
        assert!(folder.session_changed("other".into(), 0).await.is_err());
    }
}
