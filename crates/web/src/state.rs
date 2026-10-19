//! Application state.
//!
//! [`Core`] holds everything non-reactive (keys, CRDT replicas, the socket)
//! in a thread-local `RefCell`. [`Ui`] is a bundle of Leptos signals the views
//! render from. Rule: never call into the editor (or anything that may call
//! back into `with`) while holding the `Core` borrow.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use coolpsonotes_core::crypto::Purpose;
use coolpsonotes_core::doc::{EntryKind, NoteDoc};
use coolpsonotes_core::engine::{self, ObjectMeta, Presence, VaultMeta, VaultReplica};
use coolpsonotes_core::keys::{AccountPublic, AccountSecret, DeviceKey, RecoveryKey};
use coolpsonotes_core::protocol::*;
use coolpsonotes_core::{paths, random_id, textdiff};
use leptos::prelude::Update;
use leptos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::editor::{Editor, RemoteCursor};
use crate::storage::{self, DeviceRecord, UiPrefs};
use crate::util::{self, now_ms};
use crate::ws::{ConnState, Socket};
use crate::{api, sizes};

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Loading,
    SignedOut {
        error: Option<String>,
    },
    Setup,
    ShowRecovery {
        key: String,
    },
    Enroll {
        pending: Option<(String, String)>,
        error: Option<String>,
    },
    Ready,
    Fatal(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    Home,
    Vault(String),
    Note(String, String),
    Trash(String),
    Settings(String),
    Device(Option<String>),
    /// A note shared with us individually (we are not a member of its vault).
    Shared(String, String),
}

impl Route {
    pub fn path(&self) -> String {
        match self {
            Route::Home => "/".into(),
            Route::Vault(v) => format!("/v/{v}"),
            Route::Note(v, n) => format!("/v/{v}/{n}"),
            Route::Trash(v) => format!("/v/{v}/trash"),
            Route::Settings(t) => format!("/settings/{t}"),
            Route::Device(Some(c)) => format!("/device?code={}", api::q(c)),
            Route::Device(None) => "/device".into(),
            Route::Shared(v, n) => format!("/s/{v}/{n}"),
        }
    }

    pub fn parse(path: &str, search: &str) -> Route {
        let parts: Vec<&str> = path
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        match parts.as_slice() {
            ["v", v] => Route::Vault(v.to_string()),
            ["v", v, "trash"] => Route::Trash(v.to_string()),
            ["v", v, n] => Route::Note(v.to_string(), n.to_string()),
            ["s", v, n] => Route::Shared(v.to_string(), n.to_string()),
            ["settings"] => Route::Settings("account".into()),
            ["settings", t] => Route::Settings(t.to_string()),
            ["device"] => {
                let code = search
                    .trim_start_matches('?')
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("code="))
                    .map(|c| {
                        js_sys::decode_uri_component(c)
                            .ok()
                            .and_then(|s| s.as_string())
                            .unwrap_or_else(|| c.to_string())
                    });
                Route::Device(code)
            }
            _ => Route::Home,
        }
    }

    pub fn vault(&self) -> Option<&str> {
        match self {
            Route::Vault(v) | Route::Note(v, _) | Route::Trash(v) => Some(v),
            _ => None,
        }
    }

    pub fn note(&self) -> Option<(&str, &str)> {
        match self {
            Route::Note(v, n) => Some((v, n)),
            _ => None,
        }
    }

    pub fn shared(&self) -> Option<(&str, &str)> {
        match self {
            Route::Shared(v, n) => Some((v, n)),
            _ => None,
        }
    }

    /// The note on screen, whether through its vault or shared individually.
    pub fn any_note(&self) -> Option<(&str, &str)> {
        self.note().or_else(|| self.shared())
    }
}

/// A note shared with us individually, for the views.
#[derive(Clone, Debug, PartialEq)]
pub struct SharedView {
    pub vault: String,
    pub note: String,
    pub title: String,
    pub role: Role,
    pub owner: String,
    pub added_at: i64,
    pub loaded: bool,
    pub error: Option<String>,
}

/// Bookkeeping for one note shared with us.
pub struct ShareState {
    pub info: SharedNote,
    pub title: String,
    /// Highest sequence number of this note we have seen (for resubscribing).
    pub seq: i64,
    pub loaded: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VaultView {
    pub id: String,
    pub name: String,
    pub role: Role,
    pub shared: bool,
    pub archived: bool,
    pub owner_id: String,
    pub loaded: bool,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewMode {
    Live,
    Source,
    Reading,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    pub id: u64,
    pub kind: ToastKind,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PresenceView {
    pub conn: u64,
    pub user_id: String,
    pub name: String,
    pub color: String,
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Modal {
    QuickSwitch,
    Search(String),
    NewVault,
    Share(String),
    ShareNote(String, String),
    VaultSettings(String),
    History(String, String),
    ApproveDevice(DeviceInfo),
    Help,
    Rename { vault: String, id: String },
    NewFolder { vault: String, parent: String },
    Move { vault: String, id: String },
    Import(String),
}

#[derive(Clone, Copy)]
pub struct Ui {
    pub phase: RwSignal<Phase>,
    pub user: RwSignal<Option<UserInfo>>,
    pub server: RwSignal<Option<ServerInfo>>,
    pub vaults: RwSignal<Vec<VaultView>>,
    /// Notes shared with us individually.
    pub shared: RwSignal<Vec<SharedView>>,
    /// Bumped when who a note is shared with changes.
    pub shares_rev: RwSignal<u64>,
    pub route: RwSignal<Route>,
    pub tree_rev: RwSignal<u64>,
    pub content_rev: RwSignal<u64>,
    pub conn: RwSignal<ConnState>,
    pub pending: RwSignal<usize>,
    pub presence: RwSignal<Vec<PresenceView>>,
    pub toasts: RwSignal<Vec<Toast>>,
    pub modal: RwSignal<Option<Modal>>,
    pub pending_devices: RwSignal<Vec<DeviceInfo>>,
    pub sidebar: RwSignal<bool>,
    pub mode: RwSignal<ViewMode>,
    pub prefs_rev: RwSignal<u64>,
    pub ui_prefs: RwSignal<UiPrefs>,
    /// Side panel shown as an overlay on narrow screens (not persisted).
    pub panel_overlay: RwSignal<bool>,
    pub warning: RwSignal<Option<String>>,
}

thread_local! {
    static UI: Cell<Option<Ui>> = const { Cell::new(None) };
    static CORE: RefCell<Core> = RefCell::new(Core::default());
    static TOAST_ID: Cell<u64> = const { Cell::new(1) };
}

pub fn init_ui() -> Ui {
    let p = storage::ui_prefs();
    let mode = match p.editor_mode.as_str() {
        "source" => ViewMode::Source,
        "reading" => ViewMode::Reading,
        _ => ViewMode::Live,
    };
    let ui = Ui {
        phase: RwSignal::new(Phase::Loading),
        user: RwSignal::new(None),
        server: RwSignal::new(None),
        vaults: RwSignal::new(vec![]),
        shared: RwSignal::new(vec![]),
        shares_rev: RwSignal::new(0),
        route: RwSignal::new(current_route()),
        tree_rev: RwSignal::new(0),
        content_rev: RwSignal::new(0),
        conn: RwSignal::new(ConnState::Connecting),
        pending: RwSignal::new(0),
        presence: RwSignal::new(vec![]),
        toasts: RwSignal::new(vec![]),
        modal: RwSignal::new(None),
        pending_devices: RwSignal::new(vec![]),
        sidebar: RwSignal::new(false),
        mode: RwSignal::new(mode),
        prefs_rev: RwSignal::new(0),
        ui_prefs: RwSignal::new(p),
        panel_overlay: RwSignal::new(false),
        warning: RwSignal::new(None),
    };
    UI.with(|u| u.set(Some(ui)));
    ui
}

pub fn ui() -> Ui {
    UI.with(|u| u.get()).expect("UI initialised")
}

pub fn with<R>(f: impl FnOnce(&mut Core) -> R) -> R {
    CORE.with(|c| f(&mut c.borrow_mut()))
}

pub fn current_route() -> Route {
    let loc = util::window().location();
    Route::parse(
        &loc.pathname().unwrap_or_default(),
        &loc.search().unwrap_or_default(),
    )
}

pub fn toast(kind: ToastKind, text: impl Into<String>) {
    let id = TOAST_ID.with(|t| {
        let v = t.get();
        t.set(v + 1);
        v
    });
    let ui = ui();
    ui.toasts.update(|t| {
        t.push(Toast {
            id,
            kind,
            text: text.into(),
        })
    });
    let ms = if kind == ToastKind::Error { 8000 } else { 4000 };
    util::set_timeout(ms, move || ui.toasts.update(|t| t.retain(|x| x.id != id)));
}

pub fn error(text: impl Into<String>) {
    toast(ToastKind::Error, text);
}

pub fn info(text: impl Into<String>) {
    toast(ToastKind::Info, text);
}

pub fn success(text: impl Into<String>) {
    toast(ToastKind::Success, text);
}

/// Encrypted, synced per-user data.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Prefs {
    #[serde(default)]
    pub favorites: Vec<(String, String)>,
    #[serde(default)]
    pub recent: Vec<(String, String)>,
    /// Trust-on-first-use pins: user id → account key fingerprint.
    #[serde(default)]
    pub pins: BTreeMap<String, String>,
}

pub struct PresenceEntry {
    pub vault: String,
    /// The note this was announced on (`None`: vault-wide).
    pub scope: Option<String>,
    pub user_id: String,
    pub presence: Presence,
    pub seen: i64,
}

#[derive(Default)]
pub struct Core {
    pub user_id: String,
    pub me: Option<MeResponse>,
    pub account: Option<AccountSecret>,
    pub device_id: Option<String>,
    pub vault_infos: BTreeMap<String, VaultInfo>,
    pub replicas: BTreeMap<String, VaultReplica>,
    pub vault_errors: BTreeMap<String, String>,
    /// Vaults whose content can be shown (restored from this browser's copy or synced).
    pub loaded: BTreeSet<String>,
    /// Vaults that finished their first catch-up this session.
    pub synced: BTreeSet<String>,
    pub cache: crate::cache::Tracker,
    pub socket: Option<Rc<Socket>>,
    pub conn_id: u64,
    pub outbox: BTreeMap<String, Vec<PushUpdate>>,
    pub prefs: Prefs,
    pub prefs_version: i64,
    pub editor: Option<(String, String, Rc<Editor>)>,
    pub presence: BTreeMap<u64, PresenceEntry>,
    pub attachment_urls: HashMap<String, String>,
    pub attachment_loading: HashSet<String>,
    pub dirty: BTreeSet<(String, String)>,
    pub last_touch: HashMap<(String, String), i64>,
    pub snapshotting: HashSet<(String, String)>,
    pub users: HashMap<String, PublicUser>,
    /// Notes shared with us individually, by (vault, note). Their content
    /// lives in a members-less replica of the vault (`VaultReplica::member == false`).
    pub shares: BTreeMap<(String, String), ShareState>,
    /// Recipient replicas whose stored unsent changes were restored.
    pub shares_restored: BTreeSet<String>,
    /// Vaults / shared notes whose keys are being reloaded.
    pub reloading: HashSet<String>,
    /// Reloads asked for while one was running: run once more afterwards.
    pub reload_again: HashSet<String>,
    /// The shared note we last announced our presence on.
    pub presence_note: Option<(String, String)>,
    pub flush: util::Debounce,
    pub prefs_save: util::Debounce,
    pub presence_debounce: util::Debounce,
    pub content_debounce: util::Debounce,
    pub cursor: Option<(usize, usize)>,
    /// Unsent updates too large for the server that the user was told about.
    pub oversized_reported: HashSet<String>,
    /// When we last told the user that local storage is full.
    pub storage_warned_at: i64,
    /// Keys of a first-device setup whose upload failed, reused on retry.
    pub setup: Option<engine::AccountBootstrap>,
}

impl Core {
    pub fn replica(&self, v: &str) -> Option<&VaultReplica> {
        self.replicas.get(v)
    }

    /// Number of documents with changes not yet confirmed by the server.
    pub fn pending_count(&self) -> usize {
        self.replicas
            .iter()
            .map(|(v, r)| {
                r.pending
                    .iter()
                    .map(|p| (v, &p.object_id))
                    .collect::<BTreeSet<_>>()
                    .len()
            })
            .sum()
    }
}

// ------------------------------------------------------------------ boot

pub fn device_name() -> String {
    let ua = util::window().navigator().user_agent().unwrap_or_default();
    let browser = if ua.contains("Firefox/") {
        "Firefox"
    } else if ua.contains("Edg/") {
        "Edge"
    } else if ua.contains("Chrome/") || ua.contains("Chromium/") {
        "Chrome"
    } else if ua.contains("Safari/") {
        "Safari"
    } else {
        "Browser"
    };
    let os = if ua.contains("Android") {
        "Android"
    } else if ua.contains("iPhone") || ua.contains("iPad") {
        "iOS"
    } else if ua.contains("Mac OS") {
        "macOS"
    } else if ua.contains("Windows") {
        "Windows"
    } else if ua.contains("Linux") {
        "Linux"
    } else {
        "unknown OS"
    };
    format!("{browser} on {os}")
}

pub async fn boot() {
    let ui = ui();
    api::on_unauthorized(|| {
        let ui = self::ui();
        if ui.phase.get_untracked() != (Phase::SignedOut { error: None }) {
            sign_out_local(Some("Your session ended. Please sign in again.".into()));
        }
    });
    match api::get::<ServerInfo>("/info").await {
        Ok(i) => ui.server.set(Some(i)),
        Err(e) => {
            ui.phase
                .set(Phase::Fatal(format!("Cannot reach the server: {e}")));
            return;
        }
    }
    let loc = util::window().location();
    if loc.pathname().unwrap_or_default() == "/auth/complete" {
        complete_oidc().await;
        return;
    }
    let Some(token) = storage::token() else {
        ui.phase.set(Phase::SignedOut { error: None });
        return;
    };
    api::set_token(Some(token));
    after_login().await;
}

fn fragment_param(frag: &str, key: &str) -> Option<String> {
    frag.trim_start_matches('#').split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| {
            js_sys::decode_uri_component(v)
                .ok()
                .and_then(|s| s.as_string())
                .unwrap_or_else(|| v.to_string())
        })
    })
}

async fn complete_oidc() {
    let ui = ui();
    let loc = util::window().location();
    let frag = loc.hash().unwrap_or_default();
    // Remove the one-time code from the address bar and history right away.
    let _ = util::window()
        .history()
        .map(|h| h.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some("/")));
    if let Some(err) = fragment_param(&frag, "error") {
        let msg = match err.as_str() {
            "account_disabled" => "This account has been disabled by an administrator.".to_string(),
            "access_denied" => "Sign-in was cancelled.".to_string(),
            "login_expired" => "The sign-in took too long. Please try again.".to_string(),
            _ => "Sign-in failed. Please try again.".to_string(),
        };
        ui.phase.set(Phase::SignedOut { error: Some(msg) });
        return;
    }
    let Some(code) = fragment_param(&frag, "code") else {
        ui.phase.set(Phase::SignedOut { error: None });
        return;
    };
    let return_to = fragment_param(&frag, "return_to").unwrap_or_else(|| "/".into());
    match api::post::<_, SessionResponse>("/auth/exchange", &ExchangeRequest { code }).await {
        Ok(s) => {
            storage::set_token(&s.token);
            api::set_token(Some(s.token));
            if return_to.starts_with('/') && !return_to.starts_with("//") {
                let _ = util::window().history().map(|h| {
                    h.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&return_to))
                });
                ui.route.set(current_route());
            }
            after_login().await;
        }
        Err(e) => ui.phase.set(Phase::SignedOut {
            error: Some(format!("Sign-in failed: {e}")),
        }),
    }
}

pub async fn dev_login(username: String) {
    let ui = ui();
    match api::post::<_, SessionResponse>(
        "/auth/dev-login",
        &DevLoginRequest {
            username,
            display_name: None,
        },
    )
    .await
    {
        Ok(s) => {
            storage::set_token(&s.token);
            api::set_token(Some(s.token));
            after_login().await;
        }
        Err(e) => ui.phase.set(Phase::SignedOut {
            error: Some(e.message),
        }),
    }
}

pub async fn after_login() {
    let ui = ui();
    ui.phase.set(Phase::Loading);
    let me = match api::get::<MeResponse>("/me").await {
        Ok(m) => m,
        Err(e) if e.status == 401 => {
            sign_out_local(None);
            return;
        }
        Err(e) => {
            ui.phase
                .set(Phase::Fatal(format!("Cannot load your account: {e}")));
            return;
        }
    };
    ui.user.set(Some(me.user.clone()));
    let uid = me.user.id.clone();
    let has_key = me.account_key.is_some();
    with(|c| {
        c.user_id = uid.clone();
        c.me = Some(me);
    });
    if !has_key {
        ui.phase.set(Phase::Setup);
        return;
    }
    if let Some(rec) = storage::device(&uid) {
        match api::get::<DeviceEnvelope>(&format!("/devices/{}/envelope", rec.device_id)).await {
            Ok(env) => match unlock_with_device(&uid, &rec, &env.account_envelope) {
                Unlock::Unlocked => {
                    enter_ready().await;
                    return;
                }
                Unlock::WrongAccount => {
                    ui.phase.set(Phase::Fatal(WRONG_ACCOUNT.into()));
                    return;
                }
                Unlock::Unreadable => storage::clear_device(&uid),
            },
            Err(e) if e.status == 409 => {
                show_pending(&uid, rec);
                return;
            }
            Err(e) if e.status == 404 => storage::clear_device(&uid),
            Err(e) => {
                ui.phase
                    .set(Phase::Fatal(format!("Cannot check this device: {e}")));
                return;
            }
        }
    }
    ui.phase.set(Phase::Enroll {
        pending: None,
        error: None,
    });
}

enum Unlock {
    Unlocked,
    /// Not for this device (or the record is damaged).
    Unreadable,
    /// A valid secret, but of another account key than this device was
    /// approved for: someone (possibly the server) substituted keys.
    WrongAccount,
}

const WRONG_ACCOUNT: &str = "The server sent this browser keys of a different account than the one it was approved for. This can mean the server was tampered with. Nothing was decrypted with them; check your devices from another device before signing in here again.";

/// The account key the server publishes for us, checked.
fn published_account(uid: &str) -> Option<AccountPublic> {
    with(|c| {
        let key = c.me.as_ref()?.account_key.as_ref()?;
        engine::published_account(uid, key).ok()
    })
}

fn unlock_with_device(uid: &str, rec: &DeviceRecord, envelope: &[u8]) -> Unlock {
    let Some(secret) = coolpsonotes_core::b64::decode32(&rec.secret) else {
        return Unlock::Unreadable;
    };
    let Ok(dev) = DeviceKey::from_secret(secret) else {
        return Unlock::Unreadable;
    };
    let Ok(acct) = dev.unwrap_account(envelope, uid, &rec.device_id) else {
        return Unlock::Unreadable;
    };
    // Pinned when the approval code was shown; records from before pinning
    // pin the published key now.
    let Some(expected) = rec.account().or_else(|| published_account(uid)) else {
        return Unlock::WrongAccount;
    };
    if engine::check_approved_account(&acct, &expected).is_err() {
        return Unlock::WrongAccount;
    }
    if rec.account.is_none() {
        let mut r = rec.clone();
        r.pin(&expected);
        storage::set_device(uid, &r);
    }
    with(|c| {
        c.account = Some(acct);
        c.device_id = Some(rec.device_id.clone());
    });
    Unlock::Unlocked
}

fn show_pending(uid: &str, mut rec: DeviceRecord) {
    let ui = ui();
    let Some(secret) = coolpsonotes_core::b64::decode32(&rec.secret) else {
        return;
    };
    let Ok(dev) = DeviceKey::from_secret(secret) else {
        return;
    };
    // The code covers the account key this browser expects; the approving
    // device shows one computed from the real key.
    let account = match rec.account() {
        Some(a) => a,
        None => {
            let Some(a) = published_account(uid) else {
                ui.phase.set(Phase::Fatal(
                    "Your account key on the server failed verification.".into(),
                ));
                return;
            };
            rec.pin(&a);
            storage::set_device(uid, &rec);
            a
        }
    };
    let code =
        engine::verification_code(uid, &rec.device_id, &dev.public, &account).unwrap_or_default();
    ui.phase.set(Phase::Enroll {
        pending: Some((rec.device_id.clone(), code)),
        error: None,
    });
    poll_approval(uid.to_string(), rec);
}

fn poll_approval(uid: String, rec: DeviceRecord) {
    leptos::task::spawn_local(async move {
        loop {
            util::sleep(3000).await;
            let ui = ui();
            if !matches!(
                ui.phase.get_untracked(),
                Phase::Enroll {
                    pending: Some(_),
                    ..
                }
            ) {
                return;
            }
            match api::get::<DeviceEnvelope>(&format!("/devices/{}/envelope", rec.device_id)).await
            {
                Ok(env) => {
                    match unlock_with_device(&uid, &rec, &env.account_envelope) {
                        Unlock::Unlocked => {
                            success("This browser was approved.");
                            enter_ready().await;
                        }
                        Unlock::WrongAccount => ui.phase.set(Phase::Fatal(WRONG_ACCOUNT.into())),
                        Unlock::Unreadable => ui.phase.set(Phase::Enroll {
                            pending: None,
                            error: Some("Approval data was invalid.".into()),
                        }),
                    }
                    return;
                }
                Err(e) if e.status == 404 => {
                    storage::clear_device(&uid);
                    ui.phase.set(Phase::Enroll {
                        pending: None,
                        error: Some("The request was declined or removed.".into()),
                    });
                    return;
                }
                Err(_) => {}
            }
        }
    });
}

/// First device: generate keys, show the recovery key.
pub async fn setup_keys() {
    let ui = ui();
    let uid = with(|c| c.user_id.clone());
    let boot = match with(|c| c.setup.take()) {
        // A previous attempt whose upload failed: send the same keys again.
        Some(b) => b,
        None => {
            // An earlier attempt may have succeeded although its answer got lost.
            if storage::device(&uid).is_some()
                && let Ok(me) = api::get::<MeResponse>("/me").await
                && me.account_key.is_some()
            {
                after_login().await;
                return;
            }
            match engine::bootstrap_account(&uid, &device_name()) {
                Ok(b) => b,
                Err(e) => {
                    error(format!("Key generation failed: {e}"));
                    return;
                }
            }
        }
    };
    // Keep this browser's device key before the server holds an account key
    // that only this device can unwrap: a lost answer must not lose it.
    let previous = storage::device(&uid);
    storage::set_device(&uid, &{
        let mut r = DeviceRecord {
            device_id: boot.device_id.clone(),
            secret: coolpsonotes_core::b64::encode(&boot.device.secret),
            account: None,
        };
        r.pin(&boot.account.public());
        r
    });
    if let Err(e) = api::put::<_, ()>("/me/account-key", &boot.request).await {
        let stored = match api::get::<MeResponse>("/me").await {
            // Our keys did arrive; only the answer was lost.
            Ok(me) if me.account_key.as_ref() == Some(&boot.request.key) => true,
            // Set up meanwhile from elsewhere (e.g. another tab): ours are useless.
            Ok(me) if me.account_key.is_some() => {
                match &previous {
                    Some(p) => storage::set_device(&uid, p),
                    None => storage::clear_device(&uid),
                }
                after_login().await;
                return;
            }
            _ => false,
        };
        if !stored {
            // Not stored, or we can't tell: retrying sends the same keys.
            with(|c| c.setup = Some(boot));
            error(format!("Could not save your keys: {e}"));
            return;
        }
    }
    let key = boot.recovery.to_display();
    with(|c| {
        c.account = Some(boot.account);
        c.device_id = Some(boot.device_id);
    });
    ui.phase.set(Phase::ShowRecovery { key });
}

/// After the recovery key was saved: create a first vault with a welcome note.
pub async fn finish_setup() {
    enter_ready().await;
    if with(|c| c.replicas.is_empty())
        && let Some(v) = crate::actions::create_vault("My notes".into()).await
    {
        let id = crate::actions::create_note_with(&v, "", "Welcome", WELCOME).await;
        if let Some(id) = id {
            navigate(Route::Note(v, id));
        }
    }
}

const WELCOME: &str = "# Welcome to coolpsonotes 👋\n\nThis is your private vault. Everything you write is **encrypted on this device** before it is synced — the server never sees your notes.\n\n## Getting started\n\n- [ ] Write something here — just start typing\n- [ ] Create a new note with the **+** button (or Ctrl+Alt+N)\n- [ ] Link notes by typing `[[` and a note name, like [[Ideas]]\n- [ ] Add #tags anywhere\n- [ ] Share a vault with family from the vault menu\n\nUse the toolbar for formatting, or Markdown if you know it: `**bold**`, `*italic*`, `# Heading`, `- list`.\n\nPress **Ctrl+K** to jump to any note, and **?** for all keyboard shortcuts.\n";

/// Register this browser as a new (pending) device and wait for approval.
pub async fn request_approval() {
    let ui = ui();
    let uid = with(|c| c.user_id.clone());
    let (dev, reg) = engine::new_device(&device_name());
    if let Err(e) = api::post::<_, ()>("/devices", &reg).await {
        ui.phase.set(Phase::Enroll {
            pending: None,
            error: Some(e.message),
        });
        return;
    }
    let rec = DeviceRecord {
        device_id: reg.device_id.clone(),
        secret: coolpsonotes_core::b64::encode(&dev.secret),
        account: None,
    };
    storage::set_device(&uid, &rec);
    show_pending(&uid, rec);
}

/// Unlock with the recovery key and enroll this browser without another device.
pub async fn recover(input: String) {
    let ui = ui();
    let fail = |m: String| {
        ui.phase.set(Phase::Enroll {
            pending: None,
            error: Some(m),
        })
    };
    let Ok(rk) = RecoveryKey::parse(&input) else {
        fail("That doesn't look like a valid recovery key. Check for typos.".into());
        return;
    };
    let uid = with(|c| c.user_id.clone());
    let env: RecoveryEnvelope = match api::get("/me/recovery").await {
        Ok(e) => e,
        Err(e) => return fail(e.message),
    };
    let account = match rk.unwrap_account(&env.recovery_envelope, &uid) {
        Ok(a) => a,
        Err(_) => return fail("This recovery key does not belong to this account.".into()),
    };
    // Reuse a pending device of this browser or register a new one.
    let mut rec = match storage::device(&uid) {
        Some(r) => r,
        None => {
            let (dev, reg) = engine::new_device(&device_name());
            if let Err(e) = api::post::<_, ()>("/devices", &reg).await {
                return fail(e.message);
            }
            DeviceRecord {
                device_id: reg.device_id.clone(),
                secret: coolpsonotes_core::b64::encode(&dev.secret),
                account: None,
            }
        }
    };
    // The recovery key opened the real account secret: this is the key to expect.
    rec.pin(&account.public());
    storage::set_device(&uid, &rec);
    let Some(secret) = coolpsonotes_core::b64::decode32(&rec.secret) else {
        return fail("Corrupt device record".into());
    };
    let Ok(dev) = DeviceKey::from_secret(secret) else {
        return fail("Corrupt device record".into());
    };
    let req = match engine::approve_device(&account, &uid, &rec.device_id, &dev.public) {
        Ok(r) => r,
        Err(e) => return fail(e.to_string()),
    };
    if let Err(e) = api::post::<_, ()>(&format!("/devices/{}/activate", rec.device_id), &req).await
    {
        return fail(e.message);
    }
    with(|c| {
        c.account = Some(account);
        c.device_id = Some(rec.device_id.clone());
    });
    success("Recovered. This browser is now one of your devices.");
    enter_ready().await;
}

pub fn sign_out_local(error: Option<String>) {
    let ui = ui();
    // Unsent changes go to local storage, to be sent after the next sign-in.
    flush_now();
    storage::clear_token();
    api::set_token(None);
    let (sock, urls) = with(|c| {
        let s = c.socket.take();
        let urls = take_attachment_urls(c, None);
        *c = Core::default();
        (s, urls)
    });
    if let Some(s) = sock {
        s.stop();
    }
    // Decrypted data must not outlive the session.
    revoke_urls(urls);
    crate::knowledge::clear_caches();
    ui.vaults.set(vec![]);
    ui.shared.set(vec![]);
    ui.user.set(None);
    ui.modal.set(None);
    ui.phase.set(Phase::SignedOut { error });
}

pub async fn sign_out() {
    flush_now();
    let _ = api::post_empty::<()>("/auth/logout").await;
    sign_out_local(None);
}

/// Sign out and make this browser forget the account: remove this device
/// from the account and delete its key and unsent changes from local storage.
/// Signing in here again needs approval from another device or the recovery key.
pub async fn forget_browser() {
    flush_now();
    let unsent = with(|c| c.pending_count());
    let warn = if unsent > 0 {
        format!(
            " {unsent} note{} with changes that are not synced yet will lose them.",
            if unsent == 1 { "" } else { "s" }
        )
    } else {
        String::new()
    };
    // Without another device, the recovery key is the only way back in.
    let others = api::get::<Vec<DeviceInfo>>("/devices")
        .await
        .map(|d| {
            d.iter()
                .filter(|d| !d.current && d.status == DeviceStatus::Active)
                .count()
        })
        .unwrap_or(1);
    let last = if others == 0 {
        " This is your only device: afterwards only your recovery key can open your notes. Without it they are lost for good."
    } else {
        ""
    };
    if !util::confirm(&format!(
        "Forget this browser? You will be signed out and this browser's key will be deleted, so opening your notes here again needs approval from another device or your recovery key.{last}{warn}"
    )) {
        return;
    }
    let (uid, device) = with(|c| (c.user_id.clone(), c.device_id.clone()));
    // Removing the device also ends its sessions, this one included.
    let removed = match &device {
        Some(d) => match api::delete(&format!("/devices/{d}")).await {
            Ok(()) | Err(api::Error { status: 404, .. }) => true,
            Err(_) => false,
        },
        None => true,
    };
    if !removed || device.is_none() {
        let _ = api::post_empty::<()>("/auth/logout").await;
    }
    sign_out_local(None);
    // After signing out, which stores unsent changes.
    storage::forget_user(&uid);
    crate::cache::forget_user(&uid).await;
    if removed {
        success("This browser forgot your account.");
    } else {
        error(
            "This browser forgot your keys, but could not remove itself from your devices. Remove it in Settings → Devices on another device.",
        );
    }
}

async fn enter_ready() {
    let ui = ui();
    let device_id = with(|c| c.device_id.clone());
    if let Some(d) = device_id {
        let _ = api::put::<_, ()>("/me/session/device", &BindDeviceRequest { device_id: d }).await;
    }
    load_prefs().await;
    refresh_vaults().await;
    refresh_shares().await;
    connect_socket();
    ui.phase.set(Phase::Ready);
    if matches!(ui.route.get_untracked(), Route::Home) {
        let uid = with(|c| c.user_id.clone());
        if let Some(last) = storage::last_location(&uid) {
            let r = Route::parse(&last, "");
            let known = match &r {
                Route::Shared(v, n) => with(|c| c.shares.contains_key(&(v.clone(), n.clone()))),
                _ => r
                    .vault()
                    .is_some_and(|v| with(|c| c.replicas.get(v).is_some_and(|x| x.member))),
            };
            if known {
                navigate_replace(r);
            }
        }
    }
    refresh_pending_devices().await;
}

// ------------------------------------------------------------------ prefs

async fn load_prefs() {
    let Ok(blob) = api::get::<PrefsBlob>("/me/prefs").await else {
        return;
    };
    with(|c| {
        c.prefs_version = blob.version;
        if let (Some(ct), Some(acct)) = (&blob.ciphertext, &c.account)
            && let Ok(pt) = acct.open_private(&c.user_id, ct)
            && let Ok(p) = serde_json::from_slice::<Prefs>(&pt)
        {
            c.prefs = p;
        }
    });
    ui().prefs_rev.update(|v| *v += 1);
}

pub fn save_prefs() {
    ui().prefs_rev.update(|v| *v += 1);
    let d = with(|c| c.prefs_save.clone());
    d.run(1500, || {
        leptos::task::spawn_local(async {
            for _ in 0..3 {
                let req = with(|c| {
                    let acct = c.account.as_ref()?;
                    let pt = serde_json::to_vec(&c.prefs).ok()?;
                    Some(PutPrefsRequest {
                        ciphertext: acct.seal_private(&c.user_id, &pt),
                        expected_version: c.prefs_version,
                    })
                });
                let Some(req) = req else { return };
                match api::put::<_, PrefsBlob>("/me/prefs", &req).await {
                    Ok(b) => {
                        with(|c| c.prefs_version = b.version);
                        return;
                    }
                    Err(e) if e.status == 409 => {
                        // Changed on another device: merge and retry.
                        let mine = with(|c| c.prefs.clone());
                        load_prefs().await;
                        with(|c| {
                            for f in mine.favorites {
                                if !c.prefs.favorites.contains(&f) {
                                    c.prefs.favorites.push(f);
                                }
                            }
                            for (u, fp) in mine.pins {
                                c.prefs.pins.entry(u).or_insert(fp);
                            }
                            let mut recent = mine.recent;
                            for r in c.prefs.recent.drain(..) {
                                if !recent.contains(&r) {
                                    recent.push(r);
                                }
                            }
                            recent.truncate(30);
                            c.prefs.recent = recent;
                        });
                    }
                    Err(_) => return,
                }
            }
        });
    });
}

pub fn set_ui_prefs(f: impl FnOnce(&mut UiPrefs)) {
    let ui = ui();
    ui.ui_prefs.update(|p| {
        f(p);
        storage::set_ui_prefs(p);
    });
}

// ------------------------------------------------------------------ vaults

pub async fn refresh_vaults() {
    let infos: Vec<VaultInfo> = match api::get("/vaults").await {
        Ok(v) => v,
        Err(e) => {
            if !e.is_offline() {
                error(format!("Could not load vaults: {e}"));
            }
            return;
        }
    };
    let ids: BTreeSet<String> = infos.iter().map(|v| v.id.clone()).collect();
    crate::cache::prune(&ids);
    let removed: Vec<String> = with(|c| {
        c.replicas
            .iter()
            .filter(|(k, r)| r.member && !ids.contains(*k))
            .map(|(k, _)| k.clone())
            .collect()
    });
    for v in &removed {
        drop_vault(v);
    }
    // In parallel: each needs its own key request (and local copy read).
    futures::future::join_all(infos.into_iter().map(load_vault)).await;
    publish_vaults();
}

/// Load (or refresh) one vault's keys and metadata.
pub async fn load_vault(info: VaultInfo) {
    load_vault_with(info, false).await
}

/// Reload a vault's keys, e.g. after a note in it got its own key.
pub async fn reload_vault_keys(vault_id: String) {
    if !begin_reload(&vault_id).await {
        return;
    }
    loop {
        if let Ok(info) = api::get::<VaultInfo>(&format!("/vaults/{vault_id}")).await {
            load_vault_with(info, true).await;
            publish_vaults();
        }
        if !with(|c| c.reload_again.remove(&vault_id)) {
            break;
        }
    }
    with(|c| c.reloading.remove(&vault_id));
}

/// Start reloading the keys behind `tag`. If a reload is already running,
/// it may have fetched the keys before they changed: ask it to run once
/// more, wait until it is done and return `false`.
async fn begin_reload(tag: &str) -> bool {
    if with(|c| c.reloading.insert(tag.to_string())) {
        with(|c| c.reload_again.remove(tag));
        return true;
    }
    with(|c| c.reload_again.insert(tag.to_string()));
    while with(|c| c.reloading.contains(tag)) {
        util::sleep(50).await;
    }
    false
}

async fn load_vault_with(info: VaultInfo, force: bool) {
    let (needs_keys, uid) = with(|c| {
        let needs = match c.replicas.get(&info.id) {
            None => true,
            Some(r) => force || !r.member || r.keyring.current_epoch() < Some(info.key_epoch),
        };
        (needs, c.user_id.clone())
    });
    let id = info.id.clone();
    if needs_keys {
        let stored = if with(|c| c.replicas.get(&id).is_none_or(|r| !r.member)) {
            crate::cache::fetch(&uid, &id).await
        } else {
            None
        };
        let keys: VaultKeysResponse = match api::get(&format!("/vaults/{id}/keys")).await {
            Ok(k) => k,
            Err(e) => {
                with(|c| c.vault_errors.insert(id.clone(), e.message.clone()));
                return;
            }
        };
        let ring = with(|c| {
            c.account
                .as_ref()
                .map(|a| engine::keyring_from_response(a, &uid, &id, &keys))
        });
        match ring {
            Some(Ok(ring)) => {
                let meta = VaultMeta::decrypt(&ring, &info.encrypted_meta)
                    .unwrap_or_else(|_| VaultMeta::named("(unreadable)"));
                let docs = engine::object_keyrings(&ring, &keys);
                // Notes shared with us individually are now reachable through the vault.
                let was_recipient = with(|c| c.replicas.get(&id).is_some_and(|r| !r.member));
                if was_recipient {
                    for (v, o) in with(|c| {
                        c.shares
                            .keys()
                            .filter(|(v, _)| *v == id)
                            .cloned()
                            .collect::<Vec<_>>()
                    }) {
                        send(&ClientMessage::Unsubscribe {
                            vault_id: v.clone(),
                            object_id: Some(o.clone()),
                        });
                        with(|c| c.shares.remove(&(v, o)));
                    }
                    with(|c| c.replicas.remove(&id));
                    publish_shares();
                }
                let (is_new, applied, docs_changed, restored) = with(|c| {
                    c.vault_errors.remove(&id);
                    match c.replicas.get_mut(&id) {
                        Some(r) => {
                            let docs_changed = r.doc_keys.keys().ne(docs.keys())
                                || r.doc_keys.iter().any(|(o, k)| {
                                    docs.get(o).map(|d| d.current_epoch())
                                        != Some(k.current_epoch())
                                });
                            r.keyring = ring;
                            r.meta = meta;
                            r.role = info.role;
                            r.doc_keys = docs;
                            let (applied, _) = r.retry_deferred();
                            (false, applied, docs_changed, false)
                        }
                        None => {
                            let mut r = VaultReplica::new(&id, info.role, ring, meta);
                            r.doc_keys = docs;
                            r.max_head_seen = storage::head_seen(&uid, &id);
                            let restored =
                                crate::cache::restore_into(&mut c.cache, &uid, &mut r, stored);
                            if restored {
                                c.loaded.insert(id.clone());
                            }
                            c.replicas.insert(id.clone(), r);
                            (true, engine::Applied::default(), false, restored)
                        }
                    }
                });
                if is_new {
                    subscribe(&id);
                    if restored {
                        show_loaded(&id);
                    }
                } else {
                    after_apply(&id, applied);
                    if docs_changed {
                        ui().tree_rev.update(|v| *v += 1);
                        ui().shares_rev.update(|v| *v += 1);
                        // Unsent edits of a note that just got its own key must be re-encrypted.
                        if with(|c| c.loaded.contains(&id)) {
                            resend_pending(&id);
                        }
                    }
                }
            }
            Some(Err(e)) => {
                with(|c| c.vault_errors.insert(id.clone(), e.to_string()));
            }
            None => {}
        }
    } else {
        with(|c| {
            if let Some(r) = c.replicas.get_mut(&id) {
                r.role = info.role;
                if let Ok(m) = VaultMeta::decrypt(&r.keyring, &info.encrypted_meta) {
                    r.meta = m;
                }
            }
        });
    }
    with(|c| c.vault_infos.insert(id, info));
}

/// Remove the cached blob URLs of decrypted attachments (of one vault, or
/// all); the caller revokes them with [`revoke_urls`] outside the borrow.
fn take_attachment_urls(c: &mut Core, vault: Option<&str>) -> Vec<String> {
    let prefix = vault.map(|v| format!("{v}:"));
    let keys: Vec<String> = c
        .attachment_urls
        .keys()
        .filter(|k| prefix.as_ref().is_none_or(|p| k.starts_with(p.as_str())))
        .cloned()
        .collect();
    keys.iter()
        .filter_map(|k| c.attachment_urls.remove(k))
        .collect()
}

fn revoke_urls(urls: Vec<String>) {
    for u in urls {
        let _ = web_sys::Url::revoke_object_url(&u);
    }
}

fn drop_vault(v: &str) {
    let urls = with(|c| {
        c.replicas.remove(v);
        c.vault_infos.remove(v);
        c.loaded.remove(v);
        c.synced.remove(v);
        c.outbox.remove(v);
        take_attachment_urls(c, Some(v))
    });
    revoke_urls(urls);
    crate::cache::forget_vault(v);
    let ui = ui();
    if ui.route.get_untracked().vault() == Some(v) {
        navigate(Route::Home);
    }
}

pub fn publish_vaults() {
    let views = with(|c| {
        let mut v: Vec<VaultView> = c
            .vault_infos
            .values()
            .map(|i| {
                let r = c.replicas.get(&i.id);
                VaultView {
                    id: i.id.clone(),
                    name: r
                        .map(|r| r.meta.name.clone())
                        .unwrap_or_else(|| "Locked vault".into()),
                    role: i.role,
                    shared: i.member_count > 1,
                    archived: i.archived,
                    owner_id: i.owner_id.clone(),
                    loaded: c.loaded.contains(&i.id),
                    error: c.vault_errors.get(&i.id).cloned(),
                }
            })
            .collect();
        v.sort_by(|a, b| {
            a.archived
                .cmp(&b.archived)
                .then(a.shared.cmp(&b.shared))
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        v
    });
    ui().vaults.set(views);
}

// ------------------------------------------------------------------ notes shared with us

/// Load the list of notes shared with us individually, their keys and titles.
pub async fn refresh_shares() {
    refresh_shares_with(false).await
}

/// Like [`refresh_shares`], and tell the user about notes newly shared with them.
async fn refresh_shares_announcing() {
    refresh_shares_with(true).await
}

async fn refresh_shares_with(announce: bool) {
    let list: Vec<SharedNote> = match api::get("/shares").await {
        Ok(l) => l,
        Err(e) => {
            if !e.is_offline() {
                util::warn(&format!("could not load shared notes: {e}"));
            }
            return;
        }
    };
    // Notes of vaults we are a member of are reachable through the vault.
    let list: Vec<SharedNote> = with(|c| {
        list.into_iter()
            .filter(|s| !c.replicas.get(&s.vault_id).is_some_and(|r| r.member))
            .collect()
    });
    let keep: BTreeSet<(String, String)> = list
        .iter()
        .map(|s| (s.vault_id.clone(), s.object_id.clone()))
        .collect();
    let removed: Vec<(String, String)> = with(|c| {
        c.shares
            .keys()
            .filter(|k| !keep.contains(*k))
            .cloned()
            .collect()
    });
    for (v, o) in removed {
        drop_share(&v, &o);
    }
    let before: BTreeSet<(String, String)> = with(|c| c.shares.keys().cloned().collect());
    for info in list {
        load_share(info).await;
    }
    publish_shares();
    if announce {
        let new: Vec<(String, String)> = with(|c| {
            c.shares
                .iter()
                .filter(|(k, s)| !before.contains(*k) && s.error.is_none())
                .map(|(_, s)| (s.info.owner.display_name.clone(), s.title.clone()))
                .collect()
        });
        for (owner, title) in new {
            info(format!(
                "{owner} shared “{title}” with you. It's under “Shared with me” in the sidebar."
            ));
        }
    }
}

async fn load_share(info: SharedNote) {
    let key = (info.vault_id.clone(), info.object_id.clone());
    let current = with(|c| {
        c.shares
            .get(&key)
            .map(|s| s.error.is_none() && s.info.key_epoch == info.key_epoch)
    });
    if current == Some(true) {
        // Same key: only the title or role may have changed.
        with(|c| {
            let title = c
                .replicas
                .get(&key.0)
                .and_then(|r| r.doc_keys.get(&key.1))
                .and_then(|ring| {
                    info.encrypted_meta
                        .as_ref()
                        .and_then(|m| ObjectMeta::decrypt(ring, &key.1, m).ok())
                })
                .map(|m| m.title);
            if let Some(s) = c.shares.get_mut(&key) {
                if let Some(t) = title {
                    s.title = t;
                }
                s.info = info;
            }
        });
        return;
    }
    // Not subscribed yet: new, or its key could not be loaded before.
    let unsubscribed = with(|c| c.shares.get(&key).is_none_or(|s| s.error.is_some()));
    if current.is_none() {
        with(|c| {
            c.shares.insert(
                key.clone(),
                ShareState {
                    info: info.clone(),
                    title: "Shared note".into(),
                    seq: 0,
                    loaded: false,
                    error: None,
                },
            )
        });
    }
    reload_share_keys(&key.0, &key.1).await;
    if unsubscribed && with(|c| c.shares.get(&key).is_some_and(|s| s.error.is_none())) {
        subscribe_share(&key.0, &key.1);
    }
}

/// (Re)load the document key of a note shared with us.
pub async fn reload_share_keys(vault: &str, note: &str) {
    let tag = format!("{vault}/{note}");
    if !begin_reload(&tag).await {
        return;
    }
    loop {
        reload_share_keys_once(vault, note).await;
        if !with(|c| c.reload_again.remove(&tag)) {
            break;
        }
    }
    with(|c| c.reloading.remove(&tag));
}

async fn reload_share_keys_once(vault: &str, note: &str) {
    let key = (vault.to_string(), note.to_string());
    let res = api::get::<ObjectKeysResponse>(&format!("/vaults/{vault}/objects/{note}/keys")).await;
    let keys = match res {
        Ok(k) => k,
        Err(e) if e.status == 404 => {
            drop_share(vault, note);
            publish_shares();
            return;
        }
        Err(e) => {
            with(|c| {
                if let Some(s) = c.shares.get_mut(&key) {
                    s.error = Some(e.message.clone());
                }
            });
            publish_shares();
            return;
        }
    };
    let ring = with(|c| {
        c.account
            .as_ref()
            .map(|a| engine::object_keyring_for_recipient(a, &c.user_id, vault, &keys))
    });
    let ring = match ring {
        Some(Ok(r)) => r,
        Some(Err(e)) => {
            with(|c| {
                if let Some(s) = c.shares.get_mut(&key) {
                    s.error = Some(e.to_string());
                }
            });
            publish_shares();
            return;
        }
        None => return,
    };
    let applied = with(|c| {
        let title = keys
            .encrypted_meta
            .as_ref()
            .and_then(|m| ObjectMeta::decrypt(&ring, note, m).ok())
            .map(|m| m.title);
        if let Some(s) = c.shares.get_mut(&key) {
            s.error = None;
            s.info.role = keys.role;
            s.info.key_epoch = keys.current_epoch;
            if let Some(t) = title {
                s.title = t;
            }
        }
        let r = c
            .replicas
            .entry(vault.to_string())
            .or_insert_with(|| VaultReplica::for_shared_notes(vault));
        if r.member {
            return engine::Applied::default();
        }
        r.doc_keys.insert(note.to_string(), ring);
        // The replica's role only gates sending; each note's own role is checked by the server.
        r.role = if c
            .shares
            .iter()
            .any(|((v, _), s)| v == vault && s.info.role.can_write())
        {
            Role::Editor
        } else {
            Role::Viewer
        };
        r.retry_deferred().0
    });
    after_apply(vault, applied);
    publish_shares();
}

fn drop_share(vault: &str, note: &str) {
    let key = (vault.to_string(), note.to_string());
    let Some(_) = with(|c| c.shares.remove(&key)) else {
        return;
    };
    send(&ClientMessage::Unsubscribe {
        vault_id: vault.to_string(),
        object_id: Some(note.to_string()),
    });
    with(|c| {
        let last = !c.shares.keys().any(|(v, _)| v == vault);
        if let Some(r) = c.replicas.get_mut(vault).filter(|r| !r.member) {
            r.doc_keys.remove(note);
            r.notes.remove(note);
            r.pending.retain(|p| p.object_id != note);
            r.deferred.clear();
            if last {
                c.replicas.remove(vault);
                c.shares_restored.remove(vault);
            }
        }
        c.outbox.remove(vault);
    });
    persist_pending(vault);
    update_pending_count();
    if ui().route.get_untracked().shared() == Some((vault, note)) {
        navigate(Route::Home);
    }
}

pub fn publish_shares() {
    let mut views: Vec<SharedView> = with(|c| {
        c.shares
            .iter()
            .map(|((v, n), s)| SharedView {
                vault: v.clone(),
                note: n.clone(),
                title: s.title.clone(),
                role: s.info.role,
                owner: s.info.owner.display_name.clone(),
                added_at: s.info.added_at,
                loaded: s.loaded,
                error: s.error.clone(),
            })
            .collect()
    });
    views.sort_by_key(|v| v.title.to_lowercase());
    let ui = ui();
    if ui.shared.get_untracked() != views {
        ui.shared.set(views);
    }
}

fn subscribe_share(vault: &str, note: &str) {
    let since = with(|c| {
        c.shares
            .get(&(vault.to_string(), note.to_string()))
            .map(|s| s.seq)
    });
    if let Some(since) = since {
        send(&ClientMessage::Subscribe {
            vault_id: vault.to_string(),
            since: Some(since),
            object_id: Some(note.to_string()),
        });
    }
}

fn on_share_synced(vault: &str, note: &str, head: i64) {
    let first = with(|c| {
        let Some(s) = c.shares.get_mut(&(vault.to_string(), note.to_string())) else {
            return false;
        };
        s.seq = s.seq.max(head);
        let first = !s.loaded;
        s.loaded = true;
        first
    });
    if first {
        publish_shares();
        // Unsent changes from an earlier visit, once every shared note of this vault is loaded.
        let restore = with(|c| {
            c.shares
                .iter()
                .filter(|((v, _), _)| v == vault)
                .all(|(_, s)| s.loaded || s.error.is_some())
                && c.shares_restored.insert(vault.to_string())
        });
        if restore {
            restore_stored_pending(vault);
        }
        if ui().route.get_untracked().shared() == Some((vault, note)) {
            ui().content_rev.update(|v| *v += 1);
            send_presence();
        }
    }
    resend_pending(vault);
}

/// Whether we reach this vault as a member (not only through shared notes).
pub fn is_member(vault: &str) -> bool {
    with(|c| c.replicas.get(vault).is_some_and(|r| r.member))
}

/// Whether a note of a vault we are a member of has its own key (is shared individually).
pub fn is_shared_note(vault: &str, note: &str) -> bool {
    with(|c| {
        c.replicas
            .get(vault)
            .is_some_and(|r| r.member && r.is_shared_object(note))
    })
}

/// Whether we may edit a note (vault role, or the role of its individual share).
pub fn can_write_note(vault: &str, note: &str) -> bool {
    with(|c| match c.replicas.get(vault) {
        Some(r) if r.member => r.can_write(),
        _ => c
            .shares
            .get(&(vault.to_string(), note.to_string()))
            .is_some_and(|s| s.info.role.can_write()),
    })
}

/// Reload whatever keys are missing for content of this vault.
fn keys_missing(vault: &str) {
    if is_member(vault) {
        leptos::task::spawn_local(reload_vault_keys(vault.to_string()));
    } else {
        let notes: Vec<String> = with(|c| {
            c.shares
                .keys()
                .filter(|(v, _)| v == vault)
                .map(|(_, n)| n.clone())
                .collect()
        });
        let v = vault.to_string();
        leptos::task::spawn_local(async move {
            for n in notes {
                reload_share_keys(&v, &n).await;
            }
            resend_pending(&v);
        });
    }
}

fn on_object_event(vault: String, note: String, event: ObjectEventKind) {
    if is_member(&vault) {
        match event {
            ObjectEventKind::Shared | ObjectEventKind::KeyRotated => {
                leptos::task::spawn_local(reload_vault_keys(vault))
            }
            ObjectEventKind::SharesChanged => ui().shares_rev.update(|v| *v += 1),
            // Members read titles from the vault index.
            ObjectEventKind::MetaChanged
            | ObjectEventKind::AccessRevoked
            | ObjectEventKind::Deleted => {}
        }
        return;
    }
    match event {
        ObjectEventKind::Shared | ObjectEventKind::KeyRotated => {
            leptos::task::spawn_local(async move {
                reload_share_keys(&vault, &note).await;
                resend_pending(&vault);
            });
        }
        ObjectEventKind::MetaChanged | ObjectEventKind::SharesChanged => {
            leptos::task::spawn_local(refresh_shares())
        }
        ObjectEventKind::AccessRevoked | ObjectEventKind::Deleted => {
            let title = with(|c| {
                c.shares
                    .get(&(vault.clone(), note.clone()))
                    .map(|s| s.title.clone())
            });
            drop_share(&vault, &note);
            publish_shares();
            if let Some(t) = title {
                info(if event == ObjectEventKind::Deleted {
                    format!("“{t}” was deleted.")
                } else {
                    format!("“{t}” is no longer shared with you.")
                });
            }
        }
    }
}

// ------------------------------------------------------------------ socket

fn connect_socket() {
    let Some(token) = api::token() else { return };
    let on_message = Rc::new(|m: ServerMessage| on_server_message(m));
    let on_state = Rc::new(|s: ConnState| {
        let ui = ui();
        ui.conn.set(s);
    });
    let sock = Socket::new(token, on_message, on_state);
    with(|c| c.socket = Some(sock));
    // Reconnect promptly when the browser comes back online.
    let cb = wasm_bindgen::closure::Closure::wrap(Box::new(|| {
        if let Some(s) = with(|c| c.socket.clone()) {
            s.kick();
        }
    }) as Box<dyn FnMut()>);
    use wasm_bindgen::JsCast;
    let _ = util::window().add_event_listener_with_callback("online", cb.as_ref().unchecked_ref());
    cb.forget();
}

fn send(m: &ClientMessage) -> bool {
    match with(|c| c.socket.clone()) {
        Some(s) if s.is_ready() => s.send(m),
        _ => false,
    }
}

fn subscribe(vault_id: &str) {
    let since = with(|c| c.replicas.get(vault_id).map(|r| r.seq));
    if let Some(since) = since {
        send(&ClientMessage::Subscribe {
            vault_id: vault_id.to_string(),
            since: Some(since),
            object_id: None,
        });
    }
}

fn on_server_message(m: ServerMessage) {
    match m {
        ServerMessage::Ready { conn_id, .. } => {
            with(|c| {
                c.conn_id = conn_id;
                c.presence_note = None;
            });
            let vaults: Vec<String> = with(|c| {
                c.replicas
                    .iter()
                    .filter(|(_, r)| r.member)
                    .map(|(k, _)| k.clone())
                    .collect()
            });
            for v in vaults {
                subscribe(&v);
            }
            let shares: Vec<(String, String)> = with(|c| {
                c.shares
                    .iter()
                    .filter(|(_, s)| s.error.is_none())
                    .map(|(k, _)| k.clone())
                    .collect()
            });
            for (v, n) in shares {
                subscribe_share(&v, &n);
            }
            send_presence();
        }
        ServerMessage::Changes { vault_id, changes } => {
            let res = with(|c| {
                let r = c.replicas.get_mut(&vault_id)?;
                let res = r.apply_changes(&changes);
                Some((res, r.needs_keys()))
            });
            if let Some(((applied, errors), needs_keys)) = res {
                if !errors.is_empty() {
                    util::warn(&format!(
                        "{} items in vault could not be decrypted",
                        errors.len()
                    ));
                }
                after_apply(&vault_id, applied);
                if needs_keys {
                    keys_missing(&vault_id);
                }
            }
        }
        ServerMessage::Synced {
            vault_id,
            head_seq,
            object_id,
        } => match object_id {
            Some(o) => on_share_synced(&vault_id, &o, head_seq),
            None => on_synced(&vault_id, head_seq),
        },
        ServerMessage::Update { vault_id, update } => {
            with(|c| {
                if let Some(s) = c
                    .shares
                    .get_mut(&(vault_id.clone(), update.object_id.clone()))
                {
                    s.seq = s.seq.max(update.seq);
                }
            });
            let res = with(|c| {
                c.replicas
                    .get_mut(&vault_id)
                    .map(|r| r.apply_update(&update))
            });
            match res {
                Some(Ok(applied)) => after_apply(&vault_id, applied),
                Some(Err(e)) if engine::missing_key(&e) => keys_missing(&vault_id),
                Some(Err(e)) => util::warn(&format!("could not apply update: {e}")),
                None => {}
            }
        }
        ServerMessage::Ack { vault_id, results } => {
            with(|c| {
                if let Some(r) = c.replicas.get_mut(&vault_id) {
                    r.ack(&results, true);
                }
            });
            persist_pending(&vault_id);
            update_pending_count();
            maybe_compact(&vault_id);
            crate::cache::schedule();
        }
        ServerMessage::SnapshotCreated {
            vault_id,
            object_id,
            ..
        } => {
            with(|c| {
                if let Some(r) = c.replicas.get_mut(&vault_id) {
                    r.snapshot_created(&object_id);
                }
            });
        }
        ServerMessage::Purged {
            vault_id,
            object_id,
            seq,
        } => {
            // Someone permanently deleted a note we still had unsent edits
            // for: keep our version as a new note rather than losing it.
            let rescue = with(|c| {
                let r = c.replicas.get(&vault_id)?;
                if !r.pending.iter().any(|p| p.object_id == object_id) {
                    return None;
                }
                let title = r
                    .index
                    .get(&object_id)
                    .map(|e| e.title().to_string())
                    .unwrap_or_else(|| "Untitled".into());
                Some((title, r.note_text(&object_id)))
            });
            let had_pending = rescue.is_some();
            if let Some((title, text)) = rescue {
                let v = vault_id.clone();
                leptos::task::spawn_local(async move {
                    let name = format!("{title} (recovered)");
                    crate::actions::create_note_with(&v, "", &name, &text).await;
                    info(format!(
                        "“{title}” was deleted by someone else; your unsent changes were saved as “{name}”."
                    ));
                });
            }
            let a = with(|c| {
                c.replicas
                    .get_mut(&vault_id)
                    .map(|r| r.apply_purge(&object_id, seq))
            });
            if let Some(a) = a {
                after_apply(&vault_id, a);
            }
            if had_pending {
                // A push with updates of the purged note is refused as a whole:
                // send the other updates again, without the dropped ones.
                persist_pending(&vault_id);
                resend_pending(&vault_id);
            }
        }
        ServerMessage::Snapshot { vault_id, snapshot } => {
            let res = with(|c| {
                c.replicas
                    .get_mut(&vault_id)
                    .map(|r| r.apply_snapshot(&snapshot))
            });
            if let Some(Ok(a)) = res {
                after_apply(&vault_id, a);
            }
        }
        ServerMessage::Awareness {
            vault_id,
            object_id,
            from_user,
            from_conn,
            payload,
        } => on_awareness(&vault_id, &object_id, &from_user, from_conn, &payload),
        ServerMessage::VaultEvent { vault_id, event } => on_vault_event(vault_id, event),
        ServerMessage::ObjectEvent {
            vault_id,
            object_id,
            event,
        } => on_object_event(vault_id, object_id, event),
        ServerMessage::UserEvent { event } => match event {
            UserEventKind::DevicePending | UserEventKind::DevicesChanged => {
                leptos::task::spawn_local(refresh_pending_devices());
            }
            UserEventKind::VaultsChanged => leptos::task::spawn_local(async {
                refresh_vaults().await;
                refresh_shares().await;
            }),
            UserEventKind::SharesChanged => leptos::task::spawn_local(refresh_shares_announcing()),
            UserEventKind::SessionRevoked => {
                sign_out_local(Some("This session was signed out.".into()))
            }
        },
        ServerMessage::Error {
            code,
            message,
            vault_id,
        } => {
            if code == "stale_key"
                && let Some(v) = vault_id
            {
                // A note got its own key (or a new one): reload keys and re-send.
                let v2 = v.clone();
                leptos::task::spawn_local(async move {
                    let before = key_epochs(&v2);
                    if is_member(&v2) {
                        reload_vault_keys(v2.clone()).await;
                    } else {
                        let notes: Vec<String> = with(|c| {
                            c.shares
                                .keys()
                                .filter(|(x, _)| *x == v2)
                                .map(|(_, n)| n.clone())
                                .collect()
                        });
                        for n in notes {
                            reload_share_keys(&v2, &n).await;
                        }
                    }
                    if key_epochs(&v2) != before {
                        resend_pending(&v2);
                    } else {
                        util::warn("server refused our key, and no newer key is available");
                    }
                });
            } else if code == "not_found" && vault_id.is_some() {
                leptos::task::spawn_local(async {
                    refresh_vaults().await;
                    refresh_shares().await;
                });
            } else if code == "too_large" {
                if let Some(v) = vault_id {
                    leptos::task::spawn_local(on_too_large(v));
                }
            } else if code == "forbidden" {
                error(format!("Not allowed: {message}"));
            } else if code == "conflict" {
                // e.g. a note was permanently deleted elsewhere while we edited it.
                error(message);
                // The whole push was refused: send the rest of it again.
                if let Some(v) = vault_id
                    && drop_rejected(&v)
                {
                    resend_pending(&v);
                }
            } else {
                util::warn(&format!("server error {code}: {message}"));
            }
        }
        ServerMessage::Pong { .. } => {}
    }
}

/// Current key epochs of a replica (vault key and every document key).
fn key_epochs(vault: &str) -> Vec<(String, Option<u32>)> {
    with(|c| {
        let Some(r) = c.replicas.get(vault) else {
            return vec![];
        };
        let mut v: Vec<(String, Option<u32>)> = r
            .doc_keys
            .iter()
            .map(|(o, k)| (o.clone(), k.current_epoch()))
            .collect();
        v.push((String::new(), r.keyring.current_epoch()));
        v.sort();
        v
    })
}

/// Pending updates for purged objects can never succeed (the text is kept
/// in a new note by the `Purged` handler). Returns whether any were dropped.
fn drop_rejected(vault_id: &str) -> bool {
    let dropped = with(|c| {
        let Some(r) = c.replicas.get_mut(vault_id) else {
            return false;
        };
        let before = r.pending.len();
        let purged = r.purged.clone();
        r.pending.retain(|p| !purged.contains(&p.object_id));
        r.pending.len() != before
    });
    persist_pending(vault_id);
    update_pending_count();
    dropped
}

fn on_synced(vault_id: &str, head: i64) {
    let (rolled_back, uid, first) = with(|c| {
        let uid = c.user_id.clone();
        c.loaded.insert(vault_id.to_string());
        let first = c.synced.insert(vault_id.to_string());
        let rb = c
            .replicas
            .get_mut(vault_id)
            .map(|r| r.synced(head))
            .unwrap_or(false);
        (rb, uid, first)
    });
    storage::set_head_seen(
        &uid,
        vault_id,
        with(|c| {
            c.replicas
                .get(vault_id)
                .map(|r| r.max_head_seen)
                .unwrap_or(head)
        }),
    );
    if first {
        restore_stored_pending(vault_id);
        show_loaded(vault_id);
    }
    if rolled_back {
        ui().warning.set(Some(
            "The server's copy of a vault is older than what this browser saw before (restored backup or a server problem). Your local changes are being re-sent.".into(),
        ));
        repush_everything(vault_id);
    }
    resend_pending(vault_id);
    maybe_compact(vault_id);
    crate::cache::schedule();
}

/// A vault's content became available: show it.
fn show_loaded(vault_id: &str) {
    publish_vaults();
    let ui = ui();
    if ui.route.get_untracked().vault() == Some(vault_id) {
        ui.tree_rev.update(|v| *v += 1);
        ui.content_rev.update(|v| *v += 1);
    }
}

/// Updates left unsent by a previous session (tab closed while offline):
/// apply them locally (they are ours) and send them again.
fn restore_stored_pending(vault_id: &str) {
    let uid = with(|c| c.user_id.clone());
    let stored = storage::pending(&uid, vault_id);
    if stored.is_empty() {
        return;
    }
    let mut applied = engine::Applied::default();
    with(|c| {
        let Some(r) = c.replicas.get_mut(vault_id) else {
            return;
        };
        for p in stored {
            if r.pending
                .iter()
                .any(|x| x.client_update_id == p.client_update_id)
            {
                continue;
            }
            if let Ok(plain) = r.decrypt_object(Purpose::Update, &p.object_id, &p.payload) {
                if p.object_id == INDEX_OBJECT_ID {
                    let _ = r.index.apply_update(&plain);
                    applied.index_changed = true;
                } else {
                    let _ = r
                        .notes
                        .entry(p.object_id.clone())
                        .or_default()
                        .apply_update(&plain);
                    applied.objects.insert(p.object_id.clone());
                }
                r.pending.push(engine::Pending {
                    client_update_id: p.client_update_id,
                    object_id: p.object_id,
                    update: plain,
                });
            }
        }
    });
    after_apply(vault_id, applied);
    info("Sending changes saved while you were offline.");
}

fn repush_everything(vault_id: &str) {
    with(|c| {
        let Some(r) = c.replicas.get_mut(vault_id) else {
            return;
        };
        if !r.can_write() || !r.member {
            return;
        }
        let mut items = vec![(INDEX_OBJECT_ID.to_string(), r.index.encode_state())];
        for (id, n) in &r.notes {
            items.push((id.clone(), n.encode_state()));
        }
        for (id, state) in items {
            let _ = r.local_change(&id, state);
        }
    });
    resend_pending(vault_id);
}

fn resend_pending(vault_id: &str) {
    let ups = with(|c| {
        c.replicas
            .get(vault_id)
            .map(|r| {
                encrypt_pending(r)
                    .into_iter()
                    .filter_map(|(_, u)| u)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
    push_all(vault_id, ups);
    update_pending_count();
}

/// Every unacknowledged update with its id, encrypted with the current keys.
/// One that can't be encrypted right now (`None`, e.g. its note's key is
/// being reloaded) doesn't keep the others from being sent or stored.
fn encrypt_pending(r: &VaultReplica) -> Vec<(String, Option<PushUpdate>)> {
    r.pending
        .iter()
        .map(|p| {
            let u = r
                .encrypt_object(Purpose::Update, &p.object_id, &p.update)
                .ok()
                .map(|payload| PushUpdate {
                    object_id: p.object_id.clone(),
                    client_update_id: p.client_update_id.clone(),
                    payload,
                });
            (p.client_update_id.clone(), u)
        })
        .collect()
}

/// The server's limit for one update (from `/info`).
pub fn max_update_bytes() -> usize {
    ui().server
        .get_untracked()
        .map(|s| s.max_update_bytes as usize)
        .unwrap_or(sizes::DEFAULT_MAX_UPDATE)
}

/// Send updates in pushes the server accepts. Updates too large to ever be
/// stored are held back (they stay pending on this device) and reported,
/// instead of making every push of the vault fail.
fn push_all(vault_id: &str, ups: Vec<PushUpdate>) {
    let (pushes, oversized) = sizes::batches(ups, max_update_bytes());
    for updates in pushes {
        // If the socket is down, updates stay in the replica's pending
        // queue and are re-sent after reconnecting.
        send(&ClientMessage::Push {
            vault_id: vault_id.to_string(),
            updates,
        });
    }
    if !oversized.is_empty() {
        report_oversized(vault_id, &oversized);
    }
}

fn report_oversized(vault_id: &str, ups: &[PushUpdate]) {
    let new: Vec<&PushUpdate> = with(|c| {
        ups.iter()
            .filter(|u| c.oversized_reported.insert(u.client_update_id.clone()))
            .collect()
    });
    let Some(biggest) = new.iter().max_by_key(|u| u.payload.len()) else {
        return;
    };
    let title = with(|c| {
        c.replicas
            .get(vault_id)
            .and_then(|r| r.index.get(&biggest.object_id))
            .map(|e| e.title().to_string())
            .or_else(|| {
                c.shares
                    .get(&(vault_id.to_string(), biggest.object_id.clone()))
                    .map(|s| s.title.clone())
            })
    });
    let what = match title {
        Some(t) => format!("A change to “{t}”"),
        None => "A change".to_string(),
    };
    util::warn(&format!(
        "holding back {} update(s) larger than the server limit",
        new.len()
    ));
    error(format!(
        "{what} is too large to sync ({}; the server accepts {} at a time). It is kept on this device only.",
        util::human_bytes(biggest.payload.len() as u64),
        util::human_bytes(max_update_bytes() as u64)
    ));
}

/// The server refused a push as too large: its limit may have gone down
/// since we asked. Refresh it and re-send what still fits.
async fn on_too_large(vault_id: String) {
    let before = max_update_bytes();
    if let Ok(info) = api::get::<ServerInfo>("/info").await {
        ui().server.set(Some(info));
    }
    if max_update_bytes() < before {
        resend_pending(&vault_id);
    } else {
        error("The server refused some changes as too large; they are kept on this device.");
    }
}

/// Total payload bytes waiting in the outbox of a vault.
pub fn outbox_bytes(vault_id: &str) -> usize {
    with(|c| {
        c.outbox
            .get(vault_id)
            .map(|v| v.iter().map(|u| u.payload.len()).sum())
            .unwrap_or(0)
    })
}

fn on_vault_event(vault_id: String, event: VaultEventKind) {
    match event {
        VaultEventKind::KeyRotated
        | VaultEventKind::MetaChanged
        | VaultEventKind::RoleChanged
        | VaultEventKind::MembersChanged => {
            leptos::task::spawn_local(async move {
                // From the list rather than `/vaults/{id}`: someone just removed
                // may get this before their `AccessRevoked`, and is handled by that.
                let Ok(infos) = api::get::<Vec<VaultInfo>>("/vaults").await else {
                    return;
                };
                if let Some(info) = infos.into_iter().find(|i| i.id == vault_id) {
                    let owner = info.role == Role::Owner;
                    load_vault(info).await;
                    publish_vaults();
                    if owner && event == VaultEventKind::MembersChanged {
                        crate::actions::grant_missing_keys(&vault_id).await;
                    }
                }
            });
        }
        VaultEventKind::Deleted | VaultEventKind::AccessRevoked => {
            let name = with(|c| c.replicas.get(&vault_id).map(|r| r.meta.name.clone()));
            drop_vault(&vault_id);
            publish_vaults();
            if let Some(n) = name {
                info(if event == VaultEventKind::Deleted {
                    format!("Vault “{n}” was deleted.")
                } else {
                    format!("You no longer have access to “{n}”.")
                });
            }
        }
    }
}

/// React to newly applied content: refresh the tree, the open note, panels.
pub fn after_apply(vault_id: &str, applied: engine::Applied) {
    if applied.is_empty() {
        return;
    }
    crate::cache::note_applied(vault_id, &applied);
    let ui = ui();
    let current = ui.route.get_untracked();
    if current.vault() == Some(vault_id) || current.shared().is_some_and(|(v, _)| v == vault_id) {
        if applied.index_changed || !applied.purged.is_empty() {
            ui.tree_rev.update(|v| *v += 1);
        }
        let d = with(|c| c.content_debounce.clone());
        d.run(250, move || ui.content_rev.update(|v| *v += 1));
    }
    // Push remote content into the open editor.
    let open = with(|c| {
        c.editor
            .as_ref()
            .map(|(v, n, e)| (v.clone(), n.clone(), e.clone()))
    });
    if let Some((v, n, ed)) = open {
        if v == vault_id && applied.objects.contains(&n) {
            let text = with(|c| c.replicas.get(&v).map(|r| r.note_text(&n)));
            if let Some(t) = text {
                ed.set_text(t, false);
            }
        }
        if v == vault_id && applied.purged.contains(&n) {
            info("This note was permanently deleted by someone else.");
            navigate(if is_member(&v) {
                Route::Vault(v.clone())
            } else {
                Route::Home
            });
        }
        if v == vault_id && applied.index_changed {
            // Link targets may have appeared or disappeared.
            ed.refresh();
        }
    }
}

// ------------------------------------------------------------------ local edits

/// Record a local CRDT update and schedule sending it.
pub fn local_update(vault_id: &str, object_id: &str, update: Vec<u8>) {
    let ok = with(|c| {
        let Some(r) = c.replicas.get_mut(vault_id) else {
            return false;
        };
        match r.local_change(object_id, update) {
            Ok(p) => {
                c.outbox.entry(vault_id.to_string()).or_default().push(p);
                true
            }
            Err(e) => {
                util::warn(&format!("encrypting update failed: {e}"));
                false
            }
        }
    });
    if ok {
        crate::cache::note_local(vault_id, object_id);
        schedule_flush();
    }
}

pub fn schedule_flush() {
    let d = with(|c| c.flush.clone());
    d.run(120, flush_now);
    update_pending_count();
}

pub fn flush_now() {
    let batches: Vec<(String, Vec<PushUpdate>)> =
        with(|c| std::mem::take(&mut c.outbox).into_iter().collect());
    for (v, ups) in batches {
        push_all(&v, ups);
        persist_pending(&v);
    }
    update_pending_count();
}

fn persist_pending(vault_id: &str) {
    let (uid, items) = with(|c| {
        (
            c.user_id.clone(),
            c.replicas.get(vault_id).map(encrypt_pending),
        )
    });
    let Some(items) = items else {
        // The replica is gone (e.g. the last note shared with us from this vault).
        let _ = storage::set_pending(&uid, vault_id, &[]);
        return;
    };
    let failed = items.iter().filter(|(_, u)| u.is_none()).count();
    let ups: Vec<PushUpdate> = if failed == 0 {
        items.into_iter().filter_map(|(_, u)| u).collect()
    } else {
        // Keep what was stored before for updates we can't encrypt now.
        util::warn(&format!("{failed} unsent update(s) could not be encrypted"));
        let mut stored = storage::pending(&uid, vault_id);
        let ups: Vec<PushUpdate> = items
            .into_iter()
            .filter_map(|(id, u)| {
                u.or_else(|| {
                    let i = stored.iter().position(|s| s.client_update_id == id)?;
                    Some(stored.swap_remove(i))
                })
            })
            .collect();
        if ups.is_empty() {
            // Never replace stored changes with nothing because of an error.
            return;
        }
        ups
    };
    if let Err(e) = storage::set_pending(&uid, vault_id, &ups) {
        util::warn(&format!("could not store unsent changes: {e}"));
        storage_full();
    }
}

/// Tell the user (at most once a minute) that unsent changes can't be kept.
fn storage_full() {
    let now = now_ms();
    if with(|c| std::mem::replace(&mut c.storage_warned_at, now)) > now - 60_000 {
        return;
    }
    error(
        "This browser's storage is full, so changes that are not synced yet would be lost if you close or reload this tab. Stay online until they are synced.",
    );
}

/// Replace a note's text through its CRDT (diffing against the current
/// text) and queue the change, split into updates small enough for the
/// server. Returns how many updates were made (0: unchanged or no such note).
pub fn set_note_text(vault: &str, note: &str, text: &str) -> usize {
    let budget = sizes::insert_budget(max_update_bytes());
    let ups = with(|c| {
        let n = c.replicas.get_mut(vault)?.notes.get_mut(note)?;
        let edits = textdiff::diff(&n.text(), text);
        Some(
            sizes::split_edits(edits, budget)
                .iter()
                .filter_map(|g| n.apply_edits(g))
                .collect::<Vec<_>>(),
        )
    })
    .unwrap_or_default();
    let n = ups.len();
    for u in ups {
        local_update(vault, note, u);
    }
    n
}

fn update_pending_count() {
    let n = with(|c| c.pending_count());
    ui().pending.set(n);
}

/// Index "modified" timestamps, throttled per note.
pub fn touch(vault_id: &str, note_id: &str, size: usize) {
    let key = (vault_id.to_string(), note_id.to_string());
    let now = now_ms();
    let due = with(|c| {
        let last = c.last_touch.get(&key).copied().unwrap_or(0);
        if now - last > 30_000 {
            c.last_touch.insert(key.clone(), now);
            true
        } else {
            false
        }
    });
    if due {
        // People a single note is shared with can't see (or change) the vault index.
        let up = with(|c| {
            c.replicas
                .get(vault_id)
                .filter(|r| r.member)
                .and_then(|r| r.index.touch(note_id, now, Some(size as u64)))
        });
        if let Some(u) = up {
            local_update(vault_id, INDEX_OBJECT_ID, u);
        }
    }
    with(|c| c.dirty.insert(key));
}

/// Compact objects with long update logs (and give history a revision).
pub fn maybe_compact(vault_id: &str) {
    let candidates = with(|c| {
        c.replicas
            .get(vault_id)
            .map(|r| r.compaction_candidates())
            .unwrap_or_default()
    });
    for o in candidates {
        snapshot(vault_id.to_string(), o);
    }
}

pub fn snapshot(vault_id: String, object_id: String) {
    let key = (vault_id.clone(), object_id.clone());
    let req = with(|c| {
        if c.snapshotting.contains(&key) {
            return None;
        }
        let r = c.replicas.get(&vault_id)?;
        if !r.can_write() || r.seq == 0 || r.pending.iter().any(|p| p.object_id == object_id) {
            return None;
        }
        let mut req = r.snapshot_request(&object_id).ok()?;
        if !r.member {
            // Our sequence number only means something for the note itself.
            let s = c.shares.get(&key)?;
            if !s.loaded || !s.info.role.can_write() {
                return None;
            }
            req.covers_seq = s.seq;
        }
        c.snapshotting.insert(key.clone());
        Some(req)
    });
    let Some(req) = req else { return };
    leptos::task::spawn_local(async move {
        // Jitter so several clients don't compact the same object at once.
        util::sleep((js_sys::Math::random() * 1500.0) as i32).await;
        let r = api::post::<_, SnapshotInfo>(
            &format!("/vaults/{vault_id}/objects/{object_id}/snapshots"),
            &req,
        )
        .await;
        with(|c| {
            c.snapshotting.remove(&key);
            if r.is_ok() {
                c.dirty.remove(&key);
                if let Some(rep) = c.replicas.get_mut(&vault_id) {
                    rep.snapshot_created(&object_id);
                }
            }
        });
    });
}

/// Called when leaving a note: store a revision if it was edited.
pub fn note_closed(vault_id: &str, note_id: &str) {
    flush_now();
    let dirty = with(|c| {
        c.dirty
            .contains(&(vault_id.to_string(), note_id.to_string()))
    });
    if dirty {
        let (v, n) = (vault_id.to_string(), note_id.to_string());
        // Wait for acks so the snapshot can cover our own updates.
        util::set_timeout(2500, move || snapshot(v, n));
    }
}

// ------------------------------------------------------------------ presence

pub fn send_presence() {
    let d = with(|c| c.presence_debounce.clone());
    d.run(200, || {
        let ui = ui();
        let route = ui.route.get_untracked();
        let name = ui
            .user
            .get_untracked()
            .map(|u| u.display_name)
            .unwrap_or_default();
        let uid = with(|c| c.user_id.clone());
        let cursor = with(|c| c.cursor);
        let vaults: Vec<String> = with(|c| c.loaded.iter().cloned().collect());
        for v in vaults {
            let here = route.vault() == Some(v.as_str());
            let p = Presence {
                name: name.clone(),
                color: util::user_color(&uid),
                object_id: if here {
                    route.note().map(|(_, n)| n.to_string())
                } else {
                    None
                },
                cursor: if here { cursor } else { None },
                at: now_ms(),
            };
            announce(&v, "", &p);
        }
        // On individually shared notes, also announce ourselves on the note
        // itself, so people it is shared with (who see nothing else) see us.
        let target = match &route {
            Route::Note(v, n) if is_shared_note(v, n) => Some((v.clone(), n.clone())),
            Route::Shared(v, n)
                if with(|c| {
                    c.shares
                        .get(&(v.clone(), n.clone()))
                        .is_some_and(|s| s.loaded)
                }) =>
            {
                Some((v.clone(), n.clone()))
            }
            _ => None,
        };
        let prev = with(|c| std::mem::replace(&mut c.presence_note, target.clone()));
        let base = Presence {
            name,
            color: util::user_color(&uid),
            object_id: None,
            cursor: None,
            at: now_ms(),
        };
        if let Some((v, n)) = prev.filter(|p| Some(p) != target.as_ref()) {
            announce(&v, &n, &base);
        }
        if let Some((v, n)) = target {
            announce(
                &v,
                &n,
                &Presence {
                    object_id: Some(n.clone()),
                    cursor,
                    ..base
                },
            );
        }
    });
}

fn announce(vault: &str, object_id: &str, p: &Presence) {
    let payload = with(|c| {
        let r = c.replicas.get(vault)?;
        r.encrypt_awareness(object_id, &serde_json::to_vec(p).ok()?)
            .ok()
    });
    if let Some(payload) = payload {
        send(&ClientMessage::Awareness {
            vault_id: vault.to_string(),
            object_id: object_id.to_string(),
            payload,
        });
    }
}

fn on_awareness(vault_id: &str, object_id: &str, from_user: &str, conn: u64, payload: &[u8]) {
    // Vault members announce themselves vault-wide and, on shared notes, on the
    // note too; once we know a connection vault-wide, its note-level messages
    // add nothing (and may arrive out of order), so they are ignored.
    let scope = (!object_id.is_empty()).then(|| object_id.to_string());
    let known_vault_wide = with(|c| {
        c.presence
            .get(&conn)
            .is_some_and(|e| e.scope.is_none() && now_ms() - e.seen < 45_000)
    });
    if scope.is_some() && known_vault_wide {
        return;
    }
    if payload.is_empty() {
        with(|c| c.presence.remove(&conn));
    } else {
        let p = with(|c| {
            c.replicas
                .get(vault_id)
                .and_then(|r| r.decrypt_awareness(object_id, payload).ok())
        });
        let Some(mut p) = p.and_then(|pt| serde_json::from_slice::<Presence>(&pt).ok()) else {
            return;
        };
        if let Some(o) = &scope {
            // A note-level announcement can only be about that note.
            if p.object_id.as_ref().is_some_and(|x| x != o) {
                p.object_id = None;
            }
        }
        let is_new = with(|c| {
            let new = !c.presence.contains_key(&conn);
            c.presence.insert(
                conn,
                PresenceEntry {
                    vault: vault_id.to_string(),
                    scope,
                    user_id: from_user.to_string(),
                    presence: p,
                    seen: now_ms(),
                },
            );
            new
        });
        if is_new {
            // Let the newcomer see us too.
            send_presence();
        }
    }
    publish_presence();
}

pub fn publish_presence() {
    let ui = ui();
    let route = ui.route.get_untracked();
    let now = now_ms();
    let (views, cursors, ed) = with(|c| {
        c.presence.retain(|_, e| now - e.seen < 45_000);
        let v = route
            .vault()
            .or(route.shared().map(|(v, _)| v))
            .map(str::to_string);
        // On a note shared with us, only people on that note are relevant.
        let only = route.shared().map(|(_, n)| n.to_string());
        let views: Vec<PresenceView> = c
            .presence
            .iter()
            .filter(|(_, e)| Some(&e.vault) == v.as_ref())
            .filter(|(_, e)| only.is_none() || e.presence.object_id == only)
            .map(|(conn, e)| PresenceView {
                conn: *conn,
                user_id: e.user_id.clone(),
                name: e.presence.name.clone(),
                color: e.presence.color.clone(),
                note: e.presence.object_id.clone(),
            })
            .collect();
        let open = route.any_note().map(|(_, n)| n.to_string());
        let cursors: Vec<RemoteCursor> = c
            .presence
            .values()
            .filter(|e| {
                Some(&e.vault) == v.as_ref()
                    && e.presence.object_id.is_some()
                    && e.presence.object_id == open
            })
            .filter_map(|e| {
                let (a, h) = e.presence.cursor?;
                Some(RemoteCursor {
                    name: e.presence.name.clone(),
                    color: e.presence.color.clone(),
                    anchor: a,
                    head: h,
                })
            })
            .collect();
        (views, cursors, c.editor.as_ref().map(|(_, _, e)| e.clone()))
    });
    ui.presence.set(views);
    if let Some(ed) = ed {
        ed.set_remote_cursors(cursors);
    }
}

pub fn start_presence_heartbeat() {
    util::set_interval(15_000, || {
        if ui().phase.get_untracked() == Phase::Ready {
            send_presence();
            publish_presence();
        }
    });
}

// ------------------------------------------------------------------ devices

pub async fn refresh_pending_devices() {
    if let Ok(devs) = api::get::<Vec<DeviceInfo>>("/devices").await {
        ui().pending_devices.set(
            devs.into_iter()
                .filter(|d| d.status == DeviceStatus::Pending)
                .collect(),
        );
    }
}

// ------------------------------------------------------------------ navigation

pub fn navigate(route: Route) {
    push_route(route, false);
}

pub fn navigate_replace(route: Route) {
    push_route(route, true);
}

fn push_route(route: Route, replace: bool) {
    let ui = ui();
    let path = route.path();
    if let Ok(h) = util::window().history() {
        let _ = if replace {
            h.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&path))
        } else {
            h.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&path))
        };
    }
    let uid = with(|c| c.user_id.clone());
    if (route.vault().is_some() || route.shared().is_some()) && !uid.is_empty() {
        storage::set_last_location(&uid, &path);
    }
    if let Route::Note(v, n) = &route {
        with(|c| {
            let r = (v.clone(), n.clone());
            c.prefs.recent.retain(|x| x != &r);
            c.prefs.recent.insert(0, r);
            c.prefs.recent.truncate(30);
        });
        save_prefs();
    }
    ui.sidebar.set(false);
    ui.route.set(route);
    send_presence();
    publish_presence();
}

pub fn listen_popstate() {
    use wasm_bindgen::JsCast;
    let cb = wasm_bindgen::closure::Closure::wrap(
        Box::new(|| ui().route.set(current_route())) as Box<dyn FnMut()>
    );
    let _ =
        util::window().add_event_listener_with_callback("popstate", cb.as_ref().unchecked_ref());
    cb.forget();
    let cb = wasm_bindgen::closure::Closure::wrap(Box::new(|e: web_sys::Event| {
        flush_now();
        if with(|c| c.pending_count()) > 0
            && with(|c| c.socket.as_ref().is_some_and(|s| !s.is_ready()))
        {
            // Unsent changes are kept in local storage; warn anyway.
            e.prevent_default();
            let _ = js_sys::Reflect::set(&e, &"returnValue".into(), &"".into());
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    let _ = util::window()
        .add_event_listener_with_callback("beforeunload", cb.as_ref().unchecked_ref());
    cb.forget();
}

// ------------------------------------------------------------------ helpers used by views

pub fn note_title(vault: &str, note: &str) -> Option<String> {
    with(|c| {
        c.replicas
            .get(vault)?
            .index
            .get(note)
            .map(|e| e.title().to_string())
    })
}

pub fn entry(vault: &str, id: &str) -> Option<coolpsonotes_core::doc::Entry> {
    with(|c| c.replicas.get(vault)?.index.get(id))
}

pub fn vault_name(vault: &str) -> String {
    with(|c| {
        c.replicas
            .get(vault)
            .map(|r| r.meta.name.clone())
            .unwrap_or_default()
    })
}

pub fn role(vault: &str) -> Option<Role> {
    with(|c| c.replicas.get(vault).map(|r| r.role))
}

pub fn can_write(vault: &str) -> bool {
    role(vault).is_some_and(|r| r.can_write())
}

pub fn is_owner(vault: &str) -> bool {
    role(vault) == Some(Role::Owner)
}

pub fn ensure_note_loaded(vault: &str, note: &str) {
    with(|c| {
        if let Some(r) = c.replicas.get_mut(vault) {
            r.notes.entry(note.to_string()).or_insert_with(NoteDoc::new);
        }
    });
}

pub fn live_entries(vault: &str) -> Vec<coolpsonotes_core::doc::Entry> {
    with(|c| {
        c.replicas
            .get(vault)
            .map(|r| {
                r.index
                    .resolved_paths()
                    .into_iter()
                    .map(|(p, mut e)| {
                        e.path = p;
                        e
                    })
                    .collect()
            })
            .unwrap_or_default()
    })
}

pub fn notes_of(vault: &str) -> Vec<(String, String)> {
    live_entries(vault)
        .into_iter()
        .filter(|e| e.kind == EntryKind::Note)
        .map(|e| (e.id, e.path))
        .collect()
}

pub fn new_id() -> String {
    random_id()
}

pub fn display_path(p: &str) -> String {
    paths::title(p).to_string()
}
