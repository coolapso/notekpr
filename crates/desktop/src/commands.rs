//! What the window may ask of the native side (Tauri IPC). Only the app's
//! own window may call these (capabilities/main.json) — but they are made
//! safe for a window running someone else's script (an XSS through a
//! note): nothing here reads or writes a file the person didn't choose in
//! the system's own dialog, starts a program other than the default
//! browser for a web or mail address, adds a server, starts a Markdown
//! folder or brings notes into one without the person's yes in a system
//! dialog. Switching between accounts already added doesn't ask: the next
//! one's window has storage of its own. The one thing done outside folders
//! picked is taking over a Markdown folder the CLI keeps for the same
//! account ([`folder_take_from_cli`], docs/desktop.md "From the CLI"),
//! after a yes too.

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
use crate::from_cli::{self, Service, Systemd};
use crate::links;
use crate::status::{CliWait, Status};

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
    /// Where the API is: the open account's server, or with notes only on
    /// this computer the app's own (`http://127.0.0.1:<port>`).
    pub server: Option<String>,
    /// The open account is on this computer.
    pub this_computer: bool,
    /// The accounts in the list, in the order added.
    pub accounts: Vec<AccountInfo>,
    pub close_to_tray: bool,
    /// A tray icon could be shown: closing to it is possible.
    pub tray: bool,
    pub autostart: bool,
    /// The initial setup asked about the tray and starting at login.
    pub background_asked: bool,
    pub status_version: u64,
    pub status: Status,
}

#[derive(Serialize)]
pub struct AccountInfo {
    pub id: String,
    /// Its server's address; none: on this computer.
    pub server: Option<String>,
    /// What its window last reported (a label).
    pub name: Option<String>,
    pub open: bool,
    /// Its initial setup was shown.
    pub set_up: bool,
}

#[tauri::command]
pub fn desktop_info(app: AppHandle, state: State<'_, Arc<AppState>>) -> Res<Info> {
    let s = state.settings.lock().unwrap().clone();
    let (status_version, status) = state.updates.current();
    let open = state.open.as_ref().map(|a| a.id.as_str());
    Ok(Info {
        version: notekpr_core::VERSION,
        profile: state.profile.clone(),
        server: state.api_server(),
        this_computer: state.local.is_some(),
        accounts: s
            .visible()
            .map(|a| AccountInfo {
                id: a.id.clone(),
                server: a.server.clone(),
                name: a.name.clone(),
                open: Some(a.id.as_str()) == open,
                set_up: a.set_up,
            })
            .collect(),
        close_to_tray: s.close_to_tray,
        background_asked: s.background_asked,
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

/// Add an account on a server, then open it: asked in a system dialog
/// before the app contacts the address at all (but the first time, when
/// there is no account yet), then checked (it must answer as a notekpr
/// server apps can sign in to). A server the app has an account on already
/// opens that account (back in the list if it was removed).
#[tauri::command]
pub async fn add_server(app: AppHandle, state: State<'_, Arc<AppState>>, url: String) -> Res<()> {
    let server = notekpr_cli::profile::normalize_server_url(&url, false).map_err(chain)?;
    let known = state.settings.lock().unwrap().find(Some(&server)).cloned();
    if let Some(a) = known {
        return open_account(&app, &state, &a.id).await;
    }
    if state.open.is_some() {
        let text = format!(
            "notekpr will add an account on {server} and open it: you sign in there. Your other accounts stay \
             as they are, on this computer."
        );
        if !confirm(
            &app,
            MessageDialogKind::Info,
            "Add an account?",
            &text,
            "Add the account",
        )
        .await
        {
            return Err("cancelled".into());
        }
    }
    // There only: a redirect would reach an address the person wasn't asked
    // about.
    let info = Api::without_redirects(&server, Credentials::None)
        .map_err(text)?
        .info()
        .await
        .map_err(|e| {
            if e.is_redirect() {
                format!(
                    "{server} sends notekpr to another address: add the server at its own address"
                )
            } else {
                format!("{server} didn't answer as a notekpr server: {e}")
            }
        })?;
    if info.name != "notekpr" {
        return Err(format!("{server} isn't a notekpr server"));
    }
    if !info.app_sign_in && !(info.dev_mode && !info.oidc_enabled) {
        return Err(format!(
            "{server} runs notekpr {}, too old for the desktop app to sign in: it needs updating first",
            info.version
        ));
    }
    let id = {
        let mut s = state.settings.lock().unwrap();
        // Used with 0.17.0: its window storage, under the name it had then
        // — if that name can't be another address's too.
        let legacy = crate::paths::legacy_account_id(&server);
        let old = crate::paths::legacy_name_is_its_own(&server)
            && s.get(&legacy).is_none()
            && state.paths.webview(Some(&legacy)).is_dir();
        s.add(Some(server.clone()), || {
            if old {
                legacy
            } else {
                crate::paths::new_account_id(&server)
            }
        })
    };
    open_account(&app, &state, &id).await
}

/// Keep notes only on this computer, with the server inside the app
/// ([`crate::local`]): the account *On this computer*, added if it isn't
/// there yet, opened. No server is involved, so nothing to confirm.
#[tauri::command]
pub async fn use_this_computer(app: AppHandle, state: State<'_, Arc<AppState>>) -> Res<()> {
    let id = state.settings.lock().unwrap().add(None, || unreachable!());
    open_account(&app, &state, &id).await
}

/// Open another account in the list: the app starts again on it. Not asked
/// (a script that switches gains nothing: the next account's window has
/// storage of its own).
#[tauri::command]
pub async fn switch_account(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Res<()> {
    let listed = state
        .settings
        .lock()
        .unwrap()
        .get(&id)
        .is_some_and(|a| !a.hidden);
    if !listed {
        return Err("no such account".into());
    }
    open_account(&app, &state, &id).await
}

/// Take an account off the list, after a yes in a system dialog: what the
/// app keeps for it stays. Never the open one, nor *On this computer*.
#[tauri::command]
pub async fn remove_account(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Res<()> {
    let Some(a) = state.settings.lock().unwrap().get(&id).cloned() else {
        return Err("no such account".into());
    };
    let Some(server) = a
        .server
        .clone()
        .filter(|_| state.open.as_ref().map(|o| &o.id) != Some(&id))
    else {
        return Err("this account can't be removed".into());
    };
    let who = match &a.name {
        Some(n) => format!("{n} on {server}"),
        None => server.clone(),
    };
    let text = format!(
        "{who} leaves the list. What the app keeps for it stays on this computer: adding the account again \
         finds it all."
    );
    if !confirm(
        &app,
        MessageDialogKind::Info,
        "Remove the account?",
        &text,
        "Remove",
    )
    .await
    {
        return Err("cancelled".into());
    }
    if state.switching.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("notekpr is switching accounts".into());
    }
    let mut s = state.settings.lock().unwrap();
    s.hide(&id);
    s.save(&state.paths.settings_file()).map_err(chain)
}

/// The name the open account's window shows (its person's), kept beside
/// the account in the list: a label, never anything the app acts on.
#[tauri::command]
pub fn account_seen(state: State<'_, Arc<AppState>>, name: String) -> Res<()> {
    let Some(open) = &state.open else {
        return Ok(());
    };
    if state.switching.load(std::sync::atomic::Ordering::SeqCst) {
        return Ok(());
    }
    let name: String = name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect();
    let mut s = state.settings.lock().unwrap();
    let Some(a) = s.accounts.iter_mut().find(|a| a.id == open.id) else {
        return Ok(());
    };
    let name = Some(name).filter(|n| !n.is_empty());
    if a.name == name {
        return Ok(());
    }
    a.name = name;
    s.save(&state.paths.settings_file()).map_err(chain)
}

/// Open account `id` (in the list): its Markdown folder and the app's own
/// server stop as when quitting, the settings say so, and the app starts
/// again with that account's window storage and Content-Security-Policy.
/// One switch only: nothing about accounts changes once it began.
async fn open_account(app: &AppHandle, state: &AppState, id: &str) -> Res<()> {
    if state.open.as_ref().is_some_and(|a| a.id == id) {
        return Ok(());
    }
    if state
        .switching
        .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        return Err("notekpr is switching accounts".into());
    }
    state.stop().await;
    let saved = {
        let mut s = state.settings.lock().unwrap();
        let before = s.clone();
        s.open(id);
        let saved = s.save(&state.paths.settings_file());
        if saved.is_err() {
            *s = before;
        }
        saved
    };
    if let Err(e) = saved {
        // Still on this account: what it runs goes on.
        state
            .switching
            .store(false, std::sync::atomic::Ordering::SeqCst);
        *state.stopped.lock().await = false;
        if let Some(local) = &state.local {
            let _ = local.start().await;
        }
        let _ = state.folder.start().await;
        return Err(chain(e));
    }
    app.restart();
}

/// Notes only on this computer: the app's own server runs (started again
/// if it failed to), or why not. The window asks before anything else.
#[tauri::command]
pub async fn local_start(state: State<'_, Arc<AppState>>) -> Res<()> {
    let local = state.local.as_ref().ok_or("your notes are on a server")?;
    local.start().await
}

/// Notes only on this computer: a session for the window, from the app's
/// own server — there is nothing to sign in to. A script in the window
/// asking gets what the window holds already.
#[tauri::command]
pub async fn local_session(state: State<'_, Arc<AppState>>) -> Res<SessionResponse> {
    let local = state
        .local
        .as_ref()
        .ok_or("your notes are on a server: sign in there")?;
    local.session().await
}

/// Sign in through the system browser (`crate::signin`). A new attempt
/// ends the one before.
#[tauri::command]
pub async fn sign_in(app: AppHandle, state: State<'_, Arc<AppState>>) -> Res<SessionResponse> {
    let server = state
        .open
        .as_ref()
        .and_then(|a| a.server.clone())
        .ok_or("there is no server to sign in to")?;
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
    // Its Launch Agent would start a copy that is gone by then.
    #[cfg(target_os = "macos")]
    if on && crate::app::runs_from_temporary_copy() {
        return Err(
            "Move notekpr to your Applications folder first (drag it there), open it from there, \
             then turn this on."
                .into(),
        );
    }
    if !crate::flatpak::inside() {
        let a = app.autolaunch();
        return if on { a.enable() } else { a.disable() }.map_err(text);
    }
    // When the system changed nothing, the setting stays as it was.
    let now = crate::flatpak::request_autostart(on, state.autostart_line.clone())
        .await
        .map_err(chain)?;
    let mut s = state.settings.lock().unwrap();
    s.autostart = now;
    s.save(&state.paths.settings_file()).map_err(chain)?;
    match (on, now) {
        (true, false) => Err("The system didn't allow notekpr to start when you log in.".into()),
        (false, true) => Err("The system didn't stop notekpr starting when you log in.".into()),
        _ => Ok(()),
    }
}

/// Open a web or mail address in the default browser or mail program;
/// nothing else.
#[tauri::command]
pub fn open_external(url: String) -> Res<()> {
    let url = links::external(&url).ok_or("only web and mail addresses open from the app")?;
    tauri_plugin_opener::open_url(url.as_str(), None::<&str>).map_err(text)
}

/// The folders last picked in the system's dialog, by number: each used
/// once (`folder_start`, `folder_bring`).
#[derive(Default)]
pub struct Picks {
    last: u64,
    kept: Vec<(u64, std::path::PathBuf)>,
}

impl Picks {
    /// A few are kept: a copy uses two (where the notes are, and the new
    /// Markdown folder).
    const KEPT: usize = 4;

    fn put(&mut self, dir: std::path::PathBuf) -> u64 {
        self.last += 1;
        self.kept.push((self.last, dir));
        if self.kept.len() > Self::KEPT {
            self.kept.remove(0);
        }
        self.last
    }

    fn take(&mut self, n: u64) -> Res<std::path::PathBuf> {
        let i = self
            .kept
            .iter()
            .position(|(k, _)| *k == n)
            .ok_or("choose the folder again")?;
        Ok(self.kept.remove(i).1)
    }
}

/// A folder picked in the system's dialog, as the window may know it: a
/// number to start with, the folder as people read it, whether it can be
/// used, and the notes it holds. The window never names a folder itself.
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
    let pick = state.picked.lock().unwrap().put(dir);
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
    let server = state.api_server().ok_or("choose a server first")?;
    let _changing = state.folder.changing().map_err(chain)?;
    let dir = state.picked.lock().unwrap().take(pick)?;
    let check = state.folder.check(&dir);
    if let Some(why) = check.refused {
        return Err(why);
    }
    let shown = notekpr_cli::setup::shown(&dir);
    if check.notes.is_some() {
        return Err(format!(
            "{shown} holds notes: bring them in, or choose another folder"
        ));
    }
    let mut text = format!(
        "notekpr will write your notes into {shown} as ordinary Markdown files, decrypted, and keep them in \
         sync both ways while the app runs."
    );
    text.push_str(&plaintext_note(&dir, check.cloud.as_deref()));
    if !confirm(
        &app,
        dialog_kind(&check),
        "Keep a Markdown folder?",
        &text,
        "Keep it in sync",
    )
    .await
    {
        return Err("cancelled".into());
    }
    // Still fine after the person was asked (no notes put there meanwhile).
    let check = state.folder.check(&dir);
    if let Some(why) = check.refused {
        return Err(why);
    }
    if check.notes.is_some() {
        return Err(format!("{shown} changed meanwhile: choose it again"));
    }
    let adopt = adopt_from(server, handoff)?;
    state
        .folder
        .adopt_and_start(adopt, dir)
        .await
        .map_err(chain)
}

/// What the person agrees to with a Markdown folder in `dir`, for a system
/// dialog.
fn plaintext_note(dir: &std::path::Path, cloud: Option<&str>) -> String {
    let shown = notekpr_cli::setup::shown(dir);
    let mut text = "\n\nThose files are protected only by your account on this computer and its disk \
                    encryption: any program you run can read them, and backup or cloud tools may copy them. \
                    Everything on the server stays end-to-end encrypted."
        .to_string();
    if let Some(cloud) = cloud {
        text.push_str(&format!(
            "\n\n{shown} is synced by {cloud}: it would upload the decrypted notes, and two tools syncing \
             the same files fight over them."
        ));
    }
    text
}

fn dialog_kind(check: &Check) -> MessageDialogKind {
    if check.cloud.is_some() {
        MessageDialogKind::Warning
    } else {
        MessageDialogKind::Info
    }
}

/// Bring the notes in a folder into a vault (docs/desktop.md "Notes
/// already in the folder"): those in the folder picked as `pick` moved
/// into the vault's folder there, or — with `copy_to`, another folder
/// picked — copied into one there, which becomes the Markdown folder. The
/// vault is the account's of that name, or a new one: what it is, and who
/// else sees it, is said in the system dialog as this app's own sync knows
/// it, never as the window says.
#[tauri::command(rename_all = "snake_case")]
pub async fn folder_bring(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    handoff: Handoff,
    pick: u64,
    copy_to: Option<u64>,
    vault: String,
) -> Res<()> {
    let server = state.api_server().ok_or("choose a server first")?;
    let _changing = state.folder.changing().map_err(chain)?;
    // Nothing that runs is stopped, nor anything kept changed, before the
    // person's yes.
    if state.folder.kept().is_some() {
        return Err("this account has a Markdown folder already: stop it first".into());
    }
    let (from, root) = {
        let mut picks = state.picked.lock().unwrap();
        let from = picks.take(pick)?;
        let root = match copy_to {
            Some(n) => picks.take(n)?,
            None => from.clone(),
        };
        (from, root)
    };
    let check = state.folder.check(&root);
    if let Some(why) = check.refused {
        return Err(why);
    }
    let fresh = state
        .folder
        .adopt(adopt_from(server, handoff)?)
        .await
        .map_err(chain)?;
    let r = async {
        let plan = state
            .folder
            .plan_bring(&root, &from, &vault)
            .await
            .map_err(chain)?;
        let shown = notekpr_cli::setup::shown;
        let into = match &plan.target {
            notekpr_cli::setup::Target::Existing { name, .. } => format!("your vault '{name}'"),
            notekpr_cli::setup::Target::New(name) => format!("a new vault '{name}'"),
        };
        let what = notekpr_cli::setup::describe(&plan.contents);
        let mut text = if plan.copy() {
            format!(
                "notekpr will copy everything in {} ({what}) into {}, {into}, and keep {} in sync as your \
                 Markdown folder: your notes as ordinary Markdown files, decrypted, synced both ways while \
                 the app runs. {} stays as it is.",
                shown(&plan.from),
                shown(&plan.dest),
                shown(&plan.root),
                shown(&plan.from),
            )
        } else {
            format!(
                "notekpr will move everything in {} ({what}), hidden files too, into {}, {into}, and keep \
                 {} in sync as your Markdown folder: your notes as ordinary Markdown files, decrypted, synced \
                 both ways while the app runs. Close Obsidian and other apps using {} first.",
                shown(&plan.root),
                shown(&plan.dest),
                shown(&plan.root),
                shown(&plan.root),
            )
        };
        if plan.others > 0 {
            text.push_str(&format!(
                "\n\n{} other {} this vault: they will see these notes too.",
                plan.others,
                if plan.others == 1 { "person shares" } else { "people share" }
            ));
        }
        text.push_str(&plaintext_note(&plan.root, check.cloud.as_deref()));
        let kind = if plan.others > 0 {
            MessageDialogKind::Warning
        } else {
            dialog_kind(&check)
        };
        let (title, ok) = if plan.copy() {
            ("Copy your notes in?", "Copy them")
        } else {
            ("Move your notes in?", "Move them")
        };
        if !confirm(&app, kind, title, &text, ok).await {
            return Err("cancelled".to_string());
        }
        state.folder.bring(plan).await.map_err(chain)
    }
    .await;
    if r.is_err() && fresh {
        let _ = state.folder.unadopt().await;
    }
    r
}

/// A Markdown folder the CLI keeps for the open account, as the window
/// may know it.
#[derive(Serialize)]
pub struct CliFolder {
    pub profile: String,
    /// The folder as people read it.
    pub shown: String,
    /// A CLI runs the profile now.
    pub running: bool,
    /// Its background sync; none where the app can't ask (the Flatpak).
    pub service: Option<Service>,
}

/// The Markdown folders the CLI keeps for the person whose session `token`
/// is (the server says whose: never another's, whatever the window says)
/// on the open account's server.
#[tauri::command]
pub async fn cli_folders(state: State<'_, Arc<AppState>>, token: String) -> Res<Vec<CliFolder>> {
    let Some(server) = state.open.as_ref().and_then(|a| a.server.clone()) else {
        return Ok(vec![]);
    };
    let me = Api::new(&server, Credentials::Session(token))
        .map_err(text)?
        .me()
        .await
        .map_err(|e| format!("checking your session: {e}"))?;
    let found = from_cli::find(&from_cli::configurations(), &server, &me.user.id);
    if found.is_empty() {
        return Ok(vec![]);
    }
    let systemd = Systemd::connect().await;
    let mut v = vec![];
    for f in found {
        let service = match &systemd {
            Some(s) => s.state(&f.unit()).await.ok(),
            None => None,
        };
        v.push(CliFolder {
            shown: notekpr_cli::setup::shown(&f.dir),
            running: f.running(),
            service,
            profile: f.profile,
        });
    }
    Ok(v)
}

/// Take over the Markdown folder the CLI's `profile` keeps for this
/// account (docs/desktop.md "From the CLI"), once the person said yes in a
/// system dialog: its background sync stopped and turned off at login (or,
/// where the app can't, the person shown how and waited for), the folder
/// given back on the CLI's side while no CLI runs the profile, then taken
/// and kept by the app.
#[tauri::command]
pub async fn folder_take_from_cli(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    handoff: Handoff,
    profile: String,
) -> Res<()> {
    let server = state
        .open
        .as_ref()
        .and_then(|a| a.server.clone())
        .ok_or("your notes are only on this computer: the CLI keeps none of them")?;
    let _changing = state.folder.changing().map_err(chain)?;
    if state.folder.kept().is_some() {
        return Err("this account has a Markdown folder already: stop it first".into());
    }
    // Whose session this is, as the server says: the CLI's profile must be
    // the same person's (and checked again as it lets the folder go).
    let me = Api::new(&server, Credentials::Session(handoff.token.clone()))
        .map_err(text)?
        .me()
        .await
        .map_err(|e| format!("checking your session: {e}"))?;
    let user_id = me.user.id.clone();
    let found = from_cli::find(&from_cli::configurations(), &server, &user_id)
        .into_iter()
        .find(|f| f.profile == profile)
        .ok_or("the CLI keeps no Markdown folder for you in that profile")?;
    let shown = notekpr_cli::setup::shown(&found.dir);
    let systemd = Systemd::connect().await;
    let was = match &systemd {
        Some(s) => s.state(&found.unit()).await.ok(),
        None => None,
    };
    let mut text = format!(
        "notekpr will take over {shown} from the CLI (its profile '{profile}'): it stops the CLI's background \
         sync, {}, and turns it off at login; the CLI forgets the folder and stays signed in. Then the app \
         keeps {shown} in sync both ways while it runs: your notes as ordinary Markdown files, decrypted.",
        found.unit().trim_end_matches(".service"),
    );
    text.push_str(&plaintext_note(
        &found.dir,
        notekpr_cli::setup::cloud_synced(&found.dir),
    ));
    if !confirm(
        &app,
        MessageDialogKind::Info,
        "Take over the CLI's Markdown folder?",
        &text,
        "Take it over",
    )
    .await
    {
        return Err("cancelled".into());
    }
    let fresh = state
        .folder
        .adopt(adopt_from(server.clone(), handoff)?)
        .await
        .map_err(chain)?;
    let r = take_from_cli(&state, &found, &server, &user_id, systemd.as_ref(), was).await;
    if r.is_err() && fresh {
        let _ = state.folder.unadopt().await;
    }
    r
}

async fn take_from_cli(
    state: &AppState,
    found: &from_cli::Found,
    server: &str,
    user_id: &str,
    systemd: Option<&Systemd>,
    was: Option<Service>,
) -> Res<()> {
    let unit = found.unit();
    // Its background sync, stopped and off at login: put back as it was if
    // nothing comes of it before the CLI lets the folder go — also when
    // only half of stopping it worked.
    let stopped = match (systemd, was) {
        (Some(s), Some(w)) if w.active || w.enabled => {
            if let Err(e) = s.stop(&unit, w).await {
                return Err(from_cli::put_back(Some(s), Some(w), &unit, chain(e)).await);
            }
            Some(w)
        }
        _ => None,
    };
    let (cancel_tx, cancel) = oneshot::channel();
    if let Some(previous) = state.cli_take.lock().unwrap().replace(cancel_tx) {
        let _ = previous.send(());
    }
    // Shown while a CLI still runs the profile: what to do, where the app
    // couldn't stop it.
    let shown = notekpr_cli::setup::shown(&found.dir);
    state.updates.update(|s| {
        s.cli_wait = Some(CliWait {
            dir: shown.clone(),
            profile: found.profile.clone(),
            command: systemd.is_none().then(|| found.command()).flatten(),
        })
    });
    let lock = found.wait_until_free(cancel).await;
    state.updates.update(|s| s.cli_wait = None);
    state.cli_take.lock().unwrap().take();
    let lock = match lock {
        Ok(Some(l)) => l,
        Ok(None) => {
            return Err(from_cli::put_back(systemd, stopped, &unit, "cancelled".into()).await);
        }
        Err(e) => return Err(from_cli::put_back(systemd, stopped, &unit, chain(e)).await),
    };
    if let Err(e) = found.give_back(server, user_id, &lock) {
        return Err(from_cli::put_back(systemd, stopped, &unit, chain(e)).await);
    }
    let r = state.folder.take_and_start(found.dir.clone()).await;
    drop(lock);
    r.map_err(|e| {
        format!(
            "The CLI no longer keeps {shown}, but the app couldn't take it over: {e:#}. Choose it again here, or \
             give it back to the CLI (notekpr --profile {} sync --dir {shown})",
            found.profile
        )
    })
}

/// Stop waiting for the CLI ([`folder_take_from_cli`]): nothing is taken
/// over, and its background sync is as it was.
#[tauri::command]
pub fn cancel_take(state: State<'_, Arc<AppState>>) {
    if let Some(tx) = state.cli_take.lock().unwrap().take() {
        let _ = tx.send(());
    }
}

/// The open account's initial setup was shown (and with it the question
/// about the tray and starting at login): not shown again by itself. A
/// script saying so only hides a screen.
#[tauri::command]
pub fn setup_done(state: State<'_, Arc<AppState>>) -> Res<()> {
    let Some(open) = &state.open else {
        return Ok(());
    };
    if state.switching.load(std::sync::atomic::Ordering::SeqCst) {
        return Ok(());
    }
    let mut s = state.settings.lock().unwrap();
    s.set_up(&open.id);
    s.save(&state.paths.settings_file()).map_err(chain)
}

/// The bar saying a newer release is out was dismissed for `version`
/// (docs/desktop.md "Update it"). A script saying so only hides the bar.
#[tauri::command]
pub fn dismiss_update(state: State<'_, Arc<AppState>>, version: String) -> Res<()> {
    crate::update::dismiss(&state, &version).map_err(chain)
}

/// How to update, on the docs' page, in the default browser: that address
/// only.
#[tauri::command]
pub fn update_help() -> Res<()> {
    tauri_plugin_opener::open_url(crate::update::HELP, None::<&str>).map_err(text)
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
