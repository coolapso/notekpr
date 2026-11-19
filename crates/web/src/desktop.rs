//! The desktop app (`crates/desktop`, docs/desktop.md): this same app, its
//! files inside the desktop app, in a window of its own. There the page's
//! origin is the app's (`tauri://localhost`), so the server is the one the
//! person chose — asked of the app at start — and a few things go through
//! the app instead of the browser: signing in (the system browser, back to
//! the app), the app's own settings (the tray, starting at login) and the
//! Markdown folder, which the app keeps in sync with this window's device
//! and session.
//!
//! Or the notes are only on this computer: the app runs the server inside
//! itself, and gives this window its sessions — there is nothing to sign
//! in to ([`local_session`]). What needs a server says so.
//!
//! The app's commands are called through Tauri's IPC
//! (`window.__TAURI_INTERNALS__.invoke`, which only the app's window has).
//! Everything else works as in a browser.

use std::cell::{Cell, RefCell};

use notekpr_core::protocol::SessionResponse;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::util;

thread_local! {
    static IS: Cell<Option<bool>> = const { Cell::new(None) };
    static INFO: RefCell<Option<Info>> = const { RefCell::new(None) };
}

/// Whether this is the desktop app's window.
pub fn is_desktop() -> bool {
    IS.with(|c| {
        *c.get().get_or_insert_with(|| {
            js_sys::Reflect::has(&util::window(), &"__TAURI_INTERNALS__".into()).unwrap_or(false)
        })
    })
}

/// What the app says about itself and its settings (`desktop_info`).
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Info {
    pub version: String,
    pub profile: String,
    /// Where the API is: the server chosen, or the app's own.
    pub server: Option<String>,
    /// The open account is on this computer (`server` is the app's own).
    #[serde(default)]
    pub this_computer: bool,
    /// The app's accounts (docs/desktop.md "Accounts").
    #[serde(default)]
    pub accounts: Vec<Account>,
    pub close_to_tray: bool,
    /// A tray icon could be shown: closing to it is possible.
    pub tray: bool,
    pub autostart: bool,
    /// The initial setup asked about the tray and starting at login.
    #[serde(default)]
    pub background_asked: bool,
    pub status_version: u64,
    pub status: Status,
}

/// One of the app's accounts: on a server, or on this computer.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Account {
    pub id: String,
    /// Its server; none: on this computer.
    pub server: Option<String>,
    /// What its window last showed: a label beside the server.
    pub name: Option<String>,
    pub open: bool,
    /// Its initial setup was shown.
    #[serde(default)]
    pub set_up: bool,
}

impl Account {
    /// How the list shows it: "ana on notes.example.com", "On this computer".
    pub fn label(&self) -> String {
        let Some(server) = &self.server else {
            return "On this computer".into();
        };
        // Normalized by the app: `scheme://host[:port][/path]`.
        let host = server
            .split_once("://")
            .map_or(server.as_str(), |(_, rest)| rest)
            .split('/')
            .next()
            .unwrap_or_default()
            .to_string();
        match &self.name {
            Some(n) => format!("{n} on {host}"),
            None => host,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_as_the_list_shows_them() {
        let a = |server: Option<&str>, name: Option<&str>| Account {
            id: "x".into(),
            server: server.map(Into::into),
            name: name.map(Into::into),
            open: false,
            set_up: false,
        };
        assert_eq!(
            a(Some("https://notes.example.com"), Some("ana")).label(),
            "ana on notes.example.com"
        );
        assert_eq!(
            a(Some("http://localhost:8099"), None).label(),
            "localhost:8099"
        );
        assert_eq!(
            a(Some("https://example.com/notes"), Some("bo")).label(),
            "bo on example.com"
        );
        assert_eq!(a(None, Some("ana")).label(), "On this computer");
    }
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub folder: FolderStatus,
    /// Goes up when the tray asks for the app's settings.
    pub show_settings: u64,
    /// While signing in: the code the server's page shows, to compare.
    #[serde(default)]
    pub sign_in_code: Option<String>,
    /// While taking over the CLI's Markdown folder: a CLI still runs it.
    #[serde(default)]
    pub cli_wait: Option<CliWait>,
    /// A newer release is out: the server runs it (docs/desktop.md "Update
    /// it").
    #[serde(default)]
    pub update: Option<Update>,
    /// The last notification of a mention clicked ([`notify_mention`]): its
    /// thread opens.
    #[serde(default)]
    pub opened: Option<Opened>,
}

/// A mention's thread, to open: `n` goes up with each click.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Opened {
    pub n: u64,
    pub vault: String,
    pub note: String,
    pub thread: String,
}

/// A newer release than this app.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Update {
    /// `0.24.0`.
    pub version: String,
    /// Its bar was dismissed.
    pub dismissed: bool,
}

/// Waiting for the CLI to stop ([`folder_take_from_cli`]).
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct CliWait {
    pub dir: String,
    pub profile: String,
    /// What stops its background sync, where the app couldn't.
    #[serde(default)]
    pub command: Option<String>,
}

/// A Markdown folder the CLI keeps for this account ([`cli_folders`]).
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct CliFolder {
    pub profile: String,
    pub shown: String,
    /// A CLI runs it now.
    pub running: bool,
    /// Its background sync; none where the app can't ask.
    #[serde(default)]
    pub service: Option<Service>,
}

#[derive(Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub struct Service {
    pub active: bool,
    pub enabled: bool,
}

/// The Markdown folder's sync; `dir` as people read it (`~/Notes`).
#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum FolderStatus {
    #[default]
    Off,
    Starting {
        dir: String,
    },
    Syncing {
        dir: String,
        vaults: usize,
    },
    Stopped {
        dir: String,
        error: String,
    },
}

/// A folder picked in the system's dialog.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Picked {
    pub pick: u64,
    pub shown: String,
    /// Why it can't be used.
    #[serde(default)]
    pub refused: Option<String>,
    /// Notes of its own, to bring in (or choose another folder).
    #[serde(default)]
    pub notes: Option<Holds>,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Holds {
    pub notes: usize,
    /// "12 notes, 3 folders and .obsidian".
    pub described: String,
}

/// What the window hands the app for the Markdown folder: this device as
/// it keeps it, and its session. The account secret stays here: the app
/// unwraps it from the device's own envelope and checks it against
/// `account`.
#[derive(Serialize)]
pub struct Handoff {
    pub user_id: String,
    pub display_name: String,
    pub device_id: String,
    pub device_secret: String,
    pub account: String,
    pub token: String,
    pub expires_at: i64,
}

async fn invoke<T: DeserializeOwned>(cmd: &str, args: Value) -> Result<T, String> {
    let w = util::window();
    let internals = js_sys::Reflect::get(&w, &"__TAURI_INTERNALS__".into())
        .map_err(|_| "not in the desktop app".to_string())?;
    let f: js_sys::Function = js_sys::Reflect::get(&internals, &"invoke".into())
        .ok()
        .and_then(|f| f.dyn_into().ok())
        .ok_or("not in the desktop app")?;
    let args = js_sys::JSON::parse(&args.to_string()).map_err(|_| "bad arguments".to_string())?;
    let promise: js_sys::Promise = f
        .call2(&internals, &JsValue::from_str(cmd), &args)
        .ok()
        .and_then(|p| p.dyn_into().ok())
        .ok_or("the app didn't answer")?;
    let v = JsFuture::from(promise).await.map_err(|e| {
        e.as_string()
            .unwrap_or_else(|| "the app couldn't do that".into())
    })?;
    let text = if v.is_undefined() || v.is_null() {
        "null".to_string()
    } else {
        js_sys::JSON::stringify(&v)
            .ok()
            .and_then(|s| s.as_string())
            .unwrap_or_else(|| "null".into())
    };
    serde_json::from_str(&text).map_err(|e| format!("the app's answer: {e}"))
}

/// Ask the app about itself (at start, and after its settings changed).
pub async fn refresh() -> Result<Info, String> {
    let info: Info = invoke("desktop_info", json!({})).await?;
    INFO.with(|i| *i.borrow_mut() = Some(info.clone()));
    Ok(info)
}

pub fn info() -> Option<Info> {
    INFO.with(|i| i.borrow().clone())
}

/// Where the API is: the server the person chose in the app
/// (`https://notes.example.com`), or with notes only on this computer the
/// app's own (never to be shown: [`this_computer`]).
pub fn server() -> Option<String> {
    INFO.with(|i| i.borrow().as_ref().and_then(|i| i.server.clone()))
}

/// The notes are only on this computer, with the app's own server.
pub fn this_computer() -> bool {
    INFO.with(|i| i.borrow().as_ref().is_some_and(|i| i.this_computer))
}

/// The app's accounts, in the order added.
pub fn accounts() -> Vec<Account> {
    INFO.with(|i| {
        i.borrow()
            .as_ref()
            .map(|i| i.accounts.clone())
            .unwrap_or_default()
    })
}

/// Where the notes are, as people read it: the server's address, or
/// "this computer".
pub fn place() -> String {
    if this_computer() {
        "this computer".into()
    } else {
        server().unwrap_or_default()
    }
}

/// The account *On this computer*, added if it isn't there yet, and
/// opened: the app starts again.
pub async fn use_this_computer() -> Result<(), String> {
    invoke("use_this_computer", json!({})).await
}

/// Notes only on this computer: the app's own server runs (it tries
/// again if it failed to start), or why not.
pub async fn local_start() -> Result<(), String> {
    invoke("local_start", json!({})).await
}

/// Notes only on this computer: a session from the app's own server.
pub async fn local_session() -> Result<SessionResponse, String> {
    invoke("local_session", json!({})).await
}

/// Add an account on a server (the app asks first, in a system dialog,
/// then checks it) and open it: the app starts again.
pub async fn add_server(url: &str) -> Result<(), String> {
    invoke("add_server", json!({ "url": url })).await
}

/// Open another of the app's accounts: the app starts again.
pub async fn switch_account(id: &str) -> Result<(), String> {
    invoke("switch_account", json!({ "id": id })).await
}

/// Take an account off the list (the app asks first); what it keeps for it
/// stays.
pub async fn remove_account(id: &str) -> Result<(), String> {
    invoke("remove_account", json!({ "id": id })).await
}

/// The open account's name, for the list.
pub fn account_seen(name: &str) {
    if is_desktop() {
        fire("account_seen", json!({ "name": name }));
    }
}

/// Sign in through the system browser; ends when the browser came back.
pub async fn sign_in() -> Result<SessionResponse, String> {
    invoke("sign_in", json!({})).await
}

pub fn cancel_sign_in() {
    fire("cancel_sign_in", json!({}));
}

pub async fn set_close_to_tray(on: bool) -> Result<(), String> {
    invoke("set_close_to_tray", json!({ "on": on })).await
}

pub async fn set_autostart(on: bool) -> Result<(), String> {
    invoke("set_autostart", json!({ "on": on })).await
}

/// The system's folder picker; `None` when cancelled.
pub async fn choose_folder() -> Result<Option<Picked>, String> {
    invoke("choose_folder", json!({})).await
}

/// Keep the picked folder in sync (the app asks for the person's yes).
pub async fn folder_start(handoff: Handoff, pick: u64) -> Result<(), String> {
    invoke(
        "folder_start",
        json!({ "handoff": serde_json::to_value(handoff).unwrap_or_default(), "pick": pick }),
    )
    .await
}

/// Bring the notes in the folder picked as `pick` into the vault named
/// `vault` (one of yours, or a new one): moved into its folder there, or
/// copied into one in the folder picked as `copy_to`. The app asks first.
pub async fn folder_bring(
    handoff: Handoff,
    pick: u64,
    copy_to: Option<u64>,
    vault: &str,
) -> Result<(), String> {
    invoke(
        "folder_bring",
        json!({ "handoff": serde_json::to_value(handoff).unwrap_or_default(), "pick": pick,
                "copy_to": copy_to, "vault": vault }),
    )
    .await
}

/// The Markdown folders the CLI keeps for the person whose session `token`
/// is, on this account's server.
pub async fn cli_folders(token: &str) -> Result<Vec<CliFolder>, String> {
    invoke("cli_folders", json!({ "token": token })).await
}

/// Take over the CLI's folder of `profile` (the app asks first); ends once
/// the app keeps it, or when cancelled ([`cancel_take`]).
pub async fn folder_take_from_cli(handoff: Handoff, profile: &str) -> Result<(), String> {
    invoke(
        "folder_take_from_cli",
        json!({ "handoff": serde_json::to_value(handoff).unwrap_or_default(), "profile": profile }),
    )
    .await
}

pub fn cancel_take() {
    fire("cancel_take", json!({}));
}

/// The open account's initial setup was shown.
pub async fn setup_done() -> Result<(), String> {
    invoke("setup_done", json!({})).await
}

/// Whether the open account still has its initial setup to see.
pub fn needs_setup() -> bool {
    is_desktop() && accounts().iter().any(|a| a.open && !a.set_up)
}

/// Don't show the bar about release `version` again.
pub fn dismiss_update(version: &str) {
    fire("dismiss_update", json!({ "version": version }));
}

/// How to update, on the docs' page, in the default browser.
pub fn update_help() {
    fire("update_help", json!({}));
}

/// Notifications of mentions turned on: the app has the system show its
/// (macOS asks the person, once), or says why it won't.
pub async fn notifications_allow() -> Result<(), String> {
    invoke("notifications_allow", json!({})).await
}

/// A system notification of a mention, shown by the app (the window's own
/// can't bring it back from the tray, and macOS's have none): clicking it
/// brings the window back and opens the thread ([`Status::opened`]).
pub fn notify_mention(title: &str, body: &str, vault: &str, note: &str, thread: &str) {
    fire(
        "notify_mention",
        json!({ "title": title, "body": body,
                "at": { "vault": vault, "note": note, "thread": thread } }),
    );
}

pub async fn folder_off() -> Result<(), String> {
    invoke("folder_off", json!({})).await
}

pub async fn folder_retry() -> Result<(), String> {
    invoke("folder_retry", json!({})).await
}

pub async fn reveal_folder() -> Result<(), String> {
    invoke("reveal_folder", json!({})).await
}

fn fire(cmd: &'static str, args: Value) {
    leptos::task::spawn_local(async move {
        if let Err(e) = invoke::<()>(cmd, args).await {
            util::warn(&format!("{cmd}: {e}"));
        }
    });
}

/// This window's session, bound to this device (each time the window
/// opens): the Markdown folder goes on with it, if it was another.
pub fn session_changed(token: &str, expires_at: i64) {
    if is_desktop() {
        fire(
            "session_changed",
            json!({ "token": token, "expires_at": expires_at }),
        );
    }
}

/// This window signed out: the folder stops.
pub fn signed_out() {
    if is_desktop() {
        fire("signed_out", json!({}));
    }
}

/// This computer forgets the account: the folder's keys and copies too.
pub fn forget_computer() {
    if is_desktop() {
        fire("forget_computer", json!({}));
    }
}

/// Follow the app's status for as long as the window lives: `f` hears each
/// new one.
pub fn watch(f: impl Fn(Status) + 'static) {
    leptos::task::spawn_local(async move {
        let mut since = info().map(|i| i.status_version).unwrap_or(0);
        loop {
            match invoke::<(u64, Status)>("next_event", json!({ "since": since })).await {
                Ok((v, s)) => {
                    if v != since {
                        since = v;
                        INFO.with(|i| {
                            if let Some(i) = i.borrow_mut().as_mut() {
                                i.status = s.clone();
                                i.status_version = v;
                            }
                        });
                        f(s);
                    }
                }
                Err(_) => util::sleep(2_000).await,
            }
        }
    });
}

/// "this browser", or in the desktop app "this computer".
pub fn here() -> &'static str {
    if is_desktop() {
        "this computer"
    } else {
        "this browser"
    }
}

/// "in this browser", or in the desktop app "on this computer".
pub fn in_here() -> &'static str {
    if is_desktop() {
        "on this computer"
    } else {
        "in this browser"
    }
}

/// How to get a newer version of notekpr: "reload the page", or in the
/// desktop app, whose files are its own, "update the app".
pub fn get_newer() -> &'static str {
    if is_desktop() {
        "update the app"
    } else {
        "reload the page"
    }
}
