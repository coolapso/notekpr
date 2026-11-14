//! What the window may ask of the native side (Tauri IPC). Only the app's
//! own window may call these (capabilities/main.json) — but they are made
//! safe for a window running someone else's script (an XSS through a
//! note): nothing here reads or writes a file the person didn't choose in
//! the system's own dialog, starts a program other than the default
//! browser for a web or mail address, or changes the server or the
//! Markdown folder without the person's yes in a system dialog.

use std::sync::Arc;
use std::time::Duration;

use notekpr_cli::account::Adopt;
use notekpr_cli::api::{Api, Credentials};
use notekpr_core::keys::AccountPublic;
use notekpr_core::protocol::SessionResponse;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_dialog::{DialogExt as _, MessageDialogButtons, MessageDialogKind};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

use crate::app::{AppState, show_window};
use crate::folder::Check;
use crate::links;
use crate::status::Status;

type Res<T> = Result<T, String>;

fn text(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn chain(e: anyhow::Error) -> String {
    format!("{e:#}")
}

/// Browser tests answer the system's dialogs, which a WebDriver can't
/// reach, with `NOTEKPR_DESKTOP_TEST_FOLDER=<path>`: every question gets a
/// yes and the folder picker that folder. Debug builds only — release
/// builds have no such thing.
#[cfg(debug_assertions)]
fn test_answers() -> Option<std::path::PathBuf> {
    std::env::var_os("NOTEKPR_DESKTOP_TEST_FOLDER").map(Into::into)
}

#[cfg(not(debug_assertions))]
fn test_answers() -> Option<std::path::PathBuf> {
    None
}

/// Ask in a system dialog (not the window's: a script in the window can't
/// answer it).
async fn confirm(
    app: &AppHandle,
    kind: MessageDialogKind,
    title: &str,
    text: &str,
    ok: &str,
) -> bool {
    if test_answers().is_some() {
        return true;
    }
    let (tx, rx) = oneshot::channel();
    let mut d = app.dialog().message(text).title(title).kind(kind).buttons(
        MessageDialogButtons::OkCancelCustom(ok.into(), "Cancel".into()),
    );
    if let Some(w) = app.get_webview_window("main") {
        d = d.parent(&w);
    }
    d.show(move |yes| {
        let _ = tx.send(yes);
    });
    rx.await.unwrap_or(false)
}

#[derive(Serialize)]
pub struct Info {
    pub version: &'static str,
    pub profile: String,
    pub server: Option<String>,
    pub close_to_tray: bool,
    /// A tray icon could be shown: closing to it is possible.
    pub tray: bool,
    pub autostart: bool,
    pub status_version: u64,
    pub status: Status,
}

#[tauri::command]
pub fn desktop_info(app: AppHandle, state: State<'_, Arc<AppState>>) -> Res<Info> {
    let s = state.settings.lock().unwrap().clone();
    let (status_version, status) = state.updates.current();
    Ok(Info {
        version: notekpr_core::VERSION,
        profile: state.profile.clone(),
        server: s.server,
        close_to_tray: s.close_to_tray,
        tray: crate::app::tray_available(),
        autostart: if crate::flatpak::inside() {
            s.autostart
        } else {
            app.autolaunch().is_enabled().unwrap_or(false)
        },
        status_version,
        status,
    })
}

/// Choose the server: checked here (it must answer as a notekpr server
/// that apps can sign in to), remembered, then the app starts again so the
/// window's Content-Security-Policy allows it.
#[tauri::command]
pub async fn set_server(app: AppHandle, state: State<'_, Arc<AppState>>, url: String) -> Res<()> {
    let server = notekpr_cli::profile::normalize_server_url(&url, false).map_err(chain)?;
    let info = Api::new(&server, Credentials::None)
        .map_err(text)?
        .info()
        .await
        .map_err(|e| format!("{server} didn't answer as a notekpr server: {e}"))?;
    if info.name != "notekpr" {
        return Err(format!("{server} isn't a notekpr server"));
    }
    if !info.app_sign_in && !(info.dev_mode && !info.oidc_enabled) {
        return Err(format!(
            "{server} runs notekpr {}, too old for the desktop app to sign in: it needs updating first",
            info.version
        ));
    }
    let before = state.settings.lock().unwrap().server.clone();
    if before.as_deref() == Some(server.as_str()) {
        return Ok(());
    }
    if let Some(before) = &before {
        let folder = match state.folder.configured() {
            Some(dir) => format!(
                "\n\nThe app stops keeping {} in sync (the files stay).",
                notekpr_cli::setup::shown(&dir)
            ),
            None => String::new(),
        };
        let text = format!(
            "notekpr will use {server} instead of {before}: you sign in there. What the app keeps for \
             {before} stays on this computer for when you come back to it.{folder}"
        );
        if !confirm(
            &app,
            MessageDialogKind::Info,
            "Change the server?",
            &text,
            "Change the server",
        )
        .await
        {
            return Err("cancelled".into());
        }
        // The folder belonged to the other server.
        state.folder.forget().await.map_err(chain)?;
    }
    {
        let mut s = state.settings.lock().unwrap();
        s.server = Some(server);
        s.save(&state.paths.settings_file()).map_err(chain)?;
    }
    // Again with the window's storage and Content-Security-Policy for it.
    app.restart();
}

/// Sign in through the system browser (`crate::signin`). A new attempt
/// ends the one before.
#[tauri::command]
pub async fn sign_in(app: AppHandle, state: State<'_, Arc<AppState>>) -> Res<SessionResponse> {
    let server = state
        .settings
        .lock()
        .unwrap()
        .server
        .clone()
        .ok_or("choose a server first")?;
    let (cancel_tx, cancel) = oneshot::channel();
    if let Some(previous) = state.sign_in.lock().unwrap().replace(cancel_tx) {
        let _ = previous.send(());
    }
    let updates = state.updates.clone();
    let result = crate::signin::sign_in(
        &server,
        |url| tauri_plugin_opener::open_url(url, None::<&str>).map_err(anyhow::Error::from),
        |code| updates.update(|s| s.sign_in_code = Some(code.to_string())),
        cancel,
    )
    .await;
    state.updates.update(|s| s.sign_in_code = None);
    show_window(&app);
    result.map_err(chain)
}

#[tauri::command]
pub fn cancel_sign_in(state: State<'_, Arc<AppState>>) {
    if let Some(tx) = state.sign_in.lock().unwrap().take() {
        let _ = tx.send(());
    }
}

#[tauri::command]
pub fn set_close_to_tray(state: State<'_, Arc<AppState>>, on: bool) -> Res<()> {
    let mut s = state.settings.lock().unwrap();
    s.close_to_tray = on;
    s.save(&state.paths.settings_file()).map_err(chain)
}

/// Start at login, or no longer: an autostart entry of the system's — in
/// the Flatpak through the Background portal, which may ask the person.
#[tauri::command]
pub async fn set_autostart(app: AppHandle, state: State<'_, Arc<AppState>>, on: bool) -> Res<()> {
    if !crate::flatpak::inside() {
        let a = app.autolaunch();
        return if on { a.enable() } else { a.disable() }.map_err(text);
    }
    let line = state.autostart_line.clone();
    let granted =
        tauri::async_runtime::spawn_blocking(move || crate::flatpak::request_autostart(on, line))
            .await
            .map_err(text)?
            .map_err(chain)?;
    let mut s = state.settings.lock().unwrap();
    s.autostart = on && granted;
    s.save(&state.paths.settings_file()).map_err(chain)?;
    if on && !granted {
        return Err("The system didn't allow notekpr to start when you log in.".into());
    }
    Ok(())
}

/// Open a web or mail address in the default browser or mail program;
/// nothing else.
#[tauri::command]
pub fn open_external(url: String) -> Res<()> {
    let url = links::external(&url).ok_or("only web and mail addresses open from the app")?;
    tauri_plugin_opener::open_url(url.as_str(), None::<&str>).map_err(text)
}

/// A folder picked in the system's dialog, as the window may know it: a
/// number to start with, the folder as people read it, and whether it can
/// be used. The window never names a folder itself.
#[derive(Serialize)]
pub struct Picked {
    pub pick: u64,
    pub shown: String,
    #[serde(flatten)]
    pub check: Check,
}

/// The system's folder picker.
#[tauri::command]
pub async fn choose_folder(app: AppHandle, state: State<'_, Arc<AppState>>) -> Res<Option<Picked>> {
    let dir = match test_answers() {
        Some(dir) => Some(dir),
        None => {
            let (tx, rx) = oneshot::channel();
            let mut dialog = app.dialog().file().set_title("The Markdown folder");
            if let Some(w) = app.get_webview_window("main") {
                dialog = dialog.set_parent(&w);
            }
            dialog.pick_folder(move |p| {
                let _ = tx.send(p);
            });
            rx.await.map_err(text)?.and_then(|p| p.into_path().ok())
        }
    };
    let Some(dir) = dir else {
        return Ok(None);
    };
    let check = state.folder.check(&dir);
    let shown = notekpr_cli::setup::shown(&dir);
    let mut picked = state.picked.lock().unwrap();
    let pick = picked.as_ref().map_or(1, |(n, _)| n + 1);
    *picked = Some((pick, dir));
    Ok(Some(Picked { pick, shown, check }))
}

/// What the window hands over for the Markdown folder: this computer's
/// device as the window holds it (`DeviceRecord`) and its session.
#[derive(Deserialize)]
pub struct Handoff {
    pub user_id: String,
    pub display_name: String,
    pub device_id: String,
    /// The device's secret, base64.
    pub device_secret: String,
    /// The account key the device was approved for (signing ‖ encryption
    /// public key, base64).
    pub account: String,
    pub token: String,
    pub expires_at: i64,
}

fn adopt_from(server: String, h: Handoff) -> Res<Adopt> {
    let secret = Zeroizing::new(
        notekpr_core::b64::decode(&h.device_secret).map_err(|_| "not a device key")?,
    );
    let device_secret: [u8; 32] = secret
        .as_slice()
        .try_into()
        .map_err(|_| "not a device key")?;
    let account = notekpr_core::b64::decode(&h.account).map_err(|_| "not an account key")?;
    if account.len() != 64 {
        return Err("not an account key".into());
    }
    Ok(Adopt {
        server,
        user_id: h.user_id,
        display_name: h.display_name,
        device_id: h.device_id,
        device_name: Some(notekpr_cli::account::host_name()),
        device_secret: Zeroizing::new(device_secret),
        account_public: AccountPublic {
            sign_public: account[..32].try_into().expect("64 bytes"),
            enc_public: account[32..].try_into().expect("64 bytes"),
        },
        token: h.token,
        expires_at: h.expires_at,
    })
}

/// Start keeping the folder picked as `pick` in sync as the Markdown
/// folder, once the person said yes to what that means in a system dialog.
#[tauri::command]
pub async fn folder_start(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    handoff: Handoff,
    pick: u64,
) -> Res<()> {
    let server = state
        .settings
        .lock()
        .unwrap()
        .server
        .clone()
        .ok_or("choose a server first")?;
    let dir = match state.picked.lock().unwrap().take() {
        Some((n, p)) if n == pick => p,
        _ => return Err("choose the folder again".into()),
    };
    let check = state.folder.check(&dir);
    if let Some(why) = check.refused {
        return Err(why);
    }
    let shown = notekpr_cli::setup::shown(&dir);
    let mut text = format!(
        "notekpr will write your notes into {shown} as ordinary Markdown files, decrypted, and keep them in \
         sync both ways while the app runs.\n\nThose files are protected only by your account on this \
         computer and its disk encryption: any program you run can read them, and backup or cloud tools may \
         copy them. Everything on the server stays end-to-end encrypted."
    );
    if let Some(cloud) = &check.cloud {
        text.push_str(&format!(
            "\n\n{shown} is synced by {cloud}: it would upload the decrypted notes, and two tools syncing \
             the same files fight over them."
        ));
    }
    let kind = if check.cloud.is_some() {
        MessageDialogKind::Warning
    } else {
        MessageDialogKind::Info
    };
    if !confirm(
        &app,
        kind,
        "Keep a Markdown folder?",
        &text,
        "Keep it in sync",
    )
    .await
    {
        return Err("cancelled".into());
    }
    // Still fine after the person was asked (still empty, say).
    if let Some(why) = state.folder.check(&dir).refused {
        return Err(why);
    }
    let adopt = adopt_from(server, handoff)?;
    state
        .folder
        .adopt_and_start(adopt, dir)
        .await
        .map_err(chain)
}

/// Stop keeping the folder in sync, once the person said yes: the app
/// forgets its copies and keys for it; the Markdown files stay.
#[tauri::command]
pub async fn folder_off(app: AppHandle, state: State<'_, Arc<AppState>>) -> Res<()> {
    let Some(dir) = state.folder.kept() else {
        return Ok(());
    };
    let text = format!(
        "notekpr stops keeping {} in sync. The files stay where they are; changes made to them from now on \
         don't reach your notes.",
        notekpr_cli::setup::shown(&dir)
    );
    if !confirm(
        &app,
        MessageDialogKind::Info,
        "Stop the Markdown folder?",
        &text,
        "Stop",
    )
    .await
    {
        return Err("cancelled".into());
    }
    state.folder.forget().await.map_err(chain)
}

#[tauri::command]
pub async fn folder_retry(state: State<'_, Arc<AppState>>) -> Res<()> {
    state.folder.start().await.map_err(chain)
}

#[tauri::command]
pub fn reveal_folder(state: State<'_, Arc<AppState>>) -> Res<()> {
    let dir = state.folder.configured().ok_or("no Markdown folder")?;
    tauri_plugin_opener::open_path(dir, None::<&str>).map_err(text)
}

/// The window signed in again: the folder goes on with the new session.
#[tauri::command(rename_all = "snake_case")]
pub async fn session_changed(
    state: State<'_, Arc<AppState>>,
    token: String,
    expires_at: i64,
) -> Res<()> {
    state
        .folder
        .session_changed(token, expires_at)
        .await
        .map_err(chain)
}

#[tauri::command]
pub async fn signed_out(state: State<'_, Arc<AppState>>) -> Res<()> {
    state.folder.signed_out().await.map_err(chain)
}

/// "Forget this computer": the folder's keys and copies go too — after a
/// yes in a system dialog, since a script in the window could be the one
/// asking (whatever the session's state: a script can end that too).
#[tauri::command]
pub async fn forget_computer(app: AppHandle, state: State<'_, Arc<AppState>>) -> Res<()> {
    let Some(dir) = state.folder.kept() else {
        return Ok(());
    };
    let text = format!(
        "notekpr also forgets its keys and copies for the Markdown folder on this computer, and stops keeping \
         {} in sync. The files stay where they are.",
        notekpr_cli::setup::shown(&dir)
    );
    if !confirm(
        &app,
        MessageDialogKind::Warning,
        "Forget the Markdown folder too?",
        &text,
        "Forget",
    )
    .await
    {
        return Err("cancelled".into());
    }
    state.folder.forget().await.map_err(chain)
}

/// The status once it differs from version `since` (or after a while).
#[tauri::command]
pub async fn next_event(state: State<'_, Arc<AppState>>, since: u64) -> Res<(u64, Status)> {
    Ok(state.updates.next(since, Duration::from_secs(25)).await)
}
