//! Account and vault administration: vaults, members and sharing (with TOFU
//! key pinning), key rotation, devices, recovery keys and agent identities.

use anyhow::{Context, Result, anyhow, bail};
use coolpsonotes_core::engine::{self, PinStatus, VaultMeta, VaultReplica};
use coolpsonotes_core::keys::RecoveryKey;
use coolpsonotes_core::memberlog::LogBody;
use coolpsonotes_core::protocol::*;
use coolpsonotes_sync::members;

use crate::hub::Hub;
use crate::session::{Session, SharedVault, VerifiedUser};
use crate::ui;
use crate::vault::Vault;

/// Create a vault (the client picks the id and wraps epoch 1 to itself).
pub async fn create_vault(session: &Session, name: &str) -> Result<Vault> {
    let name = name.trim();
    if name.is_empty() {
        bail!("the vault name must not be empty");
    }
    if session.kind == PrincipalKind::Agent {
        bail!("agents cannot create vaults; the owner shares vaults with them");
    }
    let meta = VaultMeta::named(name);
    let (req, ring) = engine::new_vault(
        session.account(),
        &session.user_id,
        &meta,
        coolpsonotes_core::now_ms(),
    )?;
    let info: VaultInfo = session.api.post("/vaults", &req).await?;
    let replica = VaultReplica::new(&info.id, info.role, ring, meta);
    let mut v = Vault::new(
        session.vault_store(&info.id)?,
        replica,
        &info.owner_id,
        info.archived,
    );
    v.set_limits(&session.limits());
    v.save()?;
    Ok(v)
}

pub(crate) fn require_owner(v: &Vault) -> Result<()> {
    if v.role() != Role::Owner {
        bail!("only the owner of vault '{}' can do this", v.name());
    }
    if let Some(r) = &v.read_only {
        bail!("vault '{}' is read-only on this device: {r}", v.name());
    }
    Ok(())
}

pub async fn rename_vault(session: &Session, vault: &SharedVault, name: &str) -> Result<()> {
    let (id, ct, meta) = {
        let v = vault.lock().await;
        require_owner(&v)?;
        let mut meta = v.replica.meta.clone();
        meta.name = name.trim().to_string();
        if meta.name.is_empty() {
            bail!("the vault name must not be empty");
        }
        (v.id().to_string(), meta.encrypt(&v.replica.keyring)?, meta)
    };
    let req = UpdateVaultRequest {
        encrypted_meta: Some(ct),
        archived: None,
        ..Default::default()
    };
    let _: VaultInfo = session.api.patch(&format!("/vaults/{id}"), &req).await?;
    let mut v = vault.lock().await;
    v.set_meta(meta);
    v.save()
}

/// The vault's icon (an emoji, replacing a picture), or with `None` neither.
pub async fn set_vault_icon(
    session: &Session,
    vault: &SharedVault,
    icon: Option<&str>,
) -> Result<()> {
    let (id, ct, meta) = {
        let v = vault.lock().await;
        require_owner(&v)?;
        let mut meta = v.replica.meta.clone();
        meta.set_icon(icon.unwrap_or(""))?;
        meta.picture = None;
        (v.id().to_string(), meta.encrypt(&v.replica.keyring)?, meta)
    };
    let req = UpdateVaultRequest {
        encrypted_meta: Some(ct),
        archived: None,
        ..Default::default()
    };
    let _: VaultInfo = session.api.patch(&format!("/vaults/{id}"), &req).await?;
    let mut v = vault.lock().await;
    v.set_meta(meta);
    v.save()
}

pub async fn set_archived(session: &Session, vault: &SharedVault, archived: bool) -> Result<()> {
    let id = {
        let v = vault.lock().await;
        require_owner(&v)?;
        v.id().to_string()
    };
    let req = UpdateVaultRequest {
        encrypted_meta: None,
        archived: Some(archived),
        ..Default::default()
    };
    let _: VaultInfo = session.api.patch(&format!("/vaults/{id}"), &req).await?;
    let mut v = vault.lock().await;
    v.archived = archived;
    v.mark_state_dirty();
    v.save()
}

/// Let viewers (and people its notes are shared with read-only) comment,
/// or not (owner).
pub async fn set_viewers_comment(session: &Session, vault: &SharedVault, on: bool) -> Result<()> {
    let id = {
        let v = vault.lock().await;
        require_owner(&v)?;
        v.id().to_string()
    };
    let req = UpdateVaultRequest {
        viewers_comment: Some(on),
        ..Default::default()
    };
    let _: VaultInfo = session.api.patch(&format!("/vaults/{id}"), &req).await?;
    let mut v = vault.lock().await;
    v.viewers_comment = on;
    v.mark_state_dirty();
    v.save()
}

/// Delete a vault on the server and its local copy.
pub async fn delete_vault(session: &Session, vault: &SharedVault) -> Result<()> {
    let (id, store) = {
        let v = vault.lock().await;
        require_owner(&v)?;
        (v.id().to_string(), v.store().clone())
    };
    session.api.delete(&format!("/vaults/{id}")).await?;
    store.wipe()
}

/// Leave a vault shared with you; the local copy is removed.
pub async fn leave_vault(session: &Session, vault: &SharedVault) -> Result<()> {
    let (id, store, role) = {
        let v = vault.lock().await;
        (v.id().to_string(), v.store().clone(), v.role())
    };
    if role == Role::Owner {
        bail!("the owner cannot leave a vault; delete it instead");
    }
    let log_entry = session
        .log_entry(
            vault,
            LogBody::Leave {
                user_id: session.user_id.clone(),
            },
        )
        .await;
    match session
        .api
        .delete_with(
            &format!("/vaults/{id}/members/{}", session.user_id),
            &RemoveMemberRequest { log_entry },
        )
        .await
    {
        Ok(()) => {}
        Err(e) if e.is_not_found() => {}
        Err(e) => return Err(e.into()),
    }
    store.wipe()
}

pub struct MemberView {
    pub member: MemberInfo,
    pub key: Option<VerifiedUser>,
    pub key_error: Option<String>,
    /// Listed in the owner-signed membership log with this role and key
    /// (always true for vaults without a log).
    pub vouched: bool,
    /// The vault has a membership log.
    pub signed: bool,
}

pub async fn members(session: &Session, vault: &SharedVault) -> Result<Vec<MemberView>> {
    let (id, log) = {
        let v = vault.lock().await;
        (v.id().to_string(), v.log.clone())
    };
    let members = session.api.members(&id).await?;
    Ok(members
        .into_iter()
        .map(|m| {
            let vouched = log.vouches_for(&m);
            let (key, key_error) = match session.verify_user(&m.user) {
                Ok(key) => (key, None),
                Err(e) => (None, Some(format!("{e:#}"))),
            };
            MemberView {
                member: m,
                key,
                key_error,
                vouched,
                signed: log.has_log(),
            }
        })
        .collect())
}

fn looks_like_id(s: &str) -> bool {
    s.len() >= 16 && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// Find a user by id, exact e-mail or exact display name. A partial match is
/// never taken: sharing with "bobby" when "bob" was meant hands over keys.
pub async fn find_user(session: &Session, reference: &str) -> Result<PublicUser> {
    if looks_like_id(reference) {
        match session.api.user(reference).await {
            Ok(u) => return Ok(u),
            Err(e) if e.is_not_found() => {}
            Err(e) => return Err(e.into()),
        }
    }
    let found = session.api.search_users(reference).await?;
    let exact: Vec<&PublicUser> = found
        .iter()
        .filter(|u| {
            u.display_name.eq_ignore_ascii_case(reference)
                || u.email
                    .as_deref()
                    .is_some_and(|e| e.eq_ignore_ascii_case(reference))
                || u.id == reference
        })
        .collect();
    let list = |users: &[&PublicUser]| {
        users
            .iter()
            .map(|u| format!("  {}  {} ({})", u.id, u.display_name, u.kind.as_str()))
            .collect::<Vec<_>>()
            .join("\n")
    };
    match exact.as_slice() {
        [one] => Ok((*one).clone()),
        [] if found.is_empty() => bail!("no user matches '{reference}'"),
        [] => bail!(
            "no user is called exactly '{reference}'; did you mean one of these? Use the exact name, e-mail \
             or id:\n{}",
            list(&found.iter().collect::<Vec<_>>())
        ),
        several => bail!(
            "'{reference}' matches several users; use an id:\n{}",
            list(several)
        ),
    }
}

pub struct ShareOutcome {
    pub user: PublicUser,
    pub fingerprint: Option<String>,
    pub pin: Option<PinStatus>,
    /// The vault key was wrapped for them now (false: they have no keys yet;
    /// it is granted automatically once they set up encryption).
    pub key_shared: bool,
    pub updated_role: bool,
}

/// Share a vault (owner): verify the recipient's key, check it against the
/// pinned fingerprint, and wrap the current vault key for them.
pub async fn share(
    session: &Session,
    vault: &SharedVault,
    user_ref: &str,
    role: Role,
    accept_new_key: bool,
) -> Result<ShareOutcome> {
    if role == Role::Owner {
        bail!("a vault has exactly one owner; share as editor or viewer");
    }
    let (id, ring) = {
        let v = vault.lock().await;
        require_owner(&v)?;
        (v.id().to_string(), v.replica.keyring.clone())
    };
    let user = find_user(session, user_ref).await?;
    if user.id == session.user_id {
        bail!("you already own this vault");
    }
    let verified = session.verify_user(&user)?;
    if let Some(vu) = &verified
        && vu.pin == PinStatus::Changed
    {
        if !accept_new_key {
            bail!(
                "WARNING: the account key of {} CHANGED since you first shared with them.\n  new fingerprint: {}\n\
                     This happens if they reset their account — or if the server is trying to intercept the vault key.\n\
                     Compare the fingerprint with them in person or over a trusted channel; if it matches, re-run with \
                     --accept-new-key",
                user.display_name,
                vu.fingerprint
            );
        }
        session.accept_pin(&user.id, &vu.fingerprint)?;
    }
    let envelope = match &verified {
        Some(_) => Some(engine::share_envelope(&ring, &user)?),
        None => None,
    };
    let key_shared = envelope.is_some();
    let public = match &user.account_key {
        Some(k) if verified.is_some() => Some(k.verify()?),
        _ => None,
    };
    // The owner vouches for them, with their key pinned, in the vault's log.
    let member = members::log_member(&user.id, role, public.as_ref());
    let mut updated_role = false;
    let mut attempt = 0;
    let added = loop {
        let req = AddMemberRequest {
            user_id: user.id.clone(),
            role,
            key_envelope: envelope.clone(),
            log_entry: session
                .log_entry(
                    vault,
                    LogBody::Add {
                        member: member.clone(),
                    },
                )
                .await,
        };
        match session
            .api
            .post::<_, MemberInfo>(&format!("/vaults/{id}/members"), &req)
            .await
        {
            // Another change to the log got in first (this client's own
            // background work, or another device of the owner's): sign
            // again on top of it. The server took nothing of this request.
            Err(e) if is_log_race(&e) && attempt < 3 => {
                attempt += 1;
                session.reload_member_log(vault).await?;
            }
            r => break r,
        }
    };
    match added {
        Ok(_) => {}
        Err(e) if e.status() == Some(409) && e.to_string().contains("already a member") => {
            // Already a member: update the role and make sure they have the key.
            let log_entry = session
                .log_entry(
                    vault,
                    LogBody::Role {
                        user_id: user.id.clone(),
                        role,
                    },
                )
                .await;
            session
                .api
                .patch::<_, ()>(
                    &format!("/vaults/{id}/members/{}", user.id),
                    &UpdateMemberRequest { role, log_entry },
                )
                .await?;
            session.reload_member_log(vault).await?;
            // A new key they have now (accepted above) is pinned in the log.
            if let Some(k) = &public {
                let pinned = {
                    let v = vault.lock().await;
                    v.log.state.as_ref().is_none_or(|s| {
                        s.member(&user.id)
                            .and_then(|m| m.public().ok().flatten())
                            .is_some_and(|p| p == *k)
                    })
                };
                if !pinned {
                    session.pin_member_key(vault, &user.id, role, k).await?;
                }
            }
            updated_role = true;
            if let Some(env) = envelope {
                let _ = session
                    .api
                    .post::<_, ()>(
                        &format!("/vaults/{id}/keys/grant"),
                        &GrantKeysRequest {
                            envelopes: vec![env],
                        },
                    )
                    .await;
            }
        }
        Err(e) => return Err(e.into()),
    }
    session.reload_member_log(vault).await?;
    Ok(ShareOutcome {
        user,
        fingerprint: verified.as_ref().map(|v| v.fingerprint.clone()),
        pin: verified.map(|v| v.pin),
        key_shared,
        updated_role,
    })
}

/// The server refused a membership log entry because the log moved on since
/// it was signed: reload it and sign again.
fn is_log_race(e: &crate::api::ApiError) -> bool {
    e.status() == Some(409) && e.to_string().contains("changed meanwhile")
}

/// Remove a member and rotate the vault key so they cannot read anything
/// written afterwards. Returns the new epoch.
pub async fn unshare(
    session: &Session,
    vault: &SharedVault,
    user_ref: &str,
) -> Result<(PublicUser, u32)> {
    let id = {
        let v = vault.lock().await;
        require_owner(&v)?;
        v.id().to_string()
    };
    let members = session.api.members(&id).await?;
    let user = match members.iter().find(|m| {
        m.user.id == user_ref
            || m.user.display_name.eq_ignore_ascii_case(user_ref)
            || m.user
                .email
                .as_deref()
                .is_some_and(|e| e.eq_ignore_ascii_case(user_ref))
    }) {
        Some(m) => m.user.clone(),
        None => bail!("'{user_ref}' is not a member of this vault"),
    };
    if user.id == session.user_id {
        bail!("the owner cannot be removed");
    }
    let log_entry = session
        .log_entry(
            vault,
            LogBody::Remove {
                user_id: user.id.clone(),
            },
        )
        .await;
    session
        .api
        .delete_with(
            &format!("/vaults/{id}/members/{}", user.id),
            &RemoveMemberRequest { log_entry },
        )
        .await?;
    let name = vault.lock().await.name().to_string();
    let epoch = session.rotate_keys(vault).await.with_context(|| {
        format!(
            "{} was removed, but the vault key could not be renewed, so they could still read new notes if \
             the server passed them on. Finish with `coolpsonotes vault rotate \"{name}\"`",
            user.display_name
        )
    })?;
    Ok((user, epoch))
}

// ---------------------------------------------------------------- devices

pub async fn devices(session: &Session) -> Result<Vec<DeviceInfo>> {
    Ok(session.api.get("/devices").await?)
}

/// The verification code to compare before approving a pending device.
pub async fn pending_device(session: &Session, device_id: &str) -> Result<(DeviceInfo, String)> {
    let list = devices(session).await?;
    let d = list
        .into_iter()
        .find(|d| d.id == device_id)
        .ok_or_else(|| anyhow!("no device '{device_id}'"))?;
    if d.status != DeviceStatus::Pending {
        bail!(
            "device '{}' is not waiting for approval (status: {})",
            d.name,
            d.status.as_str()
        );
    }
    let code = engine::verification_code(
        &session.user_id,
        &d.id,
        &d.enc_public,
        &session.account().public(),
    )
    .ok_or_else(|| anyhow!("the device key is malformed"))?;
    Ok((d, code))
}

pub async fn approve_device(session: &Session, device: &DeviceInfo) -> Result<()> {
    let req = engine::approve_device(
        session.account(),
        &session.user_id,
        &device.id,
        &device.enc_public,
    )?;
    session
        .api
        .post::<_, ()>(&format!("/devices/{}/activate", device.id), &req)
        .await?;
    Ok(())
}

pub async fn revoke_device(session: &Session, device_id: &str) -> Result<()> {
    session.api.delete(&format!("/devices/{device_id}")).await?;
    Ok(())
}

/// Replace the recovery key; returns the new one (shown once).
pub async fn rotate_recovery(session: &Session) -> Result<String> {
    let (rk, req): (RecoveryKey, RecoveryUpdate) = engine::rotate_recovery(
        session.account(),
        &session.user_id,
        coolpsonotes_core::now_ms().max(0) as u64,
    );
    session.api.put::<_, ()>("/me/recovery", &req).await?;
    Ok(rk.to_display())
}

// ---------------------------------------------------------------- agents

pub async fn agents(session: &Session) -> Result<Vec<AgentInfo>> {
    Ok(session.api.get("/agents").await?)
}

pub async fn create_agent(session: &Session, name: &str) -> Result<CreateAgentResponse> {
    Ok(session
        .api
        .post(
            "/agents",
            &CreateAgentRequest {
                name: name.to_string(),
            },
        )
        .await?)
}

pub async fn delete_agent(session: &Session, agent_id: &str) -> Result<()> {
    Ok(session.api.delete(&format!("/agents/{agent_id}")).await?)
}

/// New client secret for an agent; optionally revoke the old credentials.
/// Create a new credential for an agent, hand it to `save` (which must keep
/// the secret, e.g. write it to a file), then revoke the old ones if asked.
/// If `save` fails the new credential is revoked again and the old ones stay,
/// so the agent is never left without a working secret.
pub async fn rotate_agent_secret(
    session: &Session,
    agent_id: &str,
    revoke_old: bool,
    save: impl FnOnce(&CreatedCredential) -> Result<()>,
) -> Result<CreatedCredential> {
    let before = agents(session).await?;
    let old: Vec<String> = before
        .iter()
        .find(|a| a.id == agent_id)
        .ok_or_else(|| anyhow!("no agent '{agent_id}'"))?
        .credentials
        .iter()
        .filter(|c| c.revoked_at.is_none())
        .map(|c| c.client_id.clone())
        .collect();
    let created: CreatedCredential = session
        .api
        .post(
            &format!("/agents/{agent_id}/credentials"),
            &serde_json::json!({}),
        )
        .await?;
    if let Err(e) = save(&created) {
        let _ = session
            .api
            .delete(&format!(
                "/agents/{agent_id}/credentials/{}",
                created.client_id
            ))
            .await;
        return Err(e.context("the new secret could not be saved; the old credentials still work"));
    }
    if revoke_old {
        for c in old {
            session
                .api
                .delete(&format!("/agents/{agent_id}/credentials/{c}"))
                .await?;
        }
    }
    Ok(created)
}

/// Owner side of the agent flow: the agent's fingerprint, to compare with
/// what `agent setup` printed.
pub async fn agent_fingerprint(session: &Session, agent_id: &str) -> Result<Option<String>> {
    let a = agents(session)
        .await?
        .into_iter()
        .find(|a| a.id == agent_id)
        .ok_or_else(|| anyhow!("no agent '{agent_id}'"))?;
    match a.account_key {
        Some(k) if k.user_id == a.id => Ok(Some(k.verify()?.fingerprint())),
        Some(_) => bail!("the server returned a key for a different principal"),
        None => Ok(None),
    }
}

/// Print-friendly pin status.
pub fn pin_label(p: PinStatus) -> &'static str {
    match p {
        PinStatus::New => "new (now pinned)",
        PinStatus::Match => "matches pin",
        PinStatus::Changed => "CHANGED",
    }
}

/// Owner housekeeping run by `vault members`/`status`: grant pending keys.
pub async fn grant_pending(hub: &Hub) {
    for v in hub.vaults() {
        match hub.session.grant_missing_keys(&v).await {
            Ok(names) if !names.is_empty() => {
                ui::status(&format!("shared the vault key with {}", names.join(", ")))
            }
            Ok(_) => {}
            Err(e) => tracing::debug!("granting keys: {e:#}"),
        }
    }
}
