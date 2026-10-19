//! Browser storage.
//!
//! * Session token: `sessionStorage` (per tab) with a `localStorage` copy used
//!   to seed new tabs. Each tab can therefore be signed in as a different user,
//!   which makes multi-user testing in development mode easy.
//! * Device key: `localStorage`, keyed by user id. It never leaves the browser.
//!   Anyone with access to this browser profile can use it, as with any web
//!   app holding keys; rely on OS account security and disk encryption.
//! * Unsent updates: `localStorage`, already encrypted with the vault key.

use coolpsonotes_core::protocol::PushUpdate;
use serde::{Deserialize, Serialize};
use web_sys::Storage;

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

const TOKEN: &str = "cpn.token";
const LAST_TOKEN: &str = "cpn.token.last";

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
    pub fn account(&self) -> Option<coolpsonotes_core::keys::AccountPublic> {
        let b = coolpsonotes_core::b64::decode(self.account.as_deref()?).ok()?;
        if b.len() != 64 {
            return None;
        }
        Some(coolpsonotes_core::keys::AccountPublic {
            sign_public: b[..32].try_into().ok()?,
            enc_public: b[32..].try_into().ok()?,
        })
    }

    pub fn pin(&mut self, a: &coolpsonotes_core::keys::AccountPublic) {
        self.account = Some(coolpsonotes_core::b64::encode(
            &[a.sign_public, a.enc_public].concat(),
        ));
    }
}

fn device_key(user_id: &str) -> String {
    format!("cpn.device.{user_id}")
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

fn pending_key(user_id: &str, vault_id: &str) -> String {
    format!("cpn.pending.{user_id}.{vault_id}")
}

pub fn pending(user_id: &str, vault_id: &str) -> Vec<PushUpdate> {
    get(local(), &pending_key(user_id, vault_id))
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn set_pending(user_id: &str, vault_id: &str, p: &[PushUpdate]) -> Result<(), String> {
    if p.is_empty() {
        remove(local(), &pending_key(user_id, vault_id));
        Ok(())
    } else {
        try_set(
            local(),
            &pending_key(user_id, vault_id),
            &serde_json::to_string(p).unwrap(),
        )
    }
}

/// Every vault that has unsent updates stored for this user.
pub fn pending_vaults(user_id: &str) -> Vec<String> {
    let Some(s) = local() else { return vec![] };
    let prefix = format!("cpn.pending.{user_id}.");
    let n = s.length().unwrap_or(0);
    (0..n)
        .filter_map(|i| s.key(i).ok().flatten())
        .filter_map(|k| k.strip_prefix(&prefix).map(str::to_string))
        .collect()
}

/// Delete everything stored for a user in this browser: device key, unsent
/// changes, sync positions, last location (and the session token).
pub fn forget_user(user_id: &str) {
    clear_token();
    let Some(s) = local() else { return };
    let exact = [device_key(user_id), format!("cpn.last.{user_id}")];
    let prefixes = [
        format!("cpn.pending.{user_id}."),
        format!("cpn.head.{user_id}."),
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

fn head_key(user_id: &str, vault_id: &str) -> String {
    format!("cpn.head.{user_id}.{vault_id}")
}

pub fn head_seen(user_id: &str, vault_id: &str) -> i64 {
    get(local(), &head_key(user_id, vault_id))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

pub fn set_head_seen(user_id: &str, vault_id: &str, head: i64) {
    set(local(), &head_key(user_id, vault_id), &head.to_string());
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
    #[serde(default)]
    pub wide: bool,
}

fn default_mode() -> String {
    "live".into()
}

impl Default for UiPrefs {
    fn default() -> Self {
        UiPrefs {
            theme: "system".into(),
            editor_mode: "live".into(),
            vim: false,
            right_panel: true,
            wide: false,
        }
    }
}

pub fn ui_prefs() -> UiPrefs {
    get(local(), "cpn.ui")
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn set_ui_prefs(p: &UiPrefs) {
    set(local(), "cpn.ui", &serde_json::to_string(p).unwrap());
}

pub fn last_location(user_id: &str) -> Option<String> {
    get(local(), &format!("cpn.last.{user_id}"))
}

pub fn set_last_location(user_id: &str, path: &str) {
    set(local(), &format!("cpn.last.{user_id}"), path);
}
