//! Signing in: login, first-device setup, device enrollment and recovery,
//! sign-out, "forget this browser", and entering the app.

use super::*;

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
            // The stored offline data holds this dead session's token.
            offline::signed_out(&with(|c| c.user_id.clone()));
            sign_out_local(Some("Your session ended. Please sign in again.".into()));
        }
    });
    match api::get::<ServerInfo>("/info").await {
        Ok(i) => ui.server.set(Some(i)),
        Err(e) => {
            if e.is_unreachable()
                && let Some(t) = storage::token()
                && let Some(b) = offline::for_token(&t)
            {
                api::set_token(Some(t));
                offline::start(b).await;
                return;
            }
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

pub(super) fn fragment_param(frag: &str, key: &str) -> Option<String> {
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

pub(super) async fn complete_oidc() {
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
            if e.is_unreachable()
                && let Some(b) = storage::token().and_then(|t| offline::for_token(&t))
            {
                offline::start(b).await;
                return;
            }
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
        c.me = Some(me.clone());
    });
    let server = ui.server.get_untracked();
    offline::note(|b| {
        b.me = Some(me);
        b.token = storage::token().unwrap_or_default();
        b.server = server;
    });
    if !has_key {
        ui.phase.set(Phase::Setup);
        return;
    }
    if let Some(rec) = storage::device(&uid) {
        match api::get::<DeviceEnvelope>(&format!("/devices/{}/envelope", rec.device_id)).await {
            Ok(env) => match unlock_with_device(&uid, &rec, &env.account_envelope) {
                Unlock::Unlocked => {
                    // A setup interrupted before its recovery key was confirmed.
                    if let Some(key) = unconfirmed_recovery_key(&uid).await {
                        ui.phase.set(Phase::ShowRecovery { key });
                        return;
                    }
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
            // Only the server's explicit answer: a 404 from a misrouted proxy
            // must never delete the device key.
            Err(e) if e.status == 404 && e.code == "device_revoked" => {
                offline::forget(&uid).await;
                storage::clear_device(&uid);
            }
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

/// The recovery key of an interrupted setup, if it really opens this account
/// (checked against the server's recovery envelope; a stale one is dropped).
pub(super) async fn unconfirmed_recovery_key(uid: &str) -> Option<String> {
    let key = storage::pending_recovery(uid)?;
    let rk = RecoveryKey::parse(&key).ok();
    let env = api::get::<RecoveryEnvelope>("/me/recovery").await.ok()?;
    let mine = with(|c| c.account.as_ref().map(|a| a.public()))?;
    let opens = rk
        .and_then(|rk| rk.unwrap_account(&env.recovery_envelope, uid).ok())
        .is_some_and(|a| a.public() == mine);
    if !opens {
        storage::clear_pending_recovery(uid);
    }
    opens.then_some(key)
}

pub(super) enum Unlock {
    Unlocked,
    /// Not for this device (or the record is damaged).
    Unreadable,
    /// A valid secret, but of another account key than this device was
    /// approved for: someone (possibly the server) substituted keys.
    WrongAccount,
}

pub(super) const WRONG_ACCOUNT: &str = "The server sent this browser keys of a different account than the one it was approved for. This can mean the server was tampered with. Nothing was decrypted with them; check your devices from another device before signing in here again.";

/// The account key the server publishes for us, checked.
pub(super) fn published_account(uid: &str) -> Option<AccountPublic> {
    with(|c| {
        let key = c.me.as_ref()?.account_key.as_ref()?;
        engine::published_account(uid, key).ok()
    })
}

pub(super) fn unlock_with_device(uid: &str, rec: &DeviceRecord, envelope: &[u8]) -> Unlock {
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
    offline::note(|b| {
        b.device_id = rec.device_id.clone();
        b.envelope = envelope.to_vec();
    });
    with(|c| {
        c.account = Some(acct);
        c.device_id = Some(rec.device_id.clone());
    });
    Unlock::Unlocked
}

pub(super) fn show_pending(uid: &str, mut rec: DeviceRecord) {
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

pub(super) fn poll_approval(uid: String, rec: DeviceRecord) {
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
                Err(e) if e.status == 404 && e.code == "device_revoked" => {
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
    // that only this device can unwrap: a lost answer must not lose it. The
    // recovery key too, until its screen is confirmed.
    let previous = storage::device(&uid);
    storage::set_pending_recovery(&uid, &boot.recovery.to_display());
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
                storage::clear_pending_recovery(&uid);
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
    storage::clear_pending_recovery(&with(|c| c.user_id.clone()));
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

pub(super) const WELCOME: &str = "# Welcome to coolpsonotes 👋\n\nThis is your private vault. Everything you write is **encrypted on this device** before it is synced — the server never sees your notes.\n\n## Getting started\n\n- [ ] Write something here — just start typing\n- [ ] Create a new note with the **+** button (or Ctrl+Alt+N)\n- [ ] Link notes by typing `[[` and a note name, like [[Ideas]]\n- [ ] Add #tags anywhere\n- [ ] Share a vault with family from the vault menu\n\nUse the toolbar for formatting, or Markdown if you know it: `**bold**`, `*italic*`, `# Heading`, `- list`.\n\nPress **Ctrl+P** (⌘P on a Mac) to jump to any note, and **Ctrl+/** for all keyboard shortcuts.\n";

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
    let uid = with(|c| c.user_id.clone());
    offline::signed_out(&uid);
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
    offline::forget(&uid).await;
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

pub(super) async fn enter_ready() {
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
    go_to_last_location();
    refresh_pending_devices().await;
    offline::ensure_worker().await;
}

/// On the home page after a start: go back to where this person last was.
pub(super) fn go_to_last_location() {
    let ui = ui();
    if !matches!(ui.route.get_untracked(), Route::Home) {
        return;
    }
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
