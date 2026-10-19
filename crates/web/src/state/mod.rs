//! Application state.
//!
//! [`Core`] holds everything non-reactive (keys, CRDT replicas, the socket)
//! in a thread-local `RefCell`. [`Ui`] is a bundle of Leptos signals the views
//! render from. Rule: never call into the editor (or anything that may call
//! back into `with`) while holding the `Core` borrow.
//!
//! The rest is split by concern: `session` (sign-in, setup, devices),
//! `prefs`, `vaults`, `shares` (notes shared with us), `sync` (socket and
//! local edits), `presence` and `nav`; everything is re-exported here.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use coolpsonotes_core::crypto::Purpose;
use coolpsonotes_core::doc::{EntryKind, NoteDoc};
use coolpsonotes_core::engine::{self, ObjectMeta, Presence, VaultMeta, VaultReplica};
use coolpsonotes_core::keys::{AccountPublic, AccountSecret, DeviceKey, RecoveryKey};
use coolpsonotes_core::protocol::*;
use coolpsonotes_core::{paths, random_id};
use leptos::prelude::Update;
use leptos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::api;
use crate::editor::{Editor, RemoteCursor};
use crate::storage::{self, DeviceRecord, UiPrefs};
use crate::util::{self, now_ms};
use crate::ws::{ConnState, Socket};

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
    /// All quick notes (see `crate::quick`).
    Quick,
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
            Route::Quick => "/quick".into(),
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
            ["quick"] => Route::Quick,
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
    Promote { vault: String, id: String },
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
    /// Bumped by a click outside any menu (or Escape): open menus close.
    pub close_menus: RwSignal<u64>,
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
        close_menus: RwSignal::new(0),
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
    /// The vault quick notes go to (`None`: the default, see `crate::quick`).
    #[serde(default)]
    pub quick_vault: Option<String>,
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
    /// Upload bookkeeping per replica (`coolpsonotes_sync::Outbox`).
    pub outboxes: BTreeMap<String, coolpsonotes_sync::Outbox>,
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
    /// When we last told the user that local storage is full.
    pub storage_warned_at: i64,
    /// Keys of a first-device setup whose upload failed, reused on retry.
    pub setup: Option<engine::AccountBootstrap>,
    /// Started from this browser's offline data; the normal checks run when
    /// the server is reachable again (`offline::finish_online`).
    pub offline_start: bool,
}

impl Core {
    pub fn replica(&self, v: &str) -> Option<&VaultReplica> {
        self.replicas.get(v)
    }

    /// Number of documents with changes not yet confirmed by the server
    /// (queued, or held back as too large).
    pub fn pending_count(&self) -> usize {
        self.replicas
            .iter()
            .map(|(v, r)| {
                let held = self
                    .outboxes
                    .get(v)
                    .map(|o| o.held.as_slice())
                    .unwrap_or(&[]);
                r.pending
                    .iter()
                    .chain(held)
                    .map(|p| (v, &p.object_id))
                    .collect::<BTreeSet<_>>()
                    .len()
            })
            .sum()
    }
}

mod nav;
mod offline;
mod prefs;
mod presence;
mod session;
mod shares;
mod sync;
mod vaults;

pub use nav::*;
pub use offline::{
    disable as disable_offline, enable as enable_offline, enabled as offline_enabled,
    supported as offline_supported,
};
pub use prefs::*;
pub use presence::*;
pub use session::*;
pub use shares::*;
pub use sync::*;
pub use vaults::*;

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
