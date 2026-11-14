//! Running as a Flatpak (md.notekpr.Notekpr, packaging/flatpak): what its
//! sandbox changes. The app runs the same,
//! but for three things:
//! * the tray's icon is written where the desktop outside can read it
//!   ([`tray_icon_dir`]);
//! * starting at login is asked of the Background portal, which writes the
//!   autostart entry outside, where the sandbox can't ([`request_autostart`]);
//! * the CLI and the app look for each other's Markdown folders in both
//!   installs' folders (`notekpr_cli::profile::desktop_roots_all`).

use std::path::PathBuf;

pub use notekpr_cli::profile::inside_flatpak as inside;

/// Where the tray's icon is written in the Flatpak: the one folder of the
/// app's runtime directory that the desktop outside sees, at the same path.
/// None outside one (the tray's own default then).
pub fn tray_icon_dir() -> Option<PathBuf> {
    if !inside() {
        return None;
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let id = std::env::var("FLATPAK_ID")
        .unwrap_or_else(|_| notekpr_cli::profile::DESKTOP_APP_ID.to_string());
    Some(PathBuf::from(runtime).join("app").join(id))
}

/// Ask the Background portal to start the app when the person logs in (or
/// no longer): `commandline` starts it, inside the sandbox. The portal may
/// ask the person first. The answer is whether it starts at login now; an
/// error when nothing changed (the person said no, the desktop can't, the
/// portal went away before it answered).
#[cfg(target_os = "linux")]
pub async fn request_autostart(on: bool, commandline: Vec<String>) -> anyhow::Result<bool> {
    use anyhow::Context as _;
    // Calls answer in time or fail (not the person's answer, which takes
    // what it takes): a portal that hangs doesn't hang the app's settings.
    let bus = zbus::connection::Builder::session()?
        .method_timeout(std::time::Duration::from_secs(25))
        .build()
        .await
        .context("no session bus")?;
    ask_background(&bus, on, commandline).await
}

/// [`request_autostart`], on the bus `bus`.
#[cfg(target_os = "linux")]
async fn ask_background(
    bus: &zbus::Connection,
    on: bool,
    commandline: Vec<String>,
) -> anyhow::Result<bool> {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};

    use anyhow::Context as _;
    use futures_lite::StreamExt as _;
    use zbus::Proxy;
    use zbus::fdo::DBusProxy;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    const PORTAL: &str = "org.freedesktop.portal.Desktop";
    // Its properties read when asked for, not cached: a cache waits for an
    // answer that may never come.
    let background: Proxy = zbus::proxy::Builder::new(bus)
        .destination(PORTAL)?
        .path("/org/freedesktop/portal/desktop")?
        .interface("org.freedesktop.portal.Background")?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await?;
    // Not every desktop has it (Hyprland's portals, say): said plainly, not
    // as the bus's error.
    if background.get_property::<u32>("version").await.is_err() {
        anyhow::bail!("This desktop can't start notekpr's Flatpak when you log in.");
    }

    // The portal answers on a request object named after us and a token of
    // ours (one per request): listened to before asking, so the answer can't
    // come first. The answer is lost if the portal goes away meanwhile (it
    // restarts, say): that is listened to as well, so the wait ends then too.
    static REQUESTS: AtomicU32 = AtomicU32::new(0);
    let token = format!(
        "notekpr_{}_{}",
        std::process::id(),
        REQUESTS.fetch_add(1, Ordering::Relaxed)
    );
    let sender = bus
        .unique_name()
        .context("no name on the session bus")?
        .as_str()
        .trim_start_matches(':')
        .replace('.', "_");
    let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let request = Proxy::new(bus, PORTAL, path.as_str(), "org.freedesktop.portal.Request").await?;
    let mut answers = request.receive_signal("Response").await?;
    let mut portal_gone = DBusProxy::new(bus)
        .await?
        .receive_name_owner_changed_with_args(&[(0, PORTAL)])
        .await?;

    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert(
        "reason",
        Value::from("Start notekpr in the tray when you log in"),
    );
    options.insert("autostart", Value::from(on));
    options.insert("commandline", Value::from(commandline));
    options.insert("dbus-activatable", Value::from(false));
    let _: OwnedObjectPath = background
        .call("RequestBackground", &("", options))
        .await
        .context("asking the Background portal")?;

    let answer = futures_lite::future::or(async { answers.next().await }, async {
        portal_gone.next().await;
        None
    })
    .await
    .context("The system stopped before it answered: try again.")?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = answer.body().deserialize()?;
    // 0: done; 1: the person said no; 2: it didn't happen otherwise.
    if code != 0 {
        anyhow::bail!(if on {
            "The system didn't allow notekpr to start when you log in."
        } else {
            "The system didn't stop notekpr starting when you log in."
        });
    }
    Ok(results
        .get("autostart")
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false))
}

#[cfg(not(target_os = "linux"))]
pub async fn request_autostart(_on: bool, _commandline: Vec<String>) -> anyhow::Result<bool> {
    anyhow::bail!("there is no Flatpak here")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_a_flatpak_the_tray_keeps_its_own_folder() {
        // Tests don't run in a Flatpak.
        assert!(!inside());
        assert_eq!(tray_icon_dir(), None);
    }

    /// The Background portal, played on a private bus of its own
    /// (dbus-daemon) by [`Portal`]: what it answers is what the app says.
    #[cfg(target_os = "linux")]
    #[test]
    fn starting_at_login_is_what_the_background_portal_answers() {
        use portal::{Answer, ask};

        assert!(ask(Some(Answer::With(0, true)), true).unwrap());
        assert!(!ask(Some(Answer::With(0, false)), false).unwrap());
        // The person said no, or it didn't happen: an error, so the setting
        // stays as it was — never "off" while the system still starts it.
        let e = ask(Some(Answer::With(1, true)), false).unwrap_err();
        assert!(e.to_string().contains("didn't stop"), "{e:#}");
        let e = ask(Some(Answer::With(2, false)), true).unwrap_err();
        assert!(e.to_string().contains("didn't allow"), "{e:#}");
        // The portal goes away before it answers: the wait ends.
        let e = ask(Some(Answer::Never), true).unwrap_err();
        assert!(e.to_string().contains("before it answered"), "{e:#}");
        // A desktop without it.
        let e = ask(None, true).unwrap_err();
        assert!(e.to_string().contains("can't start"), "{e:#}");
    }

    #[cfg(target_os = "linux")]
    mod portal {
        use std::collections::HashMap;
        use std::io::{BufRead, BufReader};
        use std::process::{Child, Command, Stdio};
        use std::time::Duration;

        use zbus::names::BusName;
        use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
        use zbus::{Connection, connection, interface, message::Header};

        const NAME: &str = "org.freedesktop.portal.Desktop";

        #[derive(Clone, Copy)]
        pub enum Answer {
            /// The response's code and `autostart`.
            With(u32, bool),
            /// None: it goes away instead.
            Never,
        }

        struct Portal(Answer);

        /// Another of the portal's interfaces, all a desktop without the
        /// Background portal has there (Hyprland's, say).
        struct Settings;

        #[interface(name = "org.freedesktop.portal.Settings")]
        impl Settings {
            #[zbus(property, name = "version")]
            fn version(&self) -> u32 {
                2
            }
        }

        #[interface(name = "org.freedesktop.portal.Background")]
        impl Portal {
            // The portal's own name for it (zbus would say "Version").
            #[zbus(property, name = "version")]
            fn version(&self) -> u32 {
                2
            }

            async fn request_background(
                &self,
                #[zbus(header)] header: Header<'_>,
                #[zbus(connection)] conn: &Connection,
                _parent_window: &str,
                options: HashMap<String, OwnedValue>,
            ) -> OwnedObjectPath {
                let sender = header.sender().unwrap().as_str();
                let sender = sender.trim_start_matches(':').replace('.', "_");
                let Value::Str(token) = &*options["handle_token"] else {
                    panic!("no handle_token");
                };
                let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
                match self.0 {
                    Answer::With(code, autostart) => {
                        let results = HashMap::from([("autostart", Value::from(autostart))]);
                        conn.emit_signal(
                            None::<BusName<'_>>,
                            path.as_str(),
                            "org.freedesktop.portal.Request",
                            "Response",
                            &(code, results),
                        )
                        .await
                        .unwrap();
                    }
                    Answer::Never => {
                        let conn = conn.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_millis(200));
                            let _ = futures_lite::future::block_on(conn.release_name(NAME));
                        });
                    }
                }
                path.try_into().unwrap()
            }
        }

        /// A bus of its own: dbus-daemon, stopped with it.
        struct Bus(Child, String);

        impl Bus {
            fn start() -> Bus {
                let mut daemon = Command::new("dbus-daemon")
                    .args(["--session", "--nofork", "--print-address=1"])
                    .stdout(Stdio::piped())
                    .spawn()
                    .expect("dbus-daemon (the dbus package)");
                let mut address = String::new();
                BufReader::new(daemon.stdout.take().unwrap())
                    .read_line(&mut address)
                    .unwrap();
                Bus(daemon, address.trim().to_string())
            }
        }

        impl Drop for Bus {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        /// Ask a portal answering `answer` (none: a desktop without the
        /// Background portal) to turn starting at login `on`; a test fails
        /// rather than wait for ever.
        pub fn ask(answer: Option<Answer>, on: bool) -> anyhow::Result<bool> {
            let bus = Bus::start();
            let address = bus.1.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let r = futures_lite::future::block_on(async {
                    let portal = connection::Builder::address(address.as_str())?.name(NAME)?;
                    let path = "/org/freedesktop/portal/desktop";
                    let _portal = match answer {
                        Some(a) => portal.serve_at(path, Portal(a))?,
                        None => portal.serve_at(path, Settings)?,
                    }
                    .build()
                    .await?;
                    let app = connection::Builder::address(address.as_str())?
                        .build()
                        .await?;
                    super::super::ask_background(&app, on, vec!["notekpr-desktop".into()]).await
                });
                let _ = tx.send(r);
            });
            let r = rx
                .recv_timeout(Duration::from_secs(20))
                .expect("still waiting for the portal");
            drop(bus);
            r
        }
    }
}
