//! The app: its window, tray and plugins, and what happens when the window
//! closes or the app quits.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use clap::Parser;
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
use crate::settings::{Account, Settings};
use crate::status::Updates;

pub struct AppState {
    pub profile: String,
    pub paths: Paths,
    pub settings: Mutex<Settings>,
    /// The open account, fixed when the app starts (none: not chosen yet):
    /// opening another starts the app again.
    pub open: Option<Account>,
    /// Set once a switch to another account began: nothing about accounts
    /// changes after it.
    pub switching: std::sync::atomic::AtomicBool,
    pub updates: Arc<Updates>,
    /// The open account's Markdown folder.
    pub folder: Folder,
    /// Notes only on this computer: the server inside the app. Decided at
    /// start, as the server is (switching starts the app again).
    pub local: Option<Local>,
    /// Cancels the sign-in in progress.
    pub sign_in: Mutex<Option<oneshot::Sender<()>>>,
    /// The folders last picked in the system's dialog, by number
    /// (`choose_folder`, `folder_start`, `folder_bring`).
    pub picked: Mutex<crate::commands::Picks>,
    /// Cancels waiting for the CLI to stop (`folder_take_from_cli`).
    pub cli_take: Mutex<Option<oneshot::Sender<()>>>,
    /// What starts this profile at login (the autostart entry's command).
    pub autostart_line: Vec<String>,
    /// Whether [`AppState::stop`] ran: the app is quitting or starting
    /// again. Held while it runs, so another stop waits for it.
    pub stopped: tokio::sync::Mutex<bool>,
}

/// The window's Content-Security-Policy: the server's own, but the API may
/// be reached at the chosen server (and Tauri's IPC).
pub fn csp(server: Option<&str>) -> String {
    // Tauri's IPC: `ipc:` on Linux and macOS; on Windows
    // https://ipc.localhost, as the window is https://tauri.localhost
    // (`use_https_scheme`).
    let mut connect = vec![
        "'self'".to_string(),
        "ipc:".into(),
        "https://ipc.localhost".into(),
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

/// One argument as the system's autostart entry needs it, which
/// `auto-launch` writes the arguments into as they are: [`desktop_entry_arg`],
/// on Windows [`command_line_arg`], on macOS [`plist_arg`].
fn exec_arg(a: &str) -> String {
    if cfg!(windows) {
        command_line_arg(a)
    } else if cfg!(target_os = "macos") {
        plist_arg(a)
    } else {
        desktop_entry_arg(a)
    }
}

/// One argument as a Launch Agent needs it (macOS: each argument is a
/// `<string>` of its `ProgramArguments`, nothing splits them again): XML's
/// `&`, `<` and `>` escaped.
fn plist_arg(a: &str) -> String {
    a.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// One argument as a Windows command line needs it (the `Run` key's
/// value; how the program splits it again: `CommandLineToArgvW`'s rules):
/// quoted when it holds a space or a quote, with backslashes doubled only
/// before a quote.
fn command_line_arg(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) {
        return a.to_string();
    }
    let mut q = String::from("\"");
    let mut backslashes = 0;
    for c in a.chars() {
        if c == '\\' {
            backslashes += 1;
        } else {
            if c == '"' {
                // The backslashes before it doubled, and one for the quote.
                q.extend(std::iter::repeat_n('\\', backslashes + 1));
            }
            backslashes = 0;
        }
        q.push(c);
    }
    // And those before the closing quote.
    q.extend(std::iter::repeat_n('\\', backslashes));
    q.push('"');
    q
}

/// One argument as an autostart entry's `Exec` line needs it (the desktop
/// entry spec): quoted when it holds more than plain characters, a
/// folder's path with a space say.
fn desktop_entry_arg(a: &str) -> String {
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
            None => self.open.as_ref().and_then(|a| a.server.clone()),
        }
    }

    /// Stop what runs before the app quits or starts again: taking over
    /// the CLI's folder cancelled (its background sync put back as it
    /// was) and any change of folder let finish, the Markdown folder the
    /// way `notekpr sync` does, then the app's own server it may still be
    /// sending to. Once: called while it runs (quitting twice, the system
    /// ending the app meanwhile), it waits for it.
    pub async fn stop(&self) {
        let mut stopped = self.stopped.lock().await;
        if *stopped {
            return;
        }
        if let Some(tx) = self.cli_take.lock().unwrap().take() {
            let _ = tx.send(());
        }
        self.folder
            .settled(std::time::Duration::from_secs(20))
            .await;
        self.folder.stop().await;
        if let Some(local) = &self.local {
            local.stop().await;
        }
        *stopped = true;
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

/// Whether this copy runs from where it is only for now (macOS): a
/// downloaded app that hasn't been moved yet runs from a temporary copy
/// (App Translocation), which is gone at the next login.
#[cfg(target_os = "macos")]
pub fn runs_from_temporary_copy() -> bool {
    std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .is_ok_and(|p| p.to_string_lossy().contains("/AppTranslocation/"))
}

/// The menu bar's menus (macOS): Tauri's own, but with the app's Quit. The
/// system's (terminate) ends the app at once; the app's stops the
/// Markdown folder's sync first, as Quit in the tray does.
#[cfg(target_os = "macos")]
fn menu_bar(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::default(app)?;
    // The app's menu, the first: Quit is its last item.
    if let Some(first) = menu.items()?.first().and_then(|m| m.as_submenu().cloned()) {
        if let Some(last) = first
            .items()?
            .into_iter()
            .last()
            .filter(|i| i.as_predefined_menuitem().is_some())
        {
            first.remove(&last)?;
        }
        first.append(&MenuItem::with_id(
            app,
            "menu-quit",
            "Quit notekpr",
            true,
            Some("CmdOrCtrl+Q"),
        )?)?;
    }
    Ok(menu)
}

/// Only one copy of the app runs per profile (and folders of its own): the
/// single-instance plugin, under the app's identifier with what sets this
/// copy apart (`id`) — on Linux as its D-Bus name; elsewhere in the
/// identifier itself, which the plugin names its lock after (Windows: a
/// mutex and a window). A copy started again hands its arguments over and
/// ends.
fn single_copy(
    context: &mut tauri::Context<tauri::Wry>,
    id: &[String],
) -> tauri_plugin_single_instance::Builder<tauri::Wry> {
    let single = tauri_plugin_single_instance::Builder::new().callback(|app, argv, _cwd| {
        match Args::try_parse_from(&argv) {
            Ok(a) if a.quit => quit(app),
            // The autostart of an app that is already running.
            Ok(a) if a.hidden => {}
            // Started again: show the window.
            _ => show_window(app),
        }
    });
    if id.is_empty() {
        return single;
    }
    let named = format!("{}.{}", context.config().identifier, id.join("."));
    #[cfg(target_os = "linux")]
    return single.dbus_id(named);
    #[cfg(not(target_os = "linux"))]
    {
        context.config_mut().identifier = named;
        single
    }
}

/// `--quit`: the copy running with the same profile and folders gets the
/// arguments ([`single_copy`]) and quits; this process ends there. Built,
/// it is the only copy: there is nothing to quit.
fn quit_running(
    context: tauri::Context<tauri::Wry>,
    single: tauri_plugin_single_instance::Builder<tauri::Wry>,
) -> Result<()> {
    let _only = tauri::Builder::default()
        .plugin(single.build())
        .build(context)
        .context("starting the app")?;
    Ok(())
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
    // macOS draws the menu bar's icons itself, in the menu bar's colours,
    // from a black shape (a template): the icon's diamond alone.
    let macos = cfg!(target_os = "macos");
    let icon = tauri::image::Image::from_bytes(if macos {
        include_bytes!("../icons/tray-template.png")
    } else {
        include_bytes!("../icons/tray.png")
    })?;
    let mut tray = TrayIconBuilder::with_id("main");
    if let Some(dir) = crate::flatpak::tray_icon_dir() {
        tray = tray.temp_dir_path(dir);
    }
    tray.icon(icon)
        .icon_as_template(macos)
        .tooltip("notekpr")
        .menu(&menu)
        // A click opens the window, but in macOS's menu bar the menu, as
        // its icons do (the window: the Dock's icon).
        .show_menu_on_left_click(macos)
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
        .on_tray_icon_event(move |tray, e| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = e
                && !macos
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
    // A copy with folders of its own runs beside the installed app, with an
    // autostart entry of its own that starts it with the same folders.
    let own = crate::paths::own_folders();
    let default_profile = args.profile == "default";
    let mut context = tauri::generate_context!();
    // The window's app id (Wayland) and class (X11) come from the program's
    // name: make it the app's identifier, which its desktop entry and icons
    // are named after (packaging/desktop), so desktops find them.
    #[cfg(target_os = "linux")]
    glib::set_prgname(Some(context.config().identifier.as_str()));

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
    let single = single_copy(&mut context, &id);
    if !id.is_empty() {
        autostart = autostart.app_name(format!("notekpr ({})", name.join(", ")));
    }
    if args.quit {
        return quit_running(context, single);
    }

    let paths = Paths::for_profile(&args.profile)?;
    paths.private_data()?;
    let loaded = Settings::load(&paths.settings_file())?;
    // The open account (an older file becomes a list of one); written once
    // this is the only copy of the app (`setup`).
    let mut settings = loaded.clone();
    let open = settings.settle(crate::paths::legacy_account_id);
    let settled = settings != loaded;
    // Notes only on this computer: the app's own server, its address taken
    // now (it starts once this is the only copy of the app: `setup`).
    let local = match &open {
        Some(a) if a.server.is_none() => Some(Local::bind(paths.this_computer())?),
        _ => None,
    };
    let webview_dir = paths.webview(open.as_ref().map(|a| a.id.as_str()));
    let api_server = match &local {
        Some(local) => Some(local.origin().to_string()),
        None => open.as_ref().and_then(|a| a.server.clone()),
    };
    let updates = Arc::new(Updates::default());
    // How a Markdown folder's .notekpr-folder names this app.
    let mut which = vec![];
    if let Some((tag, _)) = &own {
        which.push(format!("development {tag}"));
    }
    if !default_profile {
        which.push(format!("profile '{}'", args.profile));
    }
    let folder = Folder::new(
        paths.folder(open.as_ref().map_or("none", |a| a.id.as_str())),
        api_server.clone(),
        if which.is_empty() {
            "the desktop app".to_string()
        } else {
            format!("the desktop app ({})", which.join(", "))
        },
        updates.clone(),
    );

    context.config_mut().app.security.csp = Some(Csp::Policy(csp(api_server.as_deref())));

    let state = Arc::new(AppState {
        profile: args.profile.clone(),
        paths: paths.clone(),
        settings: Mutex::new(settings),
        open,
        switching: Default::default(),
        updates,
        folder,
        local,
        sign_in: Mutex::new(None),
        picked: Mutex::new(Default::default()),
        cli_take: Mutex::new(None),
        autostart_line: std::iter::once("notekpr-desktop".to_string())
            .chain(autostart_args.iter().cloned())
            .collect(),
        stopped: Default::default(),
    });

    let hidden = args.hidden;
    let builder = tauri::Builder::default();
    #[cfg(target_os = "macos")]
    let builder = builder.menu(menu_bar).on_menu_event(|app, e| {
        if e.id() == "menu-quit" {
            quit(app);
        }
    });
    let app = builder
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
            crate::commands::add_server,
            crate::commands::switch_account,
            crate::commands::remove_account,
            crate::commands::account_seen,
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
            crate::commands::folder_bring,
            crate::commands::cli_folders,
            crate::commands::folder_take_from_cli,
            crate::commands::cancel_take,
            crate::commands::setup_done,
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
            let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::App(PathBuf::new()))
                .title("notekpr")
                .inner_size(1200.0, 820.0)
                .min_inner_size(360.0, 480.0)
                .visible(!hidden)
                .data_directory(webview_dir.clone());
            // macOS keeps the window's storage where WebKit does, not in
            // the folder: the account's own store there.
            #[cfg(target_os = "macos")]
            let window = window.data_store_identifier(crate::paths::webview_store(&webview_dir));
            window
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
            if settled {
                let s = state.settings.lock().unwrap();
                if let Err(e) = s.save(&state.paths.settings_file()) {
                    tracing::warn!("couldn't keep the accounts list: {e:#}");
                }
            }
            // The Markdown folder from before accounts, if it is the open
            // account's.
            if let Some(a) = &state.open {
                match crate::folder::take_legacy(
                    &state.paths.legacy_folder(),
                    &state.paths.folder(&a.id),
                    a.server.as_deref(),
                ) {
                    Ok(true) => tracing::info!("the Markdown folder is the open account's now"),
                    Ok(false) => {}
                    Err(e) => tracing::warn!("the Markdown folder from before accounts: {e:#}"),
                }
            }
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
    app.run(|app, e| match e {
        RunEvent::ExitRequested {
            code: None, api, ..
        } => {
            // The window closed for good: stop the folder's sync properly
            // first.
            api.prevent_exit();
            quit(app);
        }
        // The app ends. Usually what runs stopped already; but the system
        // may end it without asking (macOS: Quit in the Dock, logging
        // out), even while it is stopping: it stops, or finishes stopping,
        // before the process ends.
        RunEvent::Exit => {
            if let Some(state) = app.try_state::<Arc<AppState>>() {
                tauri::async_runtime::block_on(state.stop());
            }
        }
        // The Dock's icon clicked: the window, closed to the menu bar, back.
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => show_window(app),
        _ => {}
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
            "connect-src 'self' ipc: https://ipc.localhost https://notes.example.com wss://notes.example.com;"
        ));
        assert!(c.contains("script-src 'self' 'wasm-unsafe-eval';"));
        let c = csp(Some("http://localhost:8080"));
        assert!(c.contains("http://localhost:8080 ws://localhost:8080;"));
        // The app's own server: that address and port only.
        let c = csp(Some("http://127.0.0.1:41234"));
        assert!(c.contains(
            "connect-src 'self' ipc: https://ipc.localhost http://127.0.0.1:41234 ws://127.0.0.1:41234;"
        ));
        assert!(csp(None).contains("connect-src 'self' ipc: https://ipc.localhost;"));
    }

    #[test]
    fn autostart_arguments_survive_the_entry() {
        let arg = desktop_entry_arg;
        assert_eq!(arg("--hidden"), "--hidden");
        assert_eq!(
            arg("/home/ana/dev/notekpr/data/desktop/config"),
            "/home/ana/dev/notekpr/data/desktop/config"
        );
        assert_eq!(
            arg("/home/ana/My notes/config"),
            "\"/home/ana/My notes/config\""
        );
        assert_eq!(arg("a\"b$c"), "\"a\\\\\"b\\\\$c\"");
        assert_eq!(arg("a\\b"), "\"a\\\\\\\\b\"");
        assert_eq!(arg(""), "\"\"");
    }

    #[test]
    fn autostart_arguments_survive_the_run_key() {
        let arg = command_line_arg;
        assert_eq!(arg("--hidden"), "--hidden");
        assert_eq!(
            arg(r"C:\Users\ana\dev\notekpr\config"),
            r"C:\Users\ana\dev\notekpr\config"
        );
        assert_eq!(
            arg(r"C:\Users\ana\My notes\config"),
            r#""C:\Users\ana\My notes\config""#
        );
        // Backslashes before a quote, a closing one included, doubled.
        assert_eq!(arg(r"C:\My notes\"), r#""C:\My notes\\""#);
        assert_eq!(arg(r#"a"b"#), r#""a\"b""#);
        assert_eq!(arg(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(arg(""), r#""""#);
    }

    #[test]
    fn autostart_arguments_survive_the_launch_agent() {
        let arg = plist_arg;
        assert_eq!(arg("--hidden"), "--hidden");
        // Spaces and quotes as they are: nothing splits the arguments again.
        assert_eq!(
            arg("/Users/ana/My notes/\"config\""),
            "/Users/ana/My notes/\"config\""
        );
        assert_eq!(arg("/Users/ana/a&b<c>"), "/Users/ana/a&amp;b&lt;c&gt;");
        assert_eq!(arg(""), "");
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
