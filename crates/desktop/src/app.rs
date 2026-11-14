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
use crate::paths::Paths;
use crate::settings::Settings;
use crate::status::Updates;

pub struct AppState {
    pub profile: String,
    pub paths: Paths,
    pub settings: Mutex<Settings>,
    pub updates: Arc<Updates>,
    pub folder: Folder,
    /// Cancels the sign-in in progress.
    pub sign_in: Mutex<Option<oneshot::Sender<()>>>,
    /// The folder last picked in the system's dialog, by number
    /// (`choose_folder`, `folder_start`).
    pub picked: Mutex<Option<(u64, PathBuf)>>,
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

/// Show the window and bring it to the front.
pub fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Quit, after the Markdown folder stopped the way `notekpr sync` does.
fn quit(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.hide();
        }
        app.state::<Arc<AppState>>().folder.stop().await;
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
        std::process::Command::new("dbus-send")
            .args([
                "--session",
                "--print-reply",
                "--dest=org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus.NameHasOwner",
                "string:org.kde.StatusNotifierWatcher",
            ])
            .output()
            .map(|o| {
                o.status.success() && String::from_utf8_lossy(&o.stdout).contains("boolean true")
            })
            .unwrap_or(false)
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
    TrayIconBuilder::with_id("main")
        .icon(icon)
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
    let settings = Settings::load(&paths.settings_file())?;
    let webview_dir = paths.webview(settings.server.as_deref());
    let updates = Arc::new(Updates::default());
    let folder = Folder::new(paths.folder(), updates.clone());

    let mut context = tauri::generate_context!();
    context.config_mut().app.security.csp = Some(Csp::Policy(csp(settings.server.as_deref())));
    let default_profile = args.profile == "default";

    let mut single = tauri_plugin_single_instance::Builder::new().callback(|app, argv, _cwd| {
        // Started again: show the window — unless it was the autostart of
        // an app that is already running.
        if !argv.iter().any(|a| a == "--hidden") {
            show_window(app);
        }
    });
    let mut autostart_args = vec!["--hidden".to_string()];
    let mut autostart = tauri_plugin_autostart::Builder::new();
    if !default_profile {
        single = single.dbus_id(format!(
            "md.notekpr.Notekpr.p_{}",
            args.profile.replace('-', "_")
        ));
        autostart_args.extend(["--profile".to_string(), args.profile.clone()]);
        autostart = autostart.app_name(format!("notekpr ({})", args.profile));
    }

    let state = Arc::new(AppState {
        profile: args.profile.clone(),
        paths: paths.clone(),
        settings: Mutex::new(settings),
        updates,
        folder,
        sign_in: Mutex::new(None),
        picked: Mutex::new(None),
    });

    let hidden = args.hidden;
    let app = tauri::Builder::default()
        .plugin(single.build())
        .plugin(autostart.args(autostart_args).build())
        .plugin(tauri_plugin_dialog::init())
        .manage(state.clone())
        .invoke_handler(tauri::generate_handler![
            crate::commands::desktop_info,
            crate::commands::set_server,
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
            // The Markdown folder goes on where it was.
            let state = handle.state::<Arc<AppState>>().inner().clone();
            tauri::async_runtime::spawn(async move {
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
        assert!(csp(None).contains("connect-src 'self' ipc: http://ipc.localhost;"));
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
