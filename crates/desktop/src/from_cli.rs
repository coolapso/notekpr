//! Taking over a Markdown folder the CLI keeps (docs/desktop.md "From the
//! CLI"): the CLI's profiles that keep one for the open account, their
//! background sync (`notekpr-sync@<profile>`) stopped and turned off at
//! login through systemd's user manager, and the folder given back on the
//! CLI's side — what `notekpr sync --no-dir` does — while no CLI runs that
//! profile. The app then takes the folder as it takes any
//! ([`crate::folder::Folder::take_and_start`]).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use notekpr_cli::profile::{self, Dirs, ProfileLock, ProfileStore};
use notekpr_core::protocol::PrincipalKind;
use serde::Serialize;
use tokio::sync::oneshot;

/// A CLI profile keeping a Markdown folder for the open account.
#[derive(Clone, Debug)]
pub struct Found {
    pub dirs: Dirs,
    pub profile: String,
    pub dir: PathBuf,
}

/// The CLI's configurations on this computer: the usual one, and in the
/// Flatpak the one outside its sandbox.
pub fn configurations() -> Vec<Dirs> {
    let mut v: Vec<Dirs> = Vec::new();
    for d in Dirs::from_env()
        .into_iter()
        .chain(profile::cli_dirs_outside_sandbox())
    {
        if !v.iter().any(|o| o.config == d.config) {
            v.push(d);
        }
    }
    v
}

/// Whether `p` is a person's profile on `server`, signed in as `user_id`.
fn of(p: &profile::Profile, server: &str, user_id: &str) -> bool {
    p.kind == PrincipalKind::Human
        && p.user_id.as_deref() == Some(user_id)
        && crate::folder::same_server(&p.server, server)
}

/// The profiles in `configs` of the person `user_id` on `server` that keep
/// a Markdown folder, one that is there.
pub fn find(configs: &[Dirs], server: &str, user_id: &str) -> Vec<Found> {
    let mut v = vec![];
    for dirs in configs {
        for name in dirs.profiles() {
            let Ok(Some(p)) = ProfileStore::new(dirs, &name).and_then(|s| s.load()) else {
                continue;
            };
            let Some(dir) = p.sync.mirror_dir.clone() else {
                continue;
            };
            if of(&p, server, user_id) && dir.is_dir() {
                v.push(Found {
                    dirs: dirs.clone(),
                    profile: name,
                    dir,
                });
            }
        }
    }
    v
}

impl Found {
    /// A CLI runs this profile now (a sync, its gateway): its lock is held.
    pub fn running(&self) -> bool {
        matches!(
            ProfileLock::try_acquire(&self.dirs, &self.profile),
            Ok(None)
        )
    }

    /// Its background sync's systemd unit (the profile's name is letters,
    /// digits, `-` and `_`: nothing to escape).
    pub fn unit(&self) -> String {
        format!("notekpr-sync@{}.service", self.profile)
    }

    /// What stops it by hand: none where the CLI doesn't sync in the
    /// background (it does through systemd, on Linux only).
    pub fn command(&self) -> Option<String> {
        cfg!(target_os = "linux").then(|| format!("systemctl --user disable --now {}", self.unit()))
    }

    /// Wait until no CLI runs the profile, then hold its lock: none if
    /// `cancel` came first.
    pub async fn wait_until_free(
        &self,
        mut cancel: oneshot::Receiver<()>,
    ) -> Result<Option<ProfileLock>> {
        loop {
            if let Some(lock) = ProfileLock::try_acquire(&self.dirs, &self.profile)? {
                return Ok(Some(lock));
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(300)) => {}
                _ = &mut cancel => return Ok(None),
            }
        }
    }

    /// What `notekpr sync --no-dir` does, while no CLI runs the profile
    /// (`_lock`): it no longer keeps the folder, whose `.notekpr-folder` goes
    /// if it is that profile's. Only if the profile is still the person's
    /// on that server and keeps that folder.
    pub fn give_back(&self, server: &str, user_id: &str, _lock: &ProfileLock) -> Result<()> {
        let store = ProfileStore::new(&self.dirs, &self.profile)?;
        let p = store.require()?;
        if !of(&p, server, user_id) || p.sync.mirror_dir.as_deref() != Some(self.dir.as_path()) {
            bail!(
                "the CLI's profile '{}' no longer keeps {} for you: nothing was taken over",
                self.profile,
                notekpr_cli::setup::shown(&self.dir)
            );
        }
        notekpr_cli::folder_claim::release(&store);
        store
            .update(|p| p.sync.mirror_dir = None)
            .with_context(|| format!("changing the CLI's profile '{}'", self.profile))?;
        Ok(())
    }
}

/// What systemd's user manager says of a background sync.
#[derive(Serialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Service {
    /// It runs.
    pub active: bool,
    /// It starts at login.
    pub enabled: bool,
}

#[cfg(target_os = "linux")]
const SYSTEMD: &str = "org.freedesktop.systemd1";

/// Systemd's user manager on the session bus. Never reached from the
/// Flatpak: its bus lets the manager's name through only to apps that ask
/// for it, and this one doesn't.
#[cfg(target_os = "linux")]
pub struct Systemd(zbus::Proxy<'static>);

/// No systemd: the CLI syncs in the background on Linux only (`notekpr
/// setup`), so there is never one to reach.
#[cfg(not(target_os = "linux"))]
pub struct Systemd(std::convert::Infallible);

#[cfg(not(target_os = "linux"))]
impl Systemd {
    pub async fn connect() -> Option<Systemd> {
        None
    }

    pub async fn state(&self, _unit: &str) -> Result<Service> {
        match self.0 {}
    }

    pub async fn stop(&self, _unit: &str, _was: Service) -> Result<()> {
        match self.0 {}
    }

    pub async fn restore(&self, _unit: &str, _was: Service) -> Result<()> {
        match self.0 {}
    }
}

#[cfg(target_os = "linux")]
type UnitRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    zbus::zvariant::OwnedObjectPath,
    u32,
    String,
    zbus::zvariant::OwnedObjectPath,
);

#[cfg(target_os = "linux")]
impl Systemd {
    /// The session's manager; none where it can't be reached (the Flatpak,
    /// a system without systemd).
    pub async fn connect() -> Option<Systemd> {
        let bus = zbus::connection::Builder::session()
            .ok()?
            .method_timeout(Duration::from_secs(25))
            .build()
            .await
            .ok()?;
        Systemd::on(&bus).await.ok()
    }

    /// The manager on `bus`, if it answers there.
    pub async fn on(bus: &zbus::Connection) -> Result<Systemd> {
        let p: zbus::Proxy<'static> = zbus::proxy::Builder::new(bus)
            .destination(SYSTEMD)?
            .path("/org/freedesktop/systemd1")?
            .interface("org.freedesktop.systemd1.Manager")?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await?;
        let _: String = p
            .get_property("Version")
            .await
            .context("systemd's user manager")?;
        Ok(Systemd(p))
    }

    pub async fn state(&self, unit: &str) -> Result<Service> {
        // No unit file of that name: not enabled.
        let enabled = self
            .0
            .call::<_, _, String>("GetUnitFileState", &(unit,))
            .await
            .is_ok_and(|s| s == "enabled");
        let rows: Vec<UnitRow> = self.0.call("ListUnitsByNames", &(vec![unit],)).await?;
        let active = rows
            .first()
            .is_some_and(|r| matches!(r.3.as_str(), "active" | "activating" | "reloading"));
        Ok(Service { active, enabled })
    }

    /// Stop `unit` and turn it off at login, as it is (`was`).
    pub async fn stop(&self, unit: &str, was: Service) -> Result<()> {
        if was.enabled {
            let _: Vec<(String, String, String)> = self
                .0
                .call("DisableUnitFiles", &(vec![unit], false))
                .await
                .with_context(|| format!("turning {unit} off at login"))?;
            let _: () = self.0.call("Reload", &()).await?;
        }
        if was.active {
            let _: zbus::zvariant::OwnedObjectPath = self
                .0
                .call("StopUnit", &(unit, "replace"))
                .await
                .with_context(|| format!("stopping {unit}"))?;
        }
        Ok(())
    }

    /// `unit` as it was before [`Systemd::stop`].
    pub async fn restore(&self, unit: &str, was: Service) -> Result<()> {
        if was.enabled {
            let _: (bool, Vec<(String, String, String)>) = self
                .0
                .call("EnableUnitFiles", &(vec![unit], false, false))
                .await
                .with_context(|| format!("turning {unit} on at login again"))?;
            let _: () = self.0.call("Reload", &()).await?;
        }
        if was.active {
            let _: zbus::zvariant::OwnedObjectPath = self
                .0
                .call("StartUnit", &(unit, "replace"))
                .await
                .with_context(|| format!("starting {unit} again"))?;
        }
        Ok(())
    }
}

/// The CLI's background sync put back as it was before the app stopped it
/// (`was`), after `why` (an error, or "cancelled"): what to do by hand, too,
/// when that fails.
pub async fn put_back(
    systemd: Option<&Systemd>,
    was: Option<Service>,
    unit: &str,
    why: String,
) -> String {
    let (Some(s), Some(w)) = (systemd, was) else {
        return why;
    };
    let Err(e) = s.restore(unit, w).await else {
        return why;
    };
    tracing::warn!("putting the CLI's background sync back: {e:#}");
    let fix = format!(
        "notekpr couldn't put the CLI's background sync back as it was ({e:#}): run `systemctl --user \
         enable --now {}`",
        unit.trim_end_matches(".service")
    );
    // A cancel says nothing; this must be said.
    if why == "cancelled" {
        fix
    } else {
        format!("{why}. {fix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notekpr_cli::profile::Profile;

    fn cli(
        dirs: &Dirs,
        name: &str,
        server: &str,
        user: &str,
        dir: Option<PathBuf>,
    ) -> ProfileStore {
        let s = ProfileStore::new(dirs, name).unwrap();
        let mut p = Profile::new(server, PrincipalKind::Human, false);
        p.user_id = Some(user.into());
        p.sync.mirror_dir = dir;
        s.save(&p).unwrap();
        s
    }

    #[test]
    fn only_the_person_s_own_folders_on_that_server() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(&tmp.path().join("cli"));
        let notes = tmp.path().join("Notes");
        std::fs::create_dir(&notes).unwrap();
        cli(
            &dirs,
            "default",
            "https://n.example.com",
            "u1",
            Some(notes.clone()),
        );
        cli(
            &dirs,
            "other",
            "https://other.example.com",
            "u1",
            Some(notes.clone()),
        );
        cli(
            &dirs,
            "bob",
            "https://n.example.com",
            "u2",
            Some(notes.clone()),
        );
        cli(&dirs, "nofolder", "https://n.example.com", "u1", None);
        cli(
            &dirs,
            "gone",
            "https://n.example.com",
            "u1",
            Some(tmp.path().join("Gone")),
        );
        let agent = ProfileStore::new(&dirs, "agent").unwrap();
        let mut p = Profile::new("https://n.example.com", PrincipalKind::Agent, false);
        p.user_id = Some("u1".into());
        p.sync.mirror_dir = Some(notes.clone());
        agent.save(&p).unwrap();

        let found = find(std::slice::from_ref(&dirs), "https://n.example.com/", "u1");
        let names: Vec<_> = found.iter().map(|f| f.profile.as_str()).collect();
        assert_eq!(names, ["default"]);
        let f = &found[0];
        assert_eq!(f.unit(), "notekpr-sync@default.service");
        assert!(!f.running());
        let held = ProfileLock::try_acquire(&dirs, "default").unwrap().unwrap();
        assert!(f.running());
        drop(held);
        assert!(find(&[dirs], "https://n.example.com", "u3").is_empty());
    }

    #[tokio::test]
    async fn given_back_once_no_cli_runs_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(&tmp.path().join("cli"));
        let notes = tmp.path().join("Notes");
        std::fs::create_dir(&notes).unwrap();
        let store = cli(
            &dirs,
            "default",
            "https://n.example.com",
            "u1",
            Some(notes.clone()),
        );
        // The CLI keeps it: its `.notekpr-folder` says so.
        let claim = notekpr_cli::folder_claim::FolderClaim::take(
            &notes,
            &store,
            &notekpr_cli::folder_claim::cli_label("default"),
            false,
        )
        .unwrap();
        drop(claim);
        assert!(notes.join(".notekpr-folder").exists());
        let f = find(std::slice::from_ref(&dirs), "https://n.example.com", "u1").remove(0);

        // A CLI runs it: the wait goes on until it stops, or until cancelled.
        let running = ProfileLock::try_acquire(&dirs, "default").unwrap().unwrap();
        let (cancel, cancelled) = oneshot::channel();
        let waiting = tokio::spawn({
            let f = f.clone();
            async move { f.wait_until_free(cancelled).await }
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(!waiting.is_finished());
        cancel.send(()).unwrap();
        assert!(waiting.await.unwrap().unwrap().is_none());
        let (_keep, never) = oneshot::channel();
        let waiting = tokio::spawn({
            let f = f.clone();
            async move { f.wait_until_free(never).await }
        });
        tokio::time::sleep(Duration::from_millis(400)).await;
        drop(running);
        let lock = waiting.await.unwrap().unwrap().unwrap();

        // Not if the profile changed meanwhile: another account's now.
        store.update(|p| p.user_id = Some("u2".into())).unwrap();
        assert!(f.give_back("https://n.example.com", "u1", &lock).is_err());
        assert!(notes.join(".notekpr-folder").exists());
        let p = store.require().unwrap();
        assert_eq!(p.sync.mirror_dir.as_deref(), Some(notes.as_path()));

        // As it was: the CLI forgets the folder, its file goes.
        store.update(|p| p.user_id = Some("u1".into())).unwrap();
        f.give_back("https://n.example.com", "u1", &lock).unwrap();
        assert!(!notes.join(".notekpr-folder").exists());
        assert_eq!(store.require().unwrap().sync.mirror_dir, None);
    }

    /// Systemd's user manager, played on a private bus of its own: what the
    /// app asks of it, and in what order.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_cli_s_background_sync_stopped_and_put_back() {
        use manager::run;

        let calls = run(
            Service {
                active: true,
                enabled: true,
            },
            |s, unit| async move {
                let was = s.state(&unit).await?;
                assert_eq!(
                    was,
                    Service {
                        active: true,
                        enabled: true
                    }
                );
                s.stop(&unit, was).await?;
                assert_eq!(s.state(&unit).await?, Service::default());
                s.restore(&unit, was).await?;
                assert_eq!(s.state(&unit).await?, was);
                Ok(())
            },
        );
        assert_eq!(
            calls,
            [
                "DisableUnitFiles notekpr-sync@default.service",
                "Reload",
                "StopUnit notekpr-sync@default.service replace",
                "EnableUnitFiles notekpr-sync@default.service",
                "Reload",
                "StartUnit notekpr-sync@default.service replace",
            ]
        );
        // Half stopped (off at login, then the manager failed): put back
        // all the same, and what to do by hand said when that fails too.
        let calls = manager::run_on(
            Service {
                active: true,
                enabled: true,
            },
            true,
            |s, unit| async move {
                let was = s.state(&unit).await?;
                let e = s.stop(&unit, was).await.unwrap_err();
                let said = put_back(Some(&s), Some(was), &unit, format!("{e:#}")).await;
                assert!(
                    said.contains("systemctl --user enable --now notekpr-sync@default"),
                    "{said}"
                );
                assert_eq!(
                    put_back(None, Some(was), &unit, "cancelled".into()).await,
                    "cancelled"
                );
                Ok(())
            },
        );
        assert_eq!(
            calls,
            [
                "DisableUnitFiles notekpr-sync@default.service",
                "Reload",
                "EnableUnitFiles notekpr-sync@default.service",
                "Reload",
            ]
        );
        // Not running and off at login: nothing to stop, nothing to put back.
        let calls = run(Service::default(), |s, unit| async move {
            let was = s.state(&unit).await?;
            s.stop(&unit, was).await?;
            s.restore(&unit, was).await
        });
        assert!(calls.is_empty(), "{calls:?}");
    }

    #[cfg(target_os = "linux")]
    mod manager {
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        use zbus::zvariant::OwnedObjectPath;
        use zbus::{connection, interface};

        use super::{Service, Systemd};
        use crate::testbus::Bus;

        struct Manager {
            unit: Mutex<Service>,
            calls: Arc<Mutex<Vec<String>>>,
            /// `Reload` fails (a manager in trouble).
            broken: bool,
        }

        fn path() -> OwnedObjectPath {
            "/org/freedesktop/systemd1/job/1".try_into().unwrap()
        }

        #[interface(name = "org.freedesktop.systemd1.Manager")]
        impl Manager {
            #[zbus(property)]
            fn version(&self) -> String {
                "257".into()
            }

            fn get_unit_file_state(&self, _file: String) -> String {
                if self.unit.lock().unwrap().enabled {
                    "enabled"
                } else {
                    "disabled"
                }
                .into()
            }

            #[allow(clippy::type_complexity)]
            fn list_units_by_names(
                &self,
                names: Vec<String>,
            ) -> Vec<(
                String,
                String,
                String,
                String,
                String,
                String,
                OwnedObjectPath,
                u32,
                String,
                OwnedObjectPath,
            )> {
                let active = self.unit.lock().unwrap().active;
                names
                    .into_iter()
                    .map(|n| {
                        let state = if active { "active" } else { "inactive" };
                        (
                            n,
                            String::new(),
                            "loaded".into(),
                            state.into(),
                            String::new(),
                            String::new(),
                            path(),
                            0,
                            String::new(),
                            path(),
                        )
                    })
                    .collect()
            }

            fn disable_unit_files(
                &self,
                files: Vec<String>,
                _runtime: bool,
            ) -> Vec<(String, String, String)> {
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("DisableUnitFiles {}", files.join(" ")));
                self.unit.lock().unwrap().enabled = false;
                vec![]
            }

            fn enable_unit_files(
                &self,
                files: Vec<String>,
                _runtime: bool,
                _force: bool,
            ) -> (bool, Vec<(String, String, String)>) {
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("EnableUnitFiles {}", files.join(" ")));
                self.unit.lock().unwrap().enabled = true;
                (true, vec![])
            }

            fn reload(&self) -> zbus::fdo::Result<()> {
                self.calls.lock().unwrap().push("Reload".into());
                if self.broken {
                    return Err(zbus::fdo::Error::Failed("no".into()));
                }
                Ok(())
            }

            fn stop_unit(&self, name: String, mode: String) -> OwnedObjectPath {
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("StopUnit {name} {mode}"));
                self.unit.lock().unwrap().active = false;
                path()
            }

            fn start_unit(&self, name: String, mode: String) -> OwnedObjectPath {
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("StartUnit {name} {mode}"));
                self.unit.lock().unwrap().active = true;
                path()
            }
        }

        /// Run `f` against a manager whose `notekpr-sync@default` is `unit`:
        /// the calls that changed something.
        pub fn run<F, Fut>(unit: Service, f: F) -> Vec<String>
        where
            F: FnOnce(Systemd, String) -> Fut + Send + 'static,
            Fut: std::future::Future<Output = anyhow::Result<()>> + Send,
        {
            run_on(unit, false, f)
        }

        /// [`run`], with a manager whose `Reload` fails if `broken`.
        pub fn run_on<F, Fut>(unit: Service, broken: bool, f: F) -> Vec<String>
        where
            F: FnOnce(Systemd, String) -> Fut + Send + 'static,
            Fut: std::future::Future<Output = anyhow::Result<()>> + Send,
        {
            let bus = Bus::start();
            let address = bus.address().to_string();
            let calls = Arc::new(Mutex::new(vec![]));
            let manager = Manager {
                unit: Mutex::new(unit),
                calls: calls.clone(),
                broken,
            };
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let r = futures_lite::future::block_on(async {
                    let _manager = connection::Builder::address(address.as_str())?
                        .name("org.freedesktop.systemd1")?
                        .serve_at("/org/freedesktop/systemd1", manager)?
                        .build()
                        .await?;
                    let app = connection::Builder::address(address.as_str())?
                        .build()
                        .await?;
                    let s = Systemd::on(&app).await?;
                    f(s, "notekpr-sync@default.service".into()).await
                });
                let _ = tx.send(r);
            });
            rx.recv_timeout(Duration::from_secs(20))
                .expect("still waiting for the manager")
                .unwrap();
            drop(bus);
            calls.lock().unwrap().clone()
        }
    }
}
