//! The app: its window, tray and plugins, and what happens when the window
//! closes or the app quits.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::utils::config::{BackgroundThrottlingPolicy, Csp};
use tauri::webview::{DownloadEvent, NewWindowResponse};
use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tokio::sync::oneshot;

use crate::Args;
use crate::folder::Folder;
use crate::links;
use crate::local::Local;
use crate::paths::Paths;
use crate::settings::Settings;
use crate::status::Updates;

pub struct AppState {
    pub profile: String,
    pub paths: Paths,
    pub settings: Mutex<Settings>,
    pub updates: Arc<Updates>,
    pub folder: Folder,
    /// Notes only on this computer: the server inside the app. Decided at
    /// start, as the server is (switching starts the app again).
    pub local: Option<Local>,
    /// Cancels the sign-in in progress.
    pub sign_in: Mutex<Option<oneshot::Sender<()>>>,
    /// The folder last picked in the system's dialog, by number
    /// (`choose_folder`, `folder_start`).
    pub picked: Mutex<Option<(u64, PathBuf)>>,
    /// What starts this profile at login (the autostart entry's command).
    pub autostart_line: Vec<String>,
}

/// The window's Content-Security-Policy: the server's own, but the API may
/// be reached at the chosen server (and Tauri's IPC).
pub fn csp(server: Option<&str>) -> String {
    let mut connect = vec![
        "'self'".to_string(),
        "ipc:".into(),
        "http://ipc.localhost".into(),
    ];
    if let Some(u) = server.and_then(|s| url::Url::parse(s).ok()) {
        let origin = u.origin().ascii_serialization();
        let ws = origin
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1);
        connect.push(origin);
        connect.push(ws);
    }
    format!(
        "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; \
         img-src 'self' blob: data:; media-src 'self' blob:; font-src 'self'; connect-src {}; \
         object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'self'",
        connect.join(" ")
    )
}

/// One argument as an autostart entry's `Exec` line needs it (the
/// desktop entry spec; `auto-launch` joins them with spaces as they are):
/// quoted when it holds more than plain characters, a folder's path with a
/// space say.
fn exec_arg(a: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "/._-=,+:@".contains(c);
    if !a.is_empty() && a.chars().all(plain) {
        return a.to_string();
    }
    let mut q = String::from("\"");
    for c in a.chars() {
        match c {
            // Escaped for the quoting, then the backslash itself for the
            // entry's string value.
            '"' | '`' | '$' => q.extend(['\\', '\\', c]),
            '\\' => q.push_str("\\\\\\\\"),
            c => q.push(c),
        }
    }
    q.push('"');
    q
}

/// Show the window and bring it to the front.
pub fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

impl AppState {
    /// Where the window and the folder reach the API: the server chosen, or
    /// the app's own with notes only on this computer.
    pub fn api_server(&self) -> Option<String> {
        match &self.local {
            Some(local) => Some(local.origin().to_string()),
            None => self.settings.lock().unwrap().remote().map(str::to_string),
        }
    }

    /// Stop what runs before the app quits or starts again: the Markdown
    /// folder the way `notekpr sync` does, then the app's own server it
    /// may still be sending to.
    pub async fn stop(&self) {
        self.folder.stop().await;
        if let Some(local) = &self.local {
            local.stop().await;
        }
    }
}

/// Quit, after what runs stopped ([`AppState::stop`]).
fn quit(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.hide();
        }
        app.state::<Arc<AppState>>().stop().await;
        app.exit(0);
    });
}

/// Whether a tray icon shows now. On Linux that takes a StatusNotifier host
/// (most desktops; GNOME with an extension): without one there is no icon,
/// and a window closed to the tray could not come back. Asked when it
/// matters — the host may come later than the app (at login), and the icon
/// appears then.
pub fn tray_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        let has_host = || -> zbus::Result<bool> {
            let bus = zbus::blocking::Connection::session()?;
            let dbus = zbus::blocking::fdo::DBusProxy::new(&bus)?;
            Ok(dbus.name_has_owner("org.kde.StatusNotifierWatcher".try_into()?)?)
        };
        has_host().unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open notekpr", true, None::<&str>)?;
    let folder = MenuItem::with_id(
        app,
        "folder",
        "Open the Markdown folder",
        true,
        None::<&str>,
    )?;
    let settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit notekpr", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &open,
            &folder,
            &settings,
            &PredefinedMenuItem::separator(app)?,
            &quit_item,
        ],
    )?;
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    let mut tray = TrayIconBuilder::with_id("main");
    if let Some(dir) = crate::flatpak::tray_icon_dir() {
        tray = tray.temp_dir_path(dir);
    }
    tray.icon(icon)
        .tooltip("notekpr")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id().as_ref() {
            "open" => show_window(app),
            "folder" => {
                let state = app.state::<Arc<AppState>>();
                match state.folder.configured() {
                    Some(dir) => {
                        let _ = tauri_plugin_opener::open_path(dir, None::<&str>);
                    }
                    None => {
                        state.updates.update(|s| s.show_settings += 1);
                        show_window(app);
                    }
                }
            }
            "settings" => {
                app.state::<Arc<AppState>>()
                    .updates
                    .update(|s| s.show_settings += 1);
                show_window(app);
            }
            "quit" => quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = e
            {
                show_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// A file the window downloads (an export): into the Downloads folder,
/// under a name nothing has yet.
fn download_path(downloads: &std::path::Path, suggested: &std::path::Path) -> PathBuf {
    let name = suggested
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "notekpr-download".into());
    let first = downloads.join(&name);
    if first.symlink_metadata().is_err() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    (2..)
        .map(|n| downloads.join(format!("{stem} ({n}){ext}")))
        .find(|p| p.symlink_metadata().is_err())
        .expect("a free name")
}

pub fn run(args: Args) -> Result<()> {
    let paths = Paths::for_profile(&args.profile)?;
    paths.private_data()?;
    let settings = Settings::load(&paths.settings_file())?;
    // Notes only on this computer: the app's own server, its address taken
    // now (it starts once this is the only copy of the app: `setup`).
    let local = if settings.this_computer {
        Some(Local::bind(paths.this_computer())?)
    } else {
        None
    };
    let (webview_dir, api_server) = match &local {
        Some(local) => (paths.webview_here(), Some(local.origin().to_string())),
        None => (
            paths.webview(settings.server.as_deref()),
            settings.server.clone(),
        ),
    };
    let updates = Arc::new(Updates::default());
    let folder = Folder::new(paths.folder(), updates.clone());

    let mut context = tauri::generate_context!();
    context.config_mut().app.security.csp = Some(Csp::Policy(csp(api_server.as_deref())));
    // The window's app id (Wayland) and class (X11) come from the program's
    // name: make it the app's identifier, which its desktop entry and icons
    // are named after (packaging/desktop), so desktops find them.
    #[cfg(target_os = "linux")]
    glib::set_prgname(Some(context.config().identifier.as_str()));
    let default_profile = args.profile == "default";

    // A copy with folders of its own runs beside the installed app, with an
    // autostart entry of its own that starts it with the same folders.
    let own = crate::paths::own_folders();
    let mut single = tauri_plugin_single_instance::Builder::new().callback(|app, argv, _cwd| {
        // Started again: show the window — unless it was the autostart of
        // an app that is already running.
        if !argv.iter().any(|a| a == "--hidden") {
            show_window(app);
        }
    });
    let mut autostart_args = vec!["--hidden".to_string()];
    let mut autostart = tauri_plugin_autostart::Builder::new();
    let mut id = vec![];
    let mut name = vec![];
    if let Some((tag, folders)) = &own {
        id.push(format!("d{tag}"));
        name.push(format!("development {tag}"));
        autostart_args.extend(folders.iter().cloned());
    }
    if !default_profile {
        id.push(format!("p_{}", args.profile.replace('-', "_")));
        name.push(args.profile.clone());
        autostart_args.extend(["--profile".to_string(), args.profile.clone()]);
    }
    if !id.is_empty() {
        single = single.dbus_id(format!("md.notekpr.Notekpr.{}", id.join(".")));
        autostart = autostart.app_name(format!("notekpr ({})", name.join(", ")));
    }

    let state = Arc::new(AppState {
        profile: args.profile.clone(),
        paths: paths.clone(),
        settings: Mutex::new(settings),
        updates,
        folder,
        local,
        sign_in: Mutex::new(None),
        picked: Mutex::new(None),
        autostart_line: std::iter::once("notekpr-desktop".to_string())
            .chain(autostart_args.iter().cloned())
            .collect(),
    });

    let hidden = args.hidden;
    let app = tauri::Builder::default()
        .plugin(single.build())
        .plugin(
            autostart
                .args(autostart_args.iter().map(|a| exec_arg(a)))
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .manage(state.clone())
        .invoke_handler(tauri::generate_handler![
            crate::commands::desktop_info,
            crate::commands::set_server,
            crate::commands::use_this_computer,
            crate::commands::local_start,
            crate::commands::local_session,
            crate::commands::sign_in,
            crate::commands::cancel_sign_in,
            crate::commands::set_close_to_tray,
            crate::commands::set_autostart,
            crate::commands::open_external,
            crate::commands::choose_folder,
            crate::commands::folder_start,
            crate::commands::folder_off,
            crate::commands::folder_retry,
            crate::commands::reveal_folder,
            crate::commands::session_changed,
            crate::commands::signed_out,
            crate::commands::forget_computer,
            crate::commands::next_event,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let downloads = app
                .path()
                .download_dir()
                .or_else(|_| app.path().home_dir())
                .unwrap_or_else(|_| PathBuf::from("."));
            let nav = handle.clone();
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App(PathBuf::new()))
                .title("notekpr")
                .inner_size(1200.0, 820.0)
                .min_inner_size(360.0, 480.0)
                .visible(!hidden)
                .data_directory(webview_dir.clone())
                // Windows: https://tauri.localhost, a secure origin like the
                // others (changing it later would strand the window's storage).
                .use_https_scheme(true)
                // The app's own drag and drop (files into notes, the file
                // tree) instead of Tauri's.
                .disable_drag_drop_handler()
                // Hidden in the tray, it keeps syncing.
                .background_throttling(BackgroundThrottlingPolicy::Disabled)
                .on_navigation(move |url| {
                    if links::is_app(url) {
                        return true;
                    }
                    if links::is_external(url) {
                        let _ = tauri_plugin_opener::open_url(url.as_str(), None::<&str>);
                    }
                    let _ = &nav;
                    false
                })
                .on_new_window(|url, _| {
                    if links::is_external(&url) {
                        let _ = tauri_plugin_opener::open_url(url.as_str(), None::<&str>);
                    }
                    NewWindowResponse::Deny
                })
                .on_download(move |_, e| {
                    match e {
                        DownloadEvent::Requested { destination, .. } => {
                            *destination = download_path(&downloads, destination);
                        }
                        // Never its name or where: they can say what is in it.
                        DownloadEvent::Finished { success: false, .. } => {
                            tracing::warn!("a download didn't finish");
                        }
                        _ => {}
                    }
                    true
                })
                .build()?;
            if let Err(e) = build_tray(&handle) {
                tracing::warn!("no tray icon: {e}");
            }
            // Started hidden (at login): the window shows after all if no
            // tray comes to bring it back from.
            if hidden {
                let h = handle.clone();
                std::thread::spawn(move || {
                    let found = (0..30).any(|_| {
                        tray_available() || {
                            std::thread::sleep(std::time::Duration::from_millis(500));
                            false
                        }
                    });
                    if !found {
                        show_window(&h);
                    }
                });
            }
            // The Markdown folder goes on where it was — with notes only
            // on this computer, at the app's own server's new address,
            // once it runs.
            let state = handle.state::<Arc<AppState>>().inner().clone();
            let followed = match &state.local {
                Some(local) => state.folder.follow_server(local.origin()),
                None => Ok(()),
            };
            tauri::async_runtime::spawn(async move {
                if let Some(local) = &state.local
                    && let Err(e) = local.start().await
                {
                    // The window says so, asking for a session.
                    tracing::error!("{e}");
                    return;
                }
                if let Err(e) = followed {
                    tracing::warn!("the Markdown folder can't reach this computer's notes: {e:#}");
                    return;
                }
                if let Err(e) = state.folder.start().await {
                    tracing::warn!("the Markdown folder didn't start: {e:#}");
                }
            });
            Ok(())
        })
        .on_window_event(|window, e| {
            if let WindowEvent::CloseRequested { api, .. } = e {
                let state = window.state::<Arc<AppState>>();
                if state.settings.lock().unwrap().close_to_tray && tray_available() {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(context)
        .context("starting the app")?;
    app.run(|app, e| {
        if let RunEvent::ExitRequested {
            code: None, api, ..
        } = e
        {
            // The window closed for good: stop the folder's sync properly
            // first.
            api.prevent_exit();
            quit(app);
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_security_policy() {
        let c = csp(Some("https://notes.example.com/sub"));
        assert!(c.contains(
            "connect-src 'self' ipc: http://ipc.localhost https://notes.example.com wss://notes.example.com;"
        ));
        assert!(c.contains("script-src 'self' 'wasm-unsafe-eval';"));
        let c = csp(Some("http://localhost:8080"));
        assert!(c.contains("http://localhost:8080 ws://localhost:8080;"));
        // The app's own server: that address and port only.
        let c = csp(Some("http://127.0.0.1:41234"));
        assert!(c.contains(
            "connect-src 'self' ipc: http://ipc.localhost http://127.0.0.1:41234 ws://127.0.0.1:41234;"
        ));
        assert!(csp(None).contains("connect-src 'self' ipc: http://ipc.localhost;"));
    }

    #[test]
    fn autostart_arguments_survive_the_entry() {
        assert_eq!(exec_arg("--hidden"), "--hidden");
        assert_eq!(
            exec_arg("/home/ana/dev/notekpr/data/desktop/config"),
            "/home/ana/dev/notekpr/data/desktop/config"
        );
        assert_eq!(
            exec_arg("/home/ana/My notes/config"),
            "\"/home/ana/My notes/config\""
        );
        assert_eq!(exec_arg("a\"b$c"), "\"a\\\\\"b\\\\$c\"");
        assert_eq!(exec_arg("a\\b"), "\"a\\\\\\\\b\"");
        assert_eq!(exec_arg(""), "\"\"");
    }

    #[test]
    fn downloads_get_free_names() {
        let d = tempfile::tempdir().unwrap();
        let p = download_path(d.path(), std::path::Path::new("Family.zip"));
        assert_eq!(p, d.path().join("Family.zip"));
        std::fs::write(&p, b"").unwrap();
        assert_eq!(
            download_path(d.path(), std::path::Path::new("/somewhere/Family.zip")),
            d.path().join("Family (2).zip")
        );
        assert_eq!(
            download_path(d.path(), std::path::Path::new("")),
            d.path().join("notekpr-download")
        );
    }
}
