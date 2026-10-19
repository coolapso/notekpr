//! "Keep this browser usable offline" (opt-in, per browser and account).
//!
//! Normally the browser asks the server for this device's wrapped account key
//! and the vault keys on every start, so without the server it can't decrypt
//! anything — and removing the device from the account cuts it off. With this
//! setting on, the browser keeps what a start needs (session token, account
//! info, this device's wrapped account key, the vault list with the wrapped
//! vault keys, the encrypted preferences) and a service worker keeps the
//! app's own files, so it opens and edits from its local copy (`cache.rs`)
//! when the server can't be reached. The cost: a copy of this browser's data
//! can then be decrypted without the server, even after the device was
//! removed. Turning it off (or "Forget this browser") deletes all of it.
//!
//! When the server is back, the normal checks run before anything else
//! ([`finish_online`]): the session, that this device still belongs to the
//! account, then fresh keys and lists.

use super::*;
use wasm_bindgen::JsCast;

/// What starting without the server needs. Everything sensitive in it is
/// already encrypted (to this device's key, the account key or vault keys).
#[derive(Serialize, Deserialize, Clone, Default)]
pub(super) struct Bundle {
    pub(super) token: String,
    pub(super) server: Option<ServerInfo>,
    pub(super) me: Option<MeResponse>,
    pub(super) device_id: String,
    #[serde(with = "coolpsonotes_core::b64")]
    pub(super) envelope: Vec<u8>,
    pub(super) vaults: Vec<VaultInfo>,
    pub(super) keys: BTreeMap<String, VaultKeysResponse>,
    pub(super) prefs: Option<PrefsBlob>,
}

const WORKER: &str = "/sw.js";
const APP_CACHE: &str = "coolpsonotes-app";

/// Whether the signed-in person turned the setting on in this browser.
pub fn enabled() -> bool {
    let uid = with(|c| c.user_id.clone());
    !uid.is_empty() && storage::offline_enabled(&uid)
}

/// Whether this browser can do it at all (service workers need HTTPS or localhost).
pub fn supported() -> bool {
    let w = util::window();
    w.is_secure_context()
        && js_sys::Reflect::has(&w.navigator(), &"serviceWorker".into()).unwrap_or(false)
}

/// Update the stored data, if the setting is on.
pub(super) fn note(f: impl FnOnce(&mut Bundle)) {
    let uid = with(|c| c.user_id.clone());
    if uid.is_empty() || !storage::offline_enabled(&uid) {
        return;
    }
    let mut b = load(&uid).unwrap_or_default();
    f(&mut b);
    save(&uid, &b);
}

fn load(uid: &str) -> Option<Bundle> {
    serde_json::from_str(&storage::offline_data(uid)?).ok()
}

fn save(uid: &str, b: &Bundle) {
    if let Ok(json) = serde_json::to_string(b)
        && let Err(e) = storage::set_offline_data(uid, &json)
    {
        util::warn(&format!("could not store the offline data: {e}"));
    }
}

/// The stored data of the account signed in with `token`, if its setting is on.
pub(super) fn for_token(token: &str) -> Option<Bundle> {
    storage::all_offline_data()
        .iter()
        .filter_map(|s| serde_json::from_str::<Bundle>(s).ok())
        .find(|b| {
            b.token == token
                && b.me
                    .as_ref()
                    .is_some_and(|m| storage::offline_enabled(&m.user.id))
        })
}

/// Turn the setting on: fetch and store everything a start needs now, and
/// install the service worker.
pub async fn enable() -> Result<(), String> {
    if !supported() {
        return Err("This needs the app to be served over HTTPS.".into());
    }
    let (uid, device) = with(|c| (c.user_id.clone(), c.device_id.clone()));
    let device = device.ok_or("This browser is not set up as a device yet.")?;
    let token = storage::token().ok_or("Not signed in.")?;
    let me: MeResponse = api::get("/me").await.map_err(|e| e.message)?;
    let env: DeviceEnvelope = api::get(&format!("/devices/{device}/envelope"))
        .await
        .map_err(|e| e.message)?;
    let vaults: Vec<VaultInfo> = api::get("/vaults").await.map_err(|e| e.message)?;
    let fetched = futures::future::join_all(vaults.iter().map(|v| {
        let id = v.id.clone();
        async move {
            let k = api::get::<VaultKeysResponse>(&format!("/vaults/{id}/keys")).await;
            (id, k)
        }
    }))
    .await;
    let mut keys = BTreeMap::new();
    for (id, k) in fetched {
        keys.insert(id, k.map_err(|e| e.message)?);
    }
    let prefs = api::get::<PrefsBlob>("/me/prefs").await.ok();
    let b = Bundle {
        token,
        server: ui().server.get_untracked(),
        me: Some(me),
        device_id: device,
        envelope: env.account_envelope,
        vaults,
        keys,
        prefs,
    };
    storage::set_offline_enabled(&uid, true);
    save(&uid, &b);
    register_worker().await
}

/// Turn the setting off: delete the stored data and the app copy.
pub async fn disable() {
    let uid = with(|c| c.user_id.clone());
    forget(&uid).await;
}

/// Delete everything this setting stored for `uid` in this browser.
pub(super) async fn forget(uid: &str) {
    storage::set_offline_enabled(uid, false);
    storage::remove_offline_data(uid);
    // The app copy is shared by every account here; keep it while another
    // one still uses it.
    if storage::all_offline_data().is_empty() {
        unregister_worker().await;
    }
}

/// Sign-out: the session token in the data is dead now. The setting stays;
/// the next start with the server stores fresh data.
pub(super) fn signed_out(uid: &str) {
    storage::remove_offline_data(uid);
}

/// (Re)install the service worker if the setting is on (a browser may drop it).
pub(super) async fn ensure_worker() {
    if enabled()
        && supported()
        && let Err(e) = register_worker().await
    {
        util::warn(&format!("could not install the offline app copy: {e}"));
    }
}

async fn register_worker() -> Result<(), String> {
    let sw = util::window().navigator().service_worker();
    wasm_bindgen_futures::JsFuture::from(sw.register(WORKER))
        .await
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

async fn unregister_worker() {
    if !supported() {
        return;
    }
    let sw = util::window().navigator().service_worker();
    if let Ok(list) = wasm_bindgen_futures::JsFuture::from(sw.get_registrations()).await {
        for r in js_sys::Array::from(&list).iter() {
            if let Ok(r) = r.dyn_into::<web_sys::ServiceWorkerRegistration>()
                && let Ok(p) = r.unregister()
            {
                let _ = wasm_bindgen_futures::JsFuture::from(p).await;
            }
        }
    }
    if let Ok(caches) = util::window().caches() {
        let _ = wasm_bindgen_futures::JsFuture::from(caches.delete(APP_CACHE)).await;
    }
}

/// Start from the stored data because the server can't be reached.
pub(super) async fn start(b: Bundle) {
    let ui = ui();
    let fail = |m: &str| ui.phase.set(Phase::Fatal(m.to_string()));
    let Some(me) = b.me.clone() else {
        return fail("Cannot reach the server.");
    };
    let uid = me.user.id.clone();
    ui.server.set(b.server.clone());
    ui.user.set(Some(me.user.clone()));
    with(|c| {
        c.user_id = uid.clone();
        c.me = Some(me);
        c.offline_start = true;
    });
    let Some(rec) = storage::device(&uid).filter(|r| r.device_id == b.device_id) else {
        return fail("Cannot reach the server, and this browser has no usable offline keys.");
    };
    match unlock_with_device(&uid, &rec, &b.envelope) {
        Unlock::Unlocked => {}
        Unlock::WrongAccount => return fail(WRONG_ACCOUNT),
        Unlock::Unreadable => {
            return fail(
                "Cannot reach the server, and this browser's offline keys can't be opened.",
            );
        }
    }
    if let Some(p) = &b.prefs {
        apply_prefs(p);
    }
    let loads = b.vaults.iter().filter_map(|info| {
        let keys = b.keys.get(&info.id)?.clone();
        Some(load_vault_with(info.clone(), false, Some(keys)))
    });
    futures::future::join_all(loads).await;
    publish_vaults();
    // Keeps trying; [`finish_online`] runs once it is through.
    connect_socket();
    ui.phase.set(Phase::Ready);
    go_to_last_location();
    info(
        "The server can't be reached: working from this browser's copy. Changes are kept here and sync when it is back.",
    );
}

/// The server is reachable again after an offline start: check the session
/// and this device like a normal start, then refresh everything.
pub(super) async fn finish_online() {
    let me = match api::get::<MeResponse>("/me").await {
        Ok(m) => m,
        // 401 signs out (see `boot`); anything else: try at the next connection.
        Err(_) => {
            with(|c| c.offline_start = true);
            return;
        }
    };
    let uid = me.user.id.clone();
    ui().user.set(Some(me.user.clone()));
    with(|c| c.me = Some(me.clone()));
    note(|b| b.me = Some(me));
    let Some(rec) = storage::device(&uid) else {
        return;
    };
    match api::get::<DeviceEnvelope>(&format!("/devices/{}/envelope", rec.device_id)).await {
        Ok(env) => match unlock_with_device(&uid, &rec, &env.account_envelope) {
            Unlock::Unlocked => {}
            Unlock::WrongAccount => {
                ui().phase.set(Phase::Fatal(WRONG_ACCOUNT.into()));
                return;
            }
            Unlock::Unreadable => {
                with(|c| c.offline_start = true);
                return;
            }
        },
        Err(e) if e.status == 404 && e.code == "device_revoked" => {
            // Removed from the account while we were away: forget its keys.
            forget(&uid).await;
            storage::clear_device(&uid);
            sign_out_local(Some(
                "This browser was removed from your account. Sign in again to use it.".into(),
            ));
            return;
        }
        Err(_) => {
            with(|c| c.offline_start = true);
            return;
        }
    }
    let _ = api::put::<_, ()>(
        "/me/session/device",
        &BindDeviceRequest {
            device_id: rec.device_id.clone(),
        },
    )
    .await;
    load_prefs().await;
    refresh_vaults().await;
    refresh_shares().await;
    refresh_pending_devices().await;
    success("Connected again.");
}
