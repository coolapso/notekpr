//! System notifications (docs/desktop.md "Notifications"). When someone
//! mentions the person while the window isn't in front — or is hidden in
//! the tray — the window asks the app for one ([`show`]); the app shows it
//! through the system's own service: the desktop's notification server on
//! Linux (`org.freedesktop.Notifications`, also from the Flatpak), a toast
//! on Windows, Notification Center on macOS. Clicking it brings the window
//! back and opens the thread: the click runs what [`show`] was given.
//!
//! The window's own notifications (the web's `Notification`) aren't used:
//! macOS's webview has none, and a click on one can't bring a window back
//! from the tray.
//!
//! A script in the window (someone else's, through a note) can do no more
//! with this than show text as notekpr does: the commands cap its length,
//! and what a click hands back to the window is checked to be ids.

/// What a click runs, at most once.
pub type OnClick = Box<dyn FnOnce() + Send>;

/// What the window's turning notifications on says when they can't be.
#[cfg(target_os = "linux")]
const NO_SERVICE: &str =
    "This computer has no notification service running, so notekpr can't show notifications.";

/// Make sure the system shows notekpr's notifications, asking the person
/// where the system wants that (macOS asks once). An error says why not,
/// for the person.
pub async fn allow() -> Result<(), String> {
    if test_file().is_some() {
        return Ok(());
    }
    imp::allow().await
}

/// Show a notification: `title`, then `body`. `on_click` runs if the person
/// clicks it (on the system's thread for it, or the app's).
pub async fn show(title: &str, body: &str, on_click: OnClick) -> Result<(), String> {
    if let Some(file) = test_file() {
        return test_show(&file, title, body, on_click);
    }
    imp::show(title, body, on_click).await
}

/// At most `max` characters of `s`, an ellipsis ending one cut short.
pub fn cap(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// Browser tests can't see the system's notifications or click them: with
/// `NOTEKPR_DESKTOP_TEST_NOTIFICATIONS=<file>` each one is added to that
/// file as a line of JSON instead (`{"title":…,"body":…}`) and clicked at
/// once. Debug builds only — release builds have no such thing.
#[cfg(debug_assertions)]
fn test_file() -> Option<std::path::PathBuf> {
    std::env::var_os("NOTEKPR_DESKTOP_TEST_NOTIFICATIONS").map(Into::into)
}

#[cfg(not(debug_assertions))]
fn test_file() -> Option<std::path::PathBuf> {
    None
}

fn test_show(
    file: &std::path::Path,
    title: &str,
    body: &str,
    on_click: OnClick,
) -> Result<(), String> {
    use std::io::Write as _;
    let line = serde_json::json!({ "title": title, "body": body }).to_string();
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .and_then(|mut f| writeln!(f, "{line}"))
        .map_err(|e| e.to_string())?;
    on_click();
    Ok(())
}

/// The clicks waiting, by the system's name for each notification: taken
/// when clicked, dropped when it closes otherwise. At most [`KEPT`]: the
/// oldest go first (the system may never say it closed one). Windows keeps
/// each toast's itself.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
struct Clicks<K> {
    waiting: std::sync::Mutex<Vec<(K, OnClick)>>,
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
const KEPT: usize = 50;

#[cfg(any(target_os = "linux", target_os = "macos", test))]
impl<K: PartialEq> Clicks<K> {
    const fn new() -> Self {
        Clicks {
            waiting: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn add(&self, key: K, on_click: OnClick) {
        let mut w = self.waiting.lock().unwrap();
        if w.len() >= KEPT {
            drop(w.remove(0));
        }
        w.push((key, on_click));
    }

    fn take(&self, key: &K) -> Option<OnClick> {
        let mut w = self.waiting.lock().unwrap();
        let i = w.iter().position(|(k, _)| k == key)?;
        Some(w.remove(i).1)
    }

    /// Clicked: what it runs, run (outside the lock).
    fn clicked(&self, key: &K) {
        if let Some(f) = self.take(key) {
            f();
        }
    }
}

#[cfg(target_os = "linux")]
use linux as imp;
#[cfg(target_os = "macos")]
use macos as imp;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use none as imp;
#[cfg(windows)]
use toasts as imp;

/// The desktop's notification server, on the session bus (the
/// specification: specifications.freedesktop.org/notification-spec). In
/// the Flatpak too: its manifest lets it talk to the server.
#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::sync::Arc;

    use anyhow::Context as _;
    use futures_lite::StreamExt as _;
    use zbus::Proxy;
    use zbus::zvariant::Value;

    use super::{Clicks, NO_SERVICE, OnClick};

    const NAME: &str = "org.freedesktop.Notifications";
    const PATH: &str = "/org/freedesktop/Notifications";
    const APP_ID: &str = notekpr_cli::profile::DESKTOP_APP_ID;

    /// The server, once reached (a server that restarts is the same name on
    /// the bus); tried again next time when there was none, and once the
    /// bus is gone.
    static SERVER: tokio::sync::Mutex<Option<Arc<Server>>> = tokio::sync::Mutex::const_new(None);

    async fn server() -> anyhow::Result<Arc<Server>> {
        let mut s = SERVER.lock().await;
        if let Some(s) = &*s {
            return Ok(s.clone());
        }
        let bus = zbus::connection::Builder::session()?
            .method_timeout(std::time::Duration::from_secs(10))
            .build()
            .await
            .context("no session bus")?;
        let server = Arc::new(Server::reach(&bus).await?);
        *s = Some(server.clone());
        Ok(server)
    }

    async fn forget() {
        *SERVER.lock().await = None;
    }

    pub async fn allow() -> Result<(), String> {
        match server().await {
            Ok(_) => Ok(()),
            Err(e) => {
                tracing::warn!("notifications: {e:#}");
                Err(NO_SERVICE.into())
            }
        }
    }

    pub async fn show(title: &str, body: &str, on_click: OnClick) -> Result<(), String> {
        let shown = match server().await {
            Ok(s) => s.show(title, body, on_click).await,
            Err(e) => Err(e),
        };
        shown.map_err(|e| {
            tracing::warn!("a notification couldn't be shown: {e:#}");
            NO_SERVICE.to_string()
        })
    }

    /// A notification as the server knows it: the server, by its own name
    /// on the bus (a server started after it, a restart say, has another),
    /// and its id there (each server's start at 1).
    type Key = (String, u32);

    fn sender(m: &zbus::Message) -> String {
        m.header()
            .sender()
            .map(|s| s.to_string())
            .unwrap_or_default()
    }

    pub struct Server {
        proxy: Proxy<'static>,
        clicks: Arc<Clicks<Key>>,
        /// Held while a notification is sent and its click kept: a click
        /// heard meanwhile waits for it.
        sending: Arc<tokio::sync::Mutex<()>>,
    }

    impl Server {
        /// The server on `bus` (any started for that name later too),
        /// listened to for clicks (and closings) from now on. An error when
        /// there is none.
        pub async fn reach(bus: &zbus::Connection) -> anyhow::Result<Server> {
            let proxy: Proxy<'static> = zbus::proxy::Builder::new(bus)
                .destination(NAME)?
                .path(PATH)?
                .interface(NAME)?
                .cache_properties(zbus::proxy::CacheProperties::No)
                .build()
                .await?;
            let _: Vec<String> = proxy
                .call("GetCapabilities", &())
                .await
                .context("asking the notification server what it shows")?;
            let clicks = Arc::new(Clicks::new());
            let sending = Arc::new(tokio::sync::Mutex::new(()));
            let mut invoked = proxy.receive_signal("ActionInvoked").await?;
            let mut closed = proxy.receive_signal("NotificationClosed").await?;
            let (c, s) = (clicks.clone(), sending.clone());
            // Its own thread, for as long as the bus: no runtime needed.
            std::thread::spawn(move || {
                futures_lite::future::block_on(async move {
                    enum Heard {
                        Clicked(Key),
                        Closed(Key),
                        Other,
                    }
                    loop {
                        let heard = futures_lite::future::or(
                            async {
                                let m = invoked.next().await?;
                                Some(match m.body().deserialize::<(u32, String)>() {
                                    Ok((id, action)) if action == "default" => {
                                        Heard::Clicked((sender(&m), id))
                                    }
                                    _ => Heard::Other,
                                })
                            },
                            async {
                                let m = closed.next().await?;
                                Some(match m.body().deserialize::<(u32, u32)>() {
                                    Ok((id, _reason)) => Heard::Closed((sender(&m), id)),
                                    _ => Heard::Other,
                                })
                            },
                        )
                        .await;
                        // A click on one being sent waits until it is kept.
                        drop(s.lock().await);
                        match heard {
                            Some(Heard::Clicked(id)) => c.clicked(&id),
                            Some(Heard::Closed(id)) => drop(c.take(&id)),
                            Some(Heard::Other) => {}
                            // The bus is gone: reached again next time.
                            None => {
                                forget().await;
                                return;
                            }
                        }
                    }
                });
            });
            Ok(Server {
                proxy,
                clicks,
                sending,
            })
        }

        pub async fn show(&self, title: &str, body: &str, on_click: OnClick) -> anyhow::Result<()> {
            // Asked each time: the server may have been replaced. Where it
            // shows the body as a little HTML (`body-markup`), it is sent
            // escaped; the title is never markup.
            let caps: Vec<String> = self
                .proxy
                .call("GetCapabilities", &())
                .await
                .context("asking the notification server what it shows")?;
            let body = if caps.iter().any(|c| c == "body-markup") {
                escape(body)
            } else {
                body.to_string()
            };
            // "default": what a click on the notification itself does.
            let actions = vec!["default", "Open"];
            let mut hints: HashMap<&str, Value> = HashMap::new();
            hints.insert("desktop-entry", Value::from(APP_ID));
            let _sending = self.sending.lock().await;
            let reply = self
                .proxy
                .call_method(
                    "Notify",
                    &(
                        "notekpr",
                        0u32,
                        APP_ID,
                        title,
                        body.as_str(),
                        actions,
                        hints,
                        -1i32,
                    ),
                )
                .await
                .context("showing a notification")?;
            let id: u32 = reply
                .body()
                .deserialize()
                .context("the notification server's answer")?;
            self.clicks.add((sender(&reply), id), on_click);
            Ok(())
        }
    }

    /// Text as the body's markup shows it as text.
    fn escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    #[cfg(test)]
    mod tests {
        use std::collections::HashMap;
        use std::sync::mpsc;
        use std::time::Duration;

        use zbus::names::BusName;
        use zbus::zvariant::{OwnedValue, Value};
        use zbus::{Connection, connection, interface};

        use super::*;
        use crate::testbus::Bus;

        /// What the server was asked to show.
        #[derive(Debug, PartialEq)]
        struct Shown {
            app: String,
            icon: String,
            title: String,
            body: String,
            actions: Vec<String>,
            entry: String,
        }

        /// A notification server: shows nothing, says what it was asked,
        /// and clicks the first notification, closes the second.
        struct Fake {
            markup: bool,
            shown: mpsc::Sender<Shown>,
            next: u32,
            /// Clicks the first, closes the second, as it shows them.
            auto: bool,
        }

        #[interface(name = "org.freedesktop.Notifications")]
        impl Fake {
            fn get_capabilities(&self) -> Vec<String> {
                let mut caps = vec!["body".to_string(), "actions".into()];
                if self.markup {
                    caps.push("body-markup".into());
                }
                caps
            }

            #[allow(clippy::too_many_arguments)]
            async fn notify(
                &mut self,
                #[zbus(connection)] conn: &Connection,
                app_name: String,
                _replaces_id: u32,
                app_icon: String,
                summary: String,
                body: String,
                actions: Vec<String>,
                hints: HashMap<String, OwnedValue>,
                _expire_timeout: i32,
            ) -> u32 {
                self.next += 1;
                let id = self.next;
                let Value::Str(entry) = &*hints["desktop-entry"] else {
                    panic!("no desktop-entry");
                };
                let entry = entry.to_string();
                let _ = self.shown.send(Shown {
                    app: app_name,
                    icon: app_icon,
                    title: summary,
                    body,
                    actions,
                    entry,
                });
                // Before the answer: the app takes it all the same.
                let to = None::<BusName<'_>>;
                if self.auto && id == 1 {
                    conn.emit_signal(to, PATH, NAME, "ActionInvoked", &(id, "default"))
                        .await
                        .unwrap();
                } else if self.auto {
                    // 2: dismissed.
                    conn.emit_signal(to, PATH, NAME, "NotificationClosed", &(id, 2u32))
                        .await
                        .unwrap();
                }
                id
            }
        }

        /// A server (`markup`, or none at all) on a bus of its own; `f`
        /// talks to it from the app's side.
        fn with_server<T: Send + 'static>(
            markup: Option<bool>,
            f: impl AsyncFnOnce(Connection) -> T + Send + 'static,
        ) -> (T, Vec<Shown>) {
            let bus = Bus::start();
            let address = bus.address().to_string();
            let (shown_tx, shown) = mpsc::channel();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let r = futures_lite::future::block_on(async {
                    let _server = match markup {
                        Some(markup) => Some(
                            connection::Builder::address(address.as_str())?
                                .name(NAME)?
                                .serve_at(
                                    PATH,
                                    Fake {
                                        markup,
                                        shown: shown_tx,
                                        next: 0,
                                        auto: true,
                                    },
                                )?
                                .build()
                                .await?,
                        ),
                        None => None,
                    };
                    let app = connection::Builder::address(address.as_str())?
                        .build()
                        .await?;
                    zbus::Result::Ok(f(app).await)
                });
                let _ = tx.send(r);
            });
            let r = rx
                .recv_timeout(Duration::from_secs(20))
                .expect("still waiting for the notification server")
                .unwrap();
            drop(bus);
            (r, shown.try_iter().collect())
        }

        #[test]
        fn shows_as_notekpr_and_a_click_runs_once() {
            let (clicked, shown) = with_server(Some(true), async |bus| {
                let server = Server::reach(&bus).await.unwrap();
                let (tx, rx) = mpsc::channel();
                let t = tx.clone();
                server
                    .show(
                        "bo mentioned you in Plans",
                        "<b>soon</b> & then",
                        Box::new(move || t.send("first").unwrap()),
                    )
                    .await
                    .unwrap();
                server
                    .show(
                        "second",
                        "closed",
                        Box::new(move || tx.send("second").unwrap()),
                    )
                    .await
                    .unwrap();
                // The click heard, the closing too: only the first ran.
                let first = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                std::thread::sleep(Duration::from_millis(200));
                (
                    first,
                    rx.try_iter().count(),
                    server.clicks.waiting.lock().unwrap().len(),
                )
            });
            assert_eq!(clicked, ("first", 0, 0));
            assert_eq!(
                shown[0],
                Shown {
                    app: "notekpr".into(),
                    icon: APP_ID.into(),
                    title: "bo mentioned you in Plans".into(),
                    // Its markup is shown as text.
                    body: "&lt;b&gt;soon&lt;/b&gt; &amp; then".into(),
                    actions: vec!["default".into(), "Open".into()],
                    entry: APP_ID.into(),
                }
            );
            assert_eq!(shown.len(), 2);
        }

        #[test]
        fn a_body_without_markup_goes_as_it_is() {
            let ((), shown) = with_server(Some(false), async |bus| {
                let server = Server::reach(&bus).await.unwrap();
                server
                    .show("t", "<b>as typed</b> & all", Box::new(|| {}))
                    .await
                    .unwrap();
            });
            assert_eq!(shown[0].body, "<b>as typed</b> & all");
        }

        /// Each server numbers its notifications from 1: after a restart,
        /// a click on the new server's 1 is not one on the old one's.
        #[test]
        fn a_click_is_on_the_server_s_own_notification() {
            let bus = Bus::start();
            let address = bus.address().to_string();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let r = futures_lite::future::block_on(async {
                    let (shown, _) = mpsc::channel();
                    let serve = async |shown| {
                        connection::Builder::address(address.as_str())?
                            .name(NAME)?
                            .serve_at(
                                PATH,
                                Fake {
                                    markup: false,
                                    shown,
                                    next: 0,
                                    auto: false,
                                },
                            )?
                            .build()
                            .await
                    };
                    let first = serve(shown.clone()).await?;
                    let app = connection::Builder::address(address.as_str())?
                        .build()
                        .await?;
                    let server = Server::reach(&app).await.unwrap();
                    let (clicked, heard) = mpsc::channel();
                    let c = clicked.clone();
                    let show = |title: &'static str, c: mpsc::Sender<&'static str>| {
                        server.show(title, "", Box::new(move || c.send(title).unwrap()))
                    };
                    show("before", c).await.unwrap();
                    first.release_name(NAME).await?;
                    let second = serve(shown).await?;
                    show("after", clicked).await.unwrap();
                    second
                        .emit_signal(
                            None::<BusName<'_>>,
                            PATH,
                            NAME,
                            "ActionInvoked",
                            &(1u32, "default"),
                        )
                        .await?;
                    let one = heard.recv_timeout(Duration::from_secs(5)).unwrap();
                    std::thread::sleep(Duration::from_millis(200));
                    zbus::Result::Ok((one, heard.try_iter().count()))
                });
                let _ = tx.send(r);
            });
            let r = rx
                .recv_timeout(Duration::from_secs(20))
                .expect("still waiting for the notification servers")
                .unwrap();
            drop(bus);
            assert_eq!(r, ("after", 0));
        }

        #[test]
        fn no_server_is_an_error() {
            let (reached, shown) = with_server(None, async |bus| Server::reach(&bus).await.is_ok());
            assert!(!reached);
            assert!(shown.is_empty());
        }
    }
}

/// Toasts. Windows shows an app's toasts once it knows the app by its id:
/// it is registered for this user (`HKEY_CURRENT_USER\Software\Classes\
/// AppUserModelId\md.notekpr.Notekpr`: its name, and its icon, written
/// beside the app's data) before the first one. The uninstaller removes it.
#[cfg(windows)]
mod toasts {
    use std::sync::Mutex;

    use ::windows::Data::Xml::Dom::XmlDocument;
    use ::windows::Foundation::{DateTime, IReference, PropertyValue};
    use ::windows::UI::Notifications::{
        NotificationSetting, ToastNotification, ToastNotificationManager,
    };
    use ::windows::core::{HRESULT, HSTRING, Interface as _, h};
    use tauri_winrt_notification::Toast;

    use super::OnClick;

    const APP_ID: &str = notekpr_cli::profile::DESKTOP_APP_ID;
    const ICON: &[u8] = include_bytes!("../icons/128x128.png");

    fn register() -> Result<(), String> {
        let icon = notekpr_cli::profile::desktop_roots()
            .map_err(|e| e.to_string())?
            .1
            .join("notifications.png");
        if std::fs::read(&icon).ok().as_deref() != Some(ICON) {
            if let Some(dir) = icon.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            std::fs::write(&icon, ICON).map_err(|e| format!("notekpr's icon: {e}"))?;
        }
        let key = windows_registry::CURRENT_USER
            .create(format!(r"Software\Classes\AppUserModelId\{APP_ID}"))
            .and_then(|k| {
                k.set_string("DisplayName", "notekpr")?;
                k.set_string("IconUri", icon.to_string_lossy().as_ref())
            });
        key.map_err(|e| format!("registering notekpr's notifications: {e}"))
    }

    /// "Not found": what asking for the setting answers while Windows
    /// knows nothing of the app's notifications yet.
    const NOT_FOUND: HRESULT = HRESULT(0x8007_0490_u32 as i32);

    /// One notification, shown to nobody and taken back at once: until the
    /// app has sent one, Windows can't say whether it shows them (as
    /// Microsoft's own ToastNotificationManagerCompat does it).
    fn first() -> ::windows::core::Result<()> {
        let xml = XmlDocument::new()?;
        xml.LoadXml(h!(
            r#"<toast><visual><binding template="ToastGeneric"><text>notekpr</text></binding></visual></toast>"#
        ))?;
        let toast = ToastNotification::CreateToastNotification(&xml)?;
        toast.SetSuppressPopup(true)?;
        toast.SetTag(h!("notekpr-first"))?;
        toast.SetGroup(h!("notekpr"))?;
        // Gone by itself in 15 seconds, should taking it back fail.
        // (Windows counts 100 ns from 1601.)
        let soon = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            + std::time::Duration::from_secs(15);
        let ticks = (soon.as_nanos() / 100) as i64 + 116_444_736_000_000_000;
        let expiry: IReference<DateTime> = PropertyValue::CreateDateTime(DateTime {
            UniversalTime: ticks,
        })?
        .cast()?;
        toast.SetExpirationTime(&expiry)?;
        let id = HSTRING::from(APP_ID);
        ToastNotificationManager::CreateToastNotifierWithId(&id)?.Show(&toast)?;
        ToastNotificationManager::History()?.RemoveGroupedTagWithId(
            h!("notekpr-first"),
            h!("notekpr"),
            &id,
        )
    }

    pub async fn allow() -> Result<(), String> {
        register()?;
        if let Err(e) = first() {
            tracing::warn!("Windows' notifications, the first: {e}");
        }
        let setting = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))
            .and_then(|n| n.Setting());
        match setting {
            Ok(NotificationSetting::Enabled) => Ok(()),
            Ok(_) => Err(
                "Windows doesn't allow notifications from notekpr: turn them on in Settings → System → Notifications, then try again."
                    .into(),
            ),
            // Not known yet: the first real one tells.
            Err(e) if e.code() == NOT_FOUND => Ok(()),
            Err(e) => Err(format!("Windows' notifications: {e}")),
        }
    }

    pub async fn show(title: &str, body: &str, on_click: OnClick) -> Result<(), String> {
        register()?;
        let on_click = Mutex::new(Some(on_click));
        Toast::new(APP_ID)
            .title(title)
            .text1(body)
            .on_activated(move |_| {
                if let Some(f) = on_click.lock().unwrap().take() {
                    f();
                }
                Ok(())
            })
            .show()
            .map_err(|e| format!("Windows' notifications: {e}"))
    }
}

/// Notification Center (the UserNotifications framework). Only in the app
/// itself (`notekpr.app`): macOS knows it by its bundle; a development run
/// outside one has none. macOS asks the person once whether to allow
/// them, when they are turned on.
#[cfg(target_os = "macos")]
mod macos {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, Once};

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
    use objc2::{AllocAnyThread, define_class, msg_send};
    use objc2_foundation::{NSBundle, NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
        UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
        UNNotificationSound, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };

    use super::{Clicks, OnClick};

    static CLICKS: Clicks<String> = Clicks::new();
    static DELEGATE: Once = Once::new();

    const NOT_HERE: &str =
        "notekpr shows notifications only when it runs as the app (notekpr.app).";
    const NOT_ALLOWED: &str = "macOS doesn't allow notifications from notekpr: allow them in System Settings → Notifications → notekpr, then try again.";

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and this has no
        // Drop.
        #[unsafe(super(NSObject))]
        #[name = "NotekprNotifications"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            // Shown even while notekpr is the app in front: the window
            // asked for it only when it isn't in front itself.
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &UNNotification,
                done: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                done.call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List
                    | UNNotificationPresentationOptions::Sound,));
            }

            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                done: &block2::DynBlock<dyn Fn()>,
            ) {
                let id = response.notification().request().identifier().to_string();
                CLICKS.clicked(&id);
                done.call(());
            }
        }
    );

    impl Delegate {
        fn new() -> Retained<Self> {
            let this = Self::alloc().set_ivars(());
            // SAFETY: NSObject's init.
            unsafe { msg_send![super(this), init] }
        }
    }

    /// The center, which hears of clicks from now on; none outside the
    /// app's bundle (it would end the program).
    fn center() -> Option<Retained<UNUserNotificationCenter>> {
        NSBundle::mainBundle().bundleIdentifier()?;
        let center = UNUserNotificationCenter::currentNotificationCenter();
        DELEGATE.call_once(|| {
            let delegate = Delegate::new();
            center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            // The center keeps it weakly: it lives as long as the app.
            std::mem::forget(delegate);
        });
        Some(center)
    }

    /// Ask (macOS asks the person the first time); `answer` hears whether
    /// they are allowed.
    fn request(answer: impl FnOnce(bool) + Send + 'static) -> Result<(), String> {
        let center = center().ok_or(NOT_HERE)?;
        let answer = Mutex::new(Some(answer));
        let block = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
            if let Some(f) = answer.lock().unwrap().take() {
                f(granted.as_bool());
            }
        });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &block,
        );
        Ok(())
    }

    pub async fn allow() -> Result<(), String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        request(move |granted| {
            let _ = tx.send(granted);
        })?;
        match rx.await {
            Ok(true) => Ok(()),
            _ => Err(NOT_ALLOWED.into()),
        }
    }

    fn add(title: &str, body: &str, on_click: OnClick) -> Result<(), String> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let center = center().ok_or(NOT_HERE)?;
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        content.setSound(Some(&UNNotificationSound::defaultSound()));
        let id = format!("notekpr-{}", NEXT.fetch_add(1, Ordering::Relaxed));
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&id),
            &content,
            None,
        );
        CLICKS.add(id, on_click);
        center.addNotificationRequest_withCompletionHandler(&request, None);
        Ok(())
    }

    pub async fn show(title: &str, body: &str, on_click: OnClick) -> Result<(), String> {
        add(title, body, on_click)
    }
}

/// Elsewhere: none.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod none {
    use super::OnClick;

    const NONE: &str = "notekpr can't show notifications on this system.";

    pub async fn allow() -> Result<(), String> {
        Err(NONE.into())
    }

    pub async fn show(_title: &str, _body: &str, _on_click: OnClick) -> Result<(), String> {
        Err(NONE.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_text_is_cut_short() {
        assert_eq!(cap("short", 10), "short");
        assert_eq!(cap("abcdef", 3), "abc…");
        assert_eq!(cap("ééééé", 2), "éé…");
    }

    #[test]
    fn a_click_runs_once_and_only_the_last_are_kept() {
        let clicks = Clicks::new();
        let (tx, rx) = std::sync::mpsc::channel();
        for i in 0..KEPT + 2 {
            let tx = tx.clone();
            clicks.add(i, Box::new(move || tx.send(i).unwrap()));
        }
        // The oldest two were dropped.
        clicks.clicked(&0);
        clicks.clicked(&1);
        clicks.clicked(&2);
        clicks.clicked(&2);
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![2]);
        assert!(clicks.take(&3).is_some());
        assert!(clicks.take(&3).is_none());
    }
}
