//! Signing in and device enrollment for people, and identity setup for agents.
//!
//! * Authentication: RFC 8628 device authorization grant (approved in the web
//!   app after an OIDC login), or `POST /auth/dev-login` against a server in
//!   development mode.
//! * First device of an account: generate the account key, recovery key and
//!   device key locally (`engine::bootstrap_account`); only public keys and
//!   ciphertexts are uploaded.
//! * Further devices: register a pending device key, show the verification
//!   code, and wait until an existing device approves it — or unwrap the
//!   account secret with the recovery key and approve this device itself.
//! * Agents: OAuth client credentials, plus their own account key
//!   (`engine::bootstrap_agent`) whose fingerprint the owner compares before
//!   sharing a vault.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use coolpsonotes_core::engine;
use coolpsonotes_core::keys::{AccountSecret, DeviceKey, RecoveryKey};
use coolpsonotes_core::protocol::*;

use crate::api::{Api, Credentials, DevicePoll};
use crate::fsutil;
use crate::profile::{AgentCredentials, Dirs, Profile, ProfileStore, normalize_server_url};
use crate::secrets::SecretStore;
use crate::ui;

#[derive(Debug, Clone, Default)]
pub struct LoginOptions {
    pub server: String,
    pub insecure_http: bool,
    /// Development-mode login as this user name (no OIDC).
    pub dev_user: Option<String>,
    pub device_name: Option<String>,
    /// Unlock with the recovery key instead of waiting for approval.
    pub recovery_key: Option<String>,
    /// Offer to type the recovery key while waiting for approval.
    pub interactive: bool,
    /// How long to wait for another device to approve this one.
    pub approval_timeout: Option<Duration>,
}

#[derive(Debug, Clone)]
pub struct LoginOutcome {
    pub user: UserInfo,
    pub device_id: String,
    pub fingerprint: String,
    /// Set when this login created the account keys: shown exactly once.
    pub recovery_key: Option<String>,
    /// This device was already enrolled; only the session was renewed.
    pub reused_device: bool,
    pub recovered: bool,
}

/// A first-device setup: kept (sealed) from before the upload until the
/// recovery key was shown, so neither can be lost to a failure in between.
#[derive(serde::Serialize, serde::Deserialize)]
struct PendingBootstrap {
    device_id: String,
    request: AccountSetupRequest,
    recovery_key: String,
}

fn pending_bootstrap(secrets: &SecretStore, user_id: &str) -> Result<Option<PendingBootstrap>> {
    match secrets.load_bootstrap(user_id)? {
        Some(b) => Ok(Some(
            serde_json::from_slice(&b).context("reading the pending account setup")?,
        )),
        None => Ok(None),
    }
}

/// Call once the recovery key of a new account (`LoginOutcome::recovery_key`)
/// was shown: until then, every `login` shows it again.
pub fn recovery_key_shown(dirs: &Dirs, profile: &str) -> Result<()> {
    let ps = ProfileStore::new(dirs, profile)?;
    SecretStore::open(&ps.data_dir())?.forget_bootstrap()
}

pub fn host_name() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is valid for its length; gethostname NUL-terminates
        // on success (and we bound the read by the buffer length anyway).
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
        if rc == 0 {
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            if let Ok(s) = std::str::from_utf8(&buf[..end])
                && !s.is_empty()
            {
                return s.to_string();
            }
        }
    }
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "this computer".into())
}

fn default_device_name() -> String {
    format!("coolpsonotes CLI on {}", host_name())
}

async fn device_flow(api: &Api, client_name: &str) -> Result<TokenResponse> {
    let da = api
        .device_authorization(client_name)
        .await
        .context("starting the device login")?;
    ui::status(&format!(
        "\nTo sign in, open this address in a browser where you are signed in to coolpsonotes:\n\n    {}\n\n\
         and check that it shows the code  {}\n",
        da.verification_uri_complete, da.user_code
    ));
    let mut interval = Duration::from_secs(da.interval.max(1));
    let deadline = Instant::now() + Duration::from_secs(da.expires_in.max(30));
    loop {
        tokio::time::sleep(interval).await;
        if Instant::now() > deadline {
            bail!("the login code expired; run `coolpsonotes login` again");
        }
        match api.poll_device_token(&da.device_code).await {
            Ok(DevicePoll::Pending) => {}
            Ok(DevicePoll::SlowDown) => interval += Duration::from_secs(5),
            Ok(DevicePoll::Denied) => bail!("the login was declined in the browser"),
            Ok(DevicePoll::Expired) => {
                bail!("the login code expired; run `coolpsonotes login` again")
            }
            Ok(DevicePoll::Token(t)) => return Ok(t),
            Err(e) if e.is_network() => {}
            Err(e) => return Err(anyhow!(e).context("waiting for the login")),
        }
    }
}

/// Sign in a person and make sure this device holds the account secret.
pub async fn login(dirs: &Dirs, profile: &str, opts: LoginOptions) -> Result<LoginOutcome> {
    let server = normalize_server_url(&opts.server, opts.insecure_http)?;
    let ps = ProfileStore::new(dirs, profile)?;
    let existing = ps.load()?;
    if let Some(p) = &existing {
        if p.kind == PrincipalKind::Agent {
            bail!("profile '{profile}' is an agent profile; use another --profile for a person");
        }
        if p.server != server && p.user_id.is_some() {
            bail!(
                "profile '{profile}' belongs to {}; use another --profile, or `logout --forget` first",
                p.server
            );
        }
    }
    let anon = Api::new(&server, Credentials::None)?;
    let info = anon
        .info()
        .await
        .with_context(|| format!("contacting {server}"))?;
    let device_name = opts.device_name.clone().unwrap_or_else(default_device_name);

    // 1. Authenticate.
    let (token, expires_at, user) = match &opts.dev_user {
        Some(name) => {
            if !info.dev_mode {
                bail!(
                    "{server} is not running in development mode; sign in without --dev-user (OIDC device login)"
                );
            }
            let s = anon.dev_login(name).await.context("development login")?;
            (s.token, s.expires_at, s.user)
        }
        None => {
            if !info.oidc_enabled && info.dev_mode {
                ui::warn(
                    "this server has no OIDC provider configured; in development mode use --dev-user NAME",
                );
            }
            let t = device_flow(&anon, &device_name).await?;
            let api = Api::new(&server, Credentials::Session(t.access_token.clone()))?;
            let me = api.me().await?;
            (
                t.access_token,
                coolpsonotes_core::now_ms() + t.expires_in as i64 * 1000,
                me.user,
            )
        }
    };
    if user.kind != PrincipalKind::Human {
        bail!("these credentials belong to an agent; use `coolpsonotes agent setup`");
    }
    if let Some(uid) = existing.as_ref().and_then(|p| p.user_id.as_ref())
        && *uid != user.id
    {
        bail!(
            "profile '{profile}' belongs to a different account; use another --profile (e.g. --profile {})",
            user.display_name
                .to_lowercase()
                .replace(|c: char| !c.is_ascii_alphanumeric(), "-")
        );
    }
    // Read-modify-write (the authentication above can take minutes; a
    // running daemon may have changed the profile meanwhile).
    let insecure = opts.insecure_http;
    let mut p = ps.upsert(
        || Profile::new(&server, PrincipalKind::Human, insecure),
        |p| {
            p.server = server.clone();
            p.insecure_http |= insecure;
            p.session_token = Some(token.clone());
            p.session_expires_at = Some(expires_at);
            p.user_id = Some(user.id.clone());
            p.display_name = Some(user.display_name.clone());
            p.clone()
        },
    )?;

    let api = Api::new(&server, Credentials::Session(token))?;
    let secrets = SecretStore::open(&ps.data_dir())?;
    let me = api.me().await?;

    // 2. Already enrolled on this device: renew the session only.
    if let Some(account) = secrets.load_account(&user.id)? {
        let pending = pending_bootstrap(&secrets, &user.id)?;
        let key = match (&me.account_key, &pending) {
            (Some(k), _) => k.clone(),
            // A first-device setup whose upload did not finish (network,
            // crash): upload the same keys again.
            (None, Some(pb)) => {
                api.put::<_, ()>("/me/account-key", &pb.request)
                    .await
                    .context("uploading the new account key")?;
                pb.request.key.clone()
            }
            (None, None) => bail!(
                "the server has no account key for you but this device has one; the account may have been reset"
            ),
        };
        // Not shown yet (the process ended before): show it now.
        let recovery_key = pending.map(|p| p.recovery_key);
        if key.verify()? != account.public() {
            bail!(
                "the account key on the server differs from the one on this device. This can mean the server was \
                 compromised or the account was reset; refusing to continue"
            );
        }
        if let Some(dev) = p.device_id.clone().filter(|_| !p.device_pending) {
            match api
                .put::<_, ()>(
                    "/me/session/device",
                    &BindDeviceRequest {
                        device_id: dev.clone(),
                    },
                )
                .await
            {
                Ok(()) => {
                    return Ok(LoginOutcome {
                        user,
                        device_id: dev,
                        fingerprint: account.public().fingerprint(),
                        reused_device: recovery_key.is_none(),
                        recovery_key,
                        recovered: false,
                    });
                }
                // The server says `device_revoked` for a device that is unknown
                // or was revoked (older servers: a plain 404). A 404 can also
                // come from a misconfigured proxy, so a plain one must be
                // confirmed by the device list before anything changes.
                Err(e)
                    if e.is_device_revoked()
                        || (e.is_not_found() && device_was_removed(&api, &dev).await?) =>
                {
                    ui::warn(&format!(
                        "this device ({dev}) was removed from your account (with `devices revoke` or in the web \
                         app). It must be approved again before it can decrypt anything new"
                    ));
                    if opts.interactive
                        && ui::stdin_is_tty()
                        && !ui::confirm("Register this computer as a new device now?")
                    {
                        bail!("cancelled; nothing was changed");
                    }
                    // Respect the removal, but keep the old (sealed) secrets.
                    if let Some(aside) = secrets.set_aside_account()? {
                        ui::status(&format!(
                            "the old device keys were moved to {} (not deleted)",
                            aside.display()
                        ));
                    }
                    ps.update(|p| {
                        p.device_id = None;
                        p.device_pending = false;
                    })?;
                    p.device_id = None;
                    p.device_pending = false;
                }
                Err(e) => return Err(anyhow!(e).context("binding the session to this device")),
            }
        }
    }

    // 3. First device of the account.
    let Some(key) = me.account_key.clone() else {
        let b = engine::bootstrap_account(&user.id, &device_name)?;
        // Everything is kept locally *before* the upload: if the upload or
        // this process fails, the next login uploads the same keys and shows
        // the recovery key then. (Uploading first could leave an account
        // whose only secret was never saved anywhere.)
        let pending = PendingBootstrap {
            device_id: b.device_id.clone(),
            request: b.request.clone(),
            recovery_key: b.recovery.to_display(),
        };
        secrets.save_bootstrap(&user.id, &serde_json::to_vec(&pending)?)?;
        secrets.save_device(&user.id, &b.device_id, &b.device)?;
        secrets.save_account(&user.id, &b.account)?;
        ps.update(|p| {
            p.device_id = Some(b.device_id.clone());
            p.device_name = Some(device_name.clone());
            p.device_pending = false;
        })?;
        api.put::<_, ()>("/me/account-key", &b.request)
            .await
            .context("uploading the new account key (run `coolpsonotes login` again to retry)")?;
        return Ok(LoginOutcome {
            user,
            device_id: b.device_id,
            fingerprint: b.account.public().fingerprint(),
            recovery_key: Some(pending.recovery_key),
            reused_device: false,
            recovered: false,
        });
    };
    if key.user_id != user.id {
        bail!("the server returned an account key for a different user");
    }
    let public = key
        .verify()
        .map_err(|e| anyhow!("your account key on the server failed verification: {e}"))?;

    // 4. Additional device: resume a pending registration or start one.
    let devices: Vec<DeviceInfo> = api.get("/devices").await?;
    let resumed = match (p.device_pending, p.device_id.clone()) {
        (true, Some(id)) => match secrets.load_device(&user.id, &id)? {
            Some(dev)
                if devices
                    .iter()
                    .any(|d| d.id == id && d.status == DeviceStatus::Pending) =>
            {
                Some((dev, id))
            }
            _ => None,
        },
        _ => None,
    };
    let (device, device_id) = match resumed {
        Some(x) => {
            api.put::<_, ()>(
                "/me/session/device",
                &BindDeviceRequest {
                    device_id: x.1.clone(),
                },
            )
            .await
            .ok();
            x
        }
        None => {
            let (dev, reg) = engine::new_device(&device_name);
            api.post::<_, ()>("/devices", &reg)
                .await
                .context("registering this device")?;
            secrets.save_device(&user.id, &reg.device_id, &dev)?;
            ps.update(|p| {
                p.device_id = Some(reg.device_id.clone());
                p.device_name = Some(device_name.clone());
                p.device_pending = true;
            })?;
            (dev, reg.device_id)
        }
    };
    let code = engine::verification_code(&user.id, &device_id, &device.public, &public)
        .unwrap_or_default();

    let (account, recovered) = match &opts.recovery_key {
        Some(k) => (
            recover_with_key(&api, &user.id, &device_id, &device, k).await?,
            true,
        ),
        None => {
            ui::status(&format!(
                "\nThis device must be approved by one of your other devices.\n\
                 On a signed-in device run `coolpsonotes devices approve {device_id}` (or open Devices in the web app)\n\
                 and approve only if it shows this verification code:\n\n    {code}\n"
            ));
            wait_for_approval(
                &api,
                &user.id,
                &device_id,
                &device,
                opts.interactive,
                opts.approval_timeout,
            )
            .await?
        }
    };
    if account.public() != public {
        bail!("the unlocked account key does not match your published key; refusing to continue");
    }
    secrets.save_account(&user.id, &account)?;
    ps.update(|p| p.device_pending = false)?;
    Ok(LoginOutcome {
        user,
        device_id,
        fingerprint: account.public().fingerprint(),
        recovery_key: None,
        reused_device: false,
        recovered,
    })
}

/// Whether the server's device list confirms that `device_id` was revoked or
/// is unknown to the account.
async fn device_was_removed(api: &Api, device_id: &str) -> Result<bool> {
    let devices: Vec<DeviceInfo> = api
        .get("/devices")
        .await
        .context("checking whether this device was removed")?;
    Ok(devices
        .iter()
        .find(|d| d.id == device_id)
        .is_none_or(|d| d.status == DeviceStatus::Revoked))
}

/// Unwrap the account secret with the recovery key and approve this device.
pub async fn recover_with_key(
    api: &Api,
    user_id: &str,
    device_id: &str,
    device: &DeviceKey,
    key: &str,
) -> Result<AccountSecret> {
    let rk = RecoveryKey::parse(key)
        .map_err(|_| anyhow!("that is not a valid recovery key (check for typos)"))?;
    let env: RecoveryEnvelope = api
        .get("/me/recovery")
        .await
        .context("fetching the recovery envelope")?;
    let account = rk
        .unwrap_account(&env.recovery_envelope, user_id)
        .map_err(|_| anyhow!("the recovery key does not unlock this account"))?;
    let req = engine::approve_device(&account, user_id, device_id, &device.public)?;
    api.post::<_, ()>(&format!("/devices/{device_id}/activate"), &req)
        .await
        .context("activating this device")?;
    Ok(account)
}

async fn wait_for_approval(
    api: &Api,
    user_id: &str,
    device_id: &str,
    device: &DeviceKey,
    interactive: bool,
    timeout: Option<Duration>,
) -> Result<(AccountSecret, bool)> {
    // A plain thread (not a blocking task) so an unanswered prompt never keeps
    // the runtime from shutting down.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    if interactive && ui::stdin_is_tty() {
        ui::status(
            "Waiting for approval… (or type your recovery key and press Enter to unlock this device yourself)",
        );
        std::thread::spawn(move || {
            if let Ok(line) = rpassword::read_password() {
                let _ = tx.send(line);
            }
        });
    } else {
        drop(tx);
        ui::status("Waiting for approval…");
    }
    let deadline = Instant::now() + timeout.unwrap_or(Duration::from_secs(30 * 60));
    let mut delay = Duration::from_secs(2);
    loop {
        match api
            .get::<DeviceEnvelope>(&format!("/devices/{device_id}/envelope"))
            .await
        {
            Ok(env) => {
                let account = device
                    .unwrap_account(&env.account_envelope, user_id, device_id)
                    .map_err(|e| anyhow!("the approval could not be decrypted: {e}"))?;
                return Ok((account, false));
            }
            Err(e) if e.status() == Some(409) || e.is_network() => {}
            Err(e) if e.is_not_found() => {
                bail!("this device was rejected or removed by another device")
            }
            Err(e) => return Err(anyhow!(e).context("checking the approval")),
        }
        if Instant::now() > deadline {
            bail!("no approval yet; run `coolpsonotes login` again later to keep waiting");
        }
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            Some(line) = rx.recv() => {
                if !line.trim().is_empty() {
                    let account = recover_with_key(api, user_id, device_id, device, &line).await?;
                    return Ok((account, true));
                }
            }
        }
        delay = (delay * 3 / 2).min(Duration::from_secs(10));
    }
}

/// Options for `agent setup`.
#[derive(Debug, Clone)]
pub struct AgentSetupOptions {
    pub server: String,
    pub insecure_http: bool,
    pub client_id: String,
    pub client_secret_file: PathBuf,
}

#[derive(Debug, Clone)]
pub struct AgentOutcome {
    pub agent_id: String,
    pub display_name: String,
    pub fingerprint: String,
    pub created_keys: bool,
}

/// Configure a profile for an agent identity and create its key pair.
pub async fn agent_setup(
    dirs: &Dirs,
    profile: &str,
    opts: AgentSetupOptions,
) -> Result<AgentOutcome> {
    let server = normalize_server_url(&opts.server, opts.insecure_http)?;
    let secret_file = std::path::absolute(&opts.client_secret_file)
        .with_context(|| format!("resolving {}", opts.client_secret_file.display()))?;
    crate::api::read_secret_file(&secret_file)?;
    if fsutil::is_shared_readable(&secret_file) {
        ui::warn(&format!(
            "{} is readable by other users; restrict it with `chmod 600`",
            secret_file.display()
        ));
    }
    let ps = ProfileStore::new(dirs, profile)?;
    let existing = ps.load()?;
    if let Some(p) = &existing
        && p.kind == PrincipalKind::Human
        && p.user_id.is_some()
    {
        bail!("profile '{profile}' belongs to a person; use another --profile for the agent");
    }
    let api = Api::new(
        &server,
        Credentials::Agent {
            client_id: opts.client_id.clone(),
            secret_file: secret_file.clone(),
        },
    )?;
    let me = api
        .me()
        .await
        .context("signing in with the agent credentials")?;
    if me.user.kind != PrincipalKind::Agent {
        bail!("these credentials do not belong to an agent identity");
    }
    let agent_id = me.user.id.clone();
    if let Some(uid) = existing.as_ref().and_then(|p| p.user_id.as_ref())
        && *uid != agent_id
    {
        bail!("profile '{profile}' is set up for a different agent; use another --profile");
    }
    let secrets = SecretStore::open(&ps.data_dir())?;
    let local = secrets.load_account(&agent_id)?;
    let (account, created) = match (local, &me.account_key) {
        (Some(a), Some(k)) => {
            if k.verify()? != a.public() {
                bail!(
                    "the agent's key on the server differs from the local one; refusing to continue"
                );
            }
            (a, false)
        }
        (Some(a), None) => {
            let req = AccountSetupRequest {
                key: PublicAccountKey::new(
                    &agent_id,
                    1,
                    &a.public(),
                    &a.sign_account_key(&agent_id, 1),
                ),
                recovery_envelope: None,
                device: None,
            };
            api.put::<_, ()>("/me/account-key", &req)
                .await
                .context("uploading the agent key")?;
            (a, true)
        }
        (None, Some(_)) => bail!(
            "this agent already has an account key that was created elsewhere. Agent keys cannot be recovered or \
             copied: run the gateway where the agent was set up, or delete the agent and create a new one"
        ),
        (None, None) => {
            let (a, req) = engine::bootstrap_agent(&agent_id);
            // Saved first: if the upload fails, running `agent setup` again
            // uploads this key (the case above) instead of being locked out.
            secrets.save_account(&agent_id, &a)?;
            api.put::<_, ()>("/me/account-key", &req)
                .await
                .context("uploading the agent key (run `agent setup` again to retry)")?;
            (a, true)
        }
    };
    secrets.save_account(&agent_id, &account)?;
    let insecure = opts.insecure_http;
    ps.upsert(
        || Profile::new(&server, PrincipalKind::Agent, insecure),
        |p| {
            p.kind = PrincipalKind::Agent;
            p.server = server.clone();
            p.insecure_http |= insecure;
            p.user_id = Some(agent_id.clone());
            p.display_name = Some(me.user.display_name.clone());
            p.session_token = None;
            p.agent = Some(AgentCredentials {
                client_id: opts.client_id.clone(),
                client_secret_file: secret_file.clone(),
            });
        },
    )?;
    Ok(AgentOutcome {
        agent_id,
        display_name: me.user.display_name,
        fingerprint: account.public().fingerprint(),
        created_keys: created,
    })
}

/// End the session on the server and forget it locally. With `forget`, also
/// remove this device from the account and wipe every local secret and copy.
pub async fn logout(dirs: &Dirs, profile: &str, forget: bool) -> Result<()> {
    let ps = ProfileStore::new(dirs, profile)?;
    let Some(p) = ps.load()? else {
        bail!("profile '{profile}' does not exist")
    };
    if let Some(token) = &p.session_token {
        let api = Api::new(&p.server, Credentials::Session(token.clone()))?;
        if forget
            && let Some(dev) = &p.device_id
            && let Err(e) = api.delete(&format!("/devices/{dev}")).await
        {
            ui::warn(&format!(
                "could not remove this device from the account: {e}"
            ));
        }
        if let Err(e) = api
            .post::<_, ()>("/auth/logout", &serde_json::json!({}))
            .await
            && !e.is_unauthorized()
        {
            ui::warn(&format!("could not end the session on the server: {e}"));
        }
    }
    if forget {
        SecretStore::wipe(&ps.data_dir())?;
        ps.delete()?;
    } else {
        ps.update(|p| {
            p.session_token = None;
            p.session_expires_at = None;
        })?;
    }
    Ok(())
}

/// Read and validate a client secret file path (shared with the CLI).
pub fn check_secret_file(path: &Path) -> Result<()> {
    crate::api::read_secret_file(path)?;
    Ok(())
}
