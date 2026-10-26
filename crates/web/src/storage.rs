//! Browser storage.
//!
//! * Session token: `sessionStorage` (per tab) with a `localStorage` copy used
//!   to seed new tabs. Each tab can therefore be signed in as a different user,
//!   which makes multi-user testing in development mode easy.
//! * Device key: `localStorage`, keyed by user id. It never leaves the browser.
//!   Anyone with access to this browser profile can use it, as with any web
//!   app holding keys; rely on OS account security and disk encryption.
//! * Unsent updates: `localStorage`, already encrypted (vault or note key),
//!   marked when some were saved without a connection.
//! * Keys of new notes not registered yet: `localStorage`, wrapped under the
//!   vault key.
//! * Only with "Keep this browser usable offline": what starting without the
//!   server needs (see `state::offline`), `localStorage`.

use notekpr_core::protocol::PushUpdate;
use notekpr_sync::NewKey;
use serde::{Deserialize, Serialize};
use web_sys::Storage;

use crate::width::NoteWidth;

fn local() -> Option<Storage> {
    web_sys::window()?.local_storage().ok()?
}

fn session() -> Option<Storage> {
    web_sys::window()?.session_storage().ok()?
}

fn get(s: Option<Storage>, k: &str) -> Option<String> {
    s?.get_item(k).ok()?
}

fn set(s: Option<Storage>, k: &str, v: &str) {
    let _ = try_set(s, k, v);
}

/// Like [`set`], but reports failures (storage full or unavailable).
fn try_set(s: Option<Storage>, k: &str, v: &str) -> Result<(), String> {
    let s = s.ok_or("local storage is not available")?;
    s.set_item(k, v).map_err(|e| {
        js_sys::Reflect::get(&e, &"name".into())
            .ok()
            .and_then(|n| n.as_string())
            .unwrap_or_else(|| "storage error".into())
    })
}

fn remove(s: Option<Storage>, k: &str) {
    if let Some(s) = s {
        let _ = s.remove_item(k);
    }
}

fn recovery_slot(user_id: &str) -> String {
    format!("notekpr.recovery.{user_id}")
}

/// The recovery key of a setup whose recovery screen wasn't confirmed yet, so
/// a reload or a lost server answer still shows it. This tab only
/// (`sessionStorage`), removed once confirmed.
pub fn pending_recovery(user_id: &str) -> Option<String> {
    get(session(), &recovery_slot(user_id))
}

pub fn set_pending_recovery(user_id: &str, key: &str) {
    set(session(), &recovery_slot(user_id), key);
}

pub fn clear_pending_recovery(user_id: &str) {
    remove(session(), &recovery_slot(user_id));
}

const TOKEN: &str = "notekpr.token";
const LAST_TOKEN: &str = "notekpr.token.last";

pub fn token() -> Option<String> {
    get(session(), TOKEN).or_else(|| {
        let t = get(local(), LAST_TOKEN)?;
        set(session(), TOKEN, &t);
        Some(t)
    })
}

pub fn set_token(t: &str) {
    set(session(), TOKEN, t);
    set(local(), LAST_TOKEN, t);
}

pub fn clear_token() {
    if let Some(t) = get(session(), TOKEN)
        && get(local(), LAST_TOKEN).as_deref() == Some(t.as_str())
    {
        remove(local(), LAST_TOKEN);
    }
    remove(session(), TOKEN);
}

#[derive(Serialize, Deserialize, Clone)]
pub struct DeviceRecord {
    pub device_id: String,
    pub secret: String,
    /// The account key this device was approved for (signing ‖ encryption
    /// public key, base64), pinned when its verification code was shown. An
    /// envelope must unwrap to a secret of exactly this account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

impl DeviceRecord {
    pub fn account(&self) -> Option<notekpr_core::keys::AccountPublic> {
        let b = notekpr_core::b64::decode(self.account.as_deref()?).ok()?;
        if b.len() != 64 {
            return None;
        }
        Some(notekpr_core::keys::AccountPublic {
            sign_public: b[..32].try_into().ok()?,
            enc_public: b[32..].try_into().ok()?,
        })
    }

    pub fn pin(&mut self, a: &notekpr_core::keys::AccountPublic) {
        self.account = Some(notekpr_core::b64::encode(
            &[a.sign_public, a.enc_public].concat(),
        ));
    }
}

fn device_key(user_id: &str) -> String {
    format!("notekpr.device.{user_id}")
}

pub fn device(user_id: &str) -> Option<DeviceRecord> {
    serde_json::from_str(&get(local(), &device_key(user_id))?).ok()
}

pub fn set_device(user_id: &str, rec: &DeviceRecord) {
    set(
        local(),
        &device_key(user_id),
        &serde_json::to_string(rec).unwrap(),
    );
}

pub fn clear_device(user_id: &str) {
    remove(local(), &device_key(user_id));
}

thread_local! {
    /// This page load: unsent changes are stored per page, so two tabs never
    /// overwrite each other's (each later load takes over what is left).
    static PAGE: String = notekpr_core::random_id();
}

fn pending_key(user_id: &str, vault_id: &str) -> String {
    PAGE.with(|p| format!("notekpr.pending.{user_id}.{vault_id}.{p}"))
}

/// A page's unsent updates of a vault, as stored.
#[derive(Deserialize, Default)]
pub struct StoredPending {
    pub updates: Vec<PushUpdate>,
    /// Some were saved while the page had no connection: they wait for it
    /// rather than being on their way (a page taking them over says so).
    #[serde(default)]
    pub offline: bool,
}

#[derive(Serialize)]
struct StoringPending<'a> {
    updates: &'a [PushUpdate],
    offline: bool,
}

fn read_pending(key: &str) -> Option<StoredPending> {
    let s = get(local(), key)?;
    serde_json::from_str(&s).ok().or_else(|| {
        // Older versions stored the updates alone (and only said "offline").
        let updates = serde_json::from_str(&s).ok()?;
        Some(StoredPending {
            updates,
            offline: true,
        })
    })
}

/// This page's unsent updates of a vault.
pub fn pending(user_id: &str, vault_id: &str) -> StoredPending {
    read_pending(&pending_key(user_id, vault_id)).unwrap_or_default()
}

/// Unsent updates of a vault stored by any page (earlier loads, other tabs,
/// and the single key of older versions), by storage key.
pub fn all_pending(user_id: &str, vault_id: &str) -> Vec<(String, StoredPending)> {
    let Some(s) = local() else { return vec![] };
    let legacy = format!("notekpr.pending.{user_id}.{vault_id}");
    let prefix = format!("{legacy}.");
    let n = s.length().unwrap_or(0);
    let keys: Vec<String> = (0..n)
        .filter_map(|i| s.key(i).ok().flatten())
        .filter(|k| *k == legacy || k.starts_with(&prefix))
        .collect();
    keys.into_iter()
        .filter_map(|k| Some((k.clone(), read_pending(&k)?)))
        .collect()
}

/// Whether `key` is this page's own storage key for the vault.
pub fn is_own_pending_key(user_id: &str, vault_id: &str, key: &str) -> bool {
    key == pending_key(user_id, vault_id)
}

pub fn remove_pending_key(key: &str) {
    if key.starts_with("notekpr.pending.") {
        remove(local(), key);
    }
}

/// Store this page's unsent updates of a vault; `offline`: some were saved
/// without a connection.
pub fn set_pending(
    user_id: &str,
    vault_id: &str,
    p: &[PushUpdate],
    offline: bool,
) -> Result<(), String> {
    if p.is_empty() {
        remove(local(), &pending_key(user_id, vault_id));
        Ok(())
    } else {
        let stored = StoringPending {
            updates: p,
            offline,
        };
        try_set(
            local(),
            &pending_key(user_id, vault_id),
            &serde_json::to_string(&stored).unwrap(),
        )
    }
}

fn new_keys_key(user_id: &str, vault_id: &str) -> String {
    PAGE.with(|p| format!("notekpr.newkeys.{user_id}.{vault_id}.{p}"))
}

/// Keys of new notes not registered with the server yet, stored by any page
/// (like [`all_pending`]: per page, so tabs never overwrite each other's).
/// Each is wrapped under the vault key.
pub fn all_new_keys(user_id: &str, vault_id: &str) -> Vec<(String, Vec<NewKey>)> {
    let Some(s) = local() else { return vec![] };
    let prefix = format!("notekpr.newkeys.{user_id}.{vault_id}.");
    let n = s.length().unwrap_or(0);
    let keys: Vec<String> = (0..n)
        .filter_map(|i| s.key(i).ok().flatten())
        .filter(|k| k.starts_with(&prefix))
        .collect();
    keys.into_iter()
        .filter_map(|k| {
            let v = serde_json::from_str(&get(local(), &k)?).ok()?;
            Some((k, v))
        })
        .collect()
}

/// Store this page's unregistered note keys of a vault; other pages' entries
/// it has taken over (`adopted`) are removed once it is stored.
pub fn set_new_keys(user_id: &str, vault_id: &str, keys: &[NewKey], adopted: &[String]) {
    let own = new_keys_key(user_id, vault_id);
    let stored = if keys.is_empty() {
        remove(local(), &own);
        true
    } else {
        try_set(local(), &own, &serde_json::to_string(keys).unwrap()).is_ok()
    };
    if stored {
        for k in adopted {
            if *k != own && k.starts_with("notekpr.newkeys.") {
                remove(local(), k);
            }
        }
    }
}

/// Delete everything stored for a user in this browser: device key, unsent
/// changes, sync positions, last location (and the session token).
pub fn forget_user(user_id: &str) {
    clear_token();
    clear_pending_recovery(user_id);
    let Some(s) = local() else { return };
    let exact = [
        device_key(user_id),
        prefs_copy_key(user_id),
        format!("notekpr.last.{user_id}"),
        offline_flag(user_id),
        offline_data_key(user_id),
    ];
    let prefixes = [
        format!("notekpr.pending.{user_id}."),
        format!("notekpr.newkeys.{user_id}."),
        format!("notekpr.logseen.{user_id}."),
        format!("notekpr.head.{user_id}."),
    ];
    let n = s.length().unwrap_or(0);
    let keys: Vec<String> = (0..n)
        .filter_map(|i| s.key(i).ok().flatten())
        .filter(|k| exact.contains(k) || prefixes.iter().any(|p| k.starts_with(p.as_str())))
        .collect();
    for k in keys {
        let _ = s.remove_item(&k);
    }
}

fn offline_flag(user_id: &str) -> String {
    format!("notekpr.offline.on.{user_id}")
}

fn offline_data_key(user_id: &str) -> String {
    format!("notekpr.offline.data.{user_id}")
}

/// Whether the person turned on "Keep this browser usable offline" here.
pub fn offline_enabled(user_id: &str) -> bool {
    get(local(), &offline_flag(user_id)).is_some()
}

pub fn set_offline_enabled(user_id: &str, on: bool) {
    if on {
        set(local(), &offline_flag(user_id), "1");
    } else {
        remove(local(), &offline_flag(user_id));
    }
}

pub fn offline_data(user_id: &str) -> Option<String> {
    get(local(), &offline_data_key(user_id))
}

pub fn set_offline_data(user_id: &str, json: &str) -> Result<(), String> {
    try_set(local(), &offline_data_key(user_id), json)
}

pub fn remove_offline_data(user_id: &str) {
    remove(local(), &offline_data_key(user_id));
}

/// The stored offline data of every account that has some in this browser.
pub fn all_offline_data() -> Vec<String> {
    let Some(s) = local() else { return vec![] };
    let n = s.length().unwrap_or(0);
    let keys: Vec<String> = (0..n)
        .filter_map(|i| s.key(i).ok().flatten())
        .filter(|k| k.starts_with("notekpr.offline.data."))
        .collect();
    keys.iter().filter_map(|k| get(local(), k)).collect()
}

fn head_key(user_id: &str, vault_id: &str) -> String {
    format!("notekpr.head.{user_id}.{vault_id}")
}

pub fn head_seen(user_id: &str, vault_id: &str) -> i64 {
    get(local(), &head_key(user_id, vault_id))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

pub fn set_head_seen(user_id: &str, vault_id: &str, head: i64) {
    set(local(), &head_key(user_id, vault_id), &head.to_string());
}

fn prefs_copy_key(user_id: &str) -> String {
    format!("notekpr.prefs.{user_id}")
}

/// This browser's copy of the user's encrypted preferences (as sealed with
/// the account key), so an older copy from the server can't replace them.
pub fn prefs_copy(user_id: &str) -> Option<Vec<u8>> {
    notekpr_core::b64::decode(&get(local(), &prefs_copy_key(user_id))?).ok()
}

pub fn set_prefs_copy(user_id: &str, ciphertext: &[u8]) {
    set(
        local(),
        &prefs_copy_key(user_id),
        &notekpr_core::b64::encode(ciphertext),
    );
}

fn log_seen_key(user_id: &str, vault_id: &str) -> String {
    format!("notekpr.logseen.{user_id}.{vault_id}")
}

/// The last membership log entry verified in this browser: `(seq, hash)`.
pub fn log_seen(user_id: &str, vault_id: &str) -> Option<(u64, Vec<u8>)> {
    let s = get(local(), &log_seen_key(user_id, vault_id))?;
    let (seq, hash) = s.split_once(':')?;
    Some((seq.parse().ok()?, notekpr_core::b64::decode(hash).ok()?))
}

pub fn set_log_seen(user_id: &str, vault_id: &str, seen: Option<(u64, Vec<u8>)>) {
    if let Some((seq, hash)) = seen {
        let v = format!("{seq}:{}", notekpr_core::b64::encode(&hash));
        set(local(), &log_seen_key(user_id, vault_id), &v);
    }
}

/// Per-browser UI preferences (not secret, not synced).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct UiPrefs {
    #[serde(default)]
    pub theme: String,
    #[serde(default = "default_mode")]
    pub editor_mode: String,
    #[serde(default)]
    pub vim: bool,
    #[serde(default)]
    pub right_panel: bool,
    /// How wide notes are, unless one has its own width or the default is
    /// the same on all devices ([`crate::width`]).
    #[serde(default)]
    pub note_width: NoteWidth,
    /// The "Quick notes" section of the sidebar.
    #[serde(default = "yes")]
    pub quick_notes: bool,
    /// Line numbers in code blocks (a block's `ln=false` turns them off too).
    #[serde(default = "yes")]
    pub code_numbers: bool,
    /// Indentation guides in code blocks.
    #[serde(default = "yes")]
    pub code_guides: bool,
    /// Long lines in code blocks wrap (a block's `wrap=` decides for itself).
    #[serde(default = "yes")]
    pub code_wrap: bool,
    /// Show a system notification when someone mentions us while the app
    /// isn't in front (this browser only: the permission is the browser's).
    #[serde(default)]
    pub mention_notifications: bool,
}

fn default_mode() -> String {
    "live".into()
}

fn yes() -> bool {
    true
}

impl Default for UiPrefs {
    fn default() -> Self {
        UiPrefs {
            theme: "system".into(),
            editor_mode: "live".into(),
            vim: false,
            right_panel: true,
            note_width: NoteWidth::NARROW,
            quick_notes: true,
            code_numbers: true,
            code_guides: true,
            code_wrap: true,
            mention_notifications: false,
        }
    }
}

pub fn ui_prefs() -> UiPrefs {
    get(local(), "notekpr.ui")
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn set_ui_prefs(p: &UiPrefs) {
    set(local(), "notekpr.ui", &serde_json::to_string(p).unwrap());
}

pub fn last_location(user_id: &str) -> Option<String> {
    get(local(), &format!("notekpr.last.{user_id}"))
}

pub fn set_last_location(user_id: &str, path: &str) {
    set(local(), &format!("notekpr.last.{user_id}"), path);
}
