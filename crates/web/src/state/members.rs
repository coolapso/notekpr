//! The vault membership log in the browser (R3; rules in
//! `coolpsonotes_sync::members`): verified whenever a vault's keys load,
//! with the vault keys checked against the owner's commitments. Owners sign
//! every membership change and give vault keys only to members the log
//! vouches for.

use coolpsonotes_core::keys::{AccountPublic, VaultKeyring};
use coolpsonotes_core::memberlog::{KeyCheckError, LogBody, LogEntry};
use coolpsonotes_sync::members::{self as rules, MemberLog};

use super::*;

/// The key a vault's log must be signed with: ours, or the owner's as
/// pinned in this account (trust on first use). `Ok(None)`: the server
/// can't be reached; `Err`: a security problem.
async fn owner_key(owner_id: &str) -> Result<Option<AccountPublic>, String> {
    let own = with(|c| {
        (c.user_id == owner_id)
            .then(|| c.account.as_ref().map(|a| a.public()))
            .flatten()
    });
    if let Some(k) = own {
        return Ok(Some(k));
    }
    if let Some(k) = with(|c| c.owner_keys.get(owner_id).cloned()) {
        return Ok(Some(k));
    }
    let owner: PublicUser = match api::get(&format!("/users/{owner_id}")).await {
        Ok(u) => u,
        Err(e) if e.status == 0 || e.status >= 500 => return Ok(None),
        Err(e) => return Err(e.message),
    };
    let key = owner
        .account_key
        .as_ref()
        .ok_or("the vault's owner has no encryption key")?
        .verify()
        .map_err(|e| format!("the vault owner's key is invalid: {e}"))?;
    let status = with(|c| engine::check_pin(&mut c.prefs.pins, owner_id, &key.fingerprint()));
    match status {
        engine::PinStatus::Changed => Err(format!(
            "Security warning: the encryption key of {}, who owns this vault, changed since you last saw it. Membership changes can't be trusted until you compare fingerprints with them.",
            owner.display_name
        )),
        engine::PinStatus::New | engine::PinStatus::Match => {
            if status == engine::PinStatus::New {
                save_prefs();
            }
            with(|c| c.owner_keys.insert(owner_id.to_string(), key.clone()));
            Ok(Some(key))
        }
    }
}

/// Fetch and verify a vault's membership log and check `ring` against it.
/// `Ok(None)`: the server can't be reached (nothing is checked or changed).
/// `Err`: a security problem, to show instead of the vault.
pub(super) async fn load_log(
    vault_id: &str,
    owner_id: &str,
    ring: &VaultKeyring,
) -> Result<Option<MemberLog>, String> {
    let Some(anchor) = owner_key(owner_id).await? else {
        return Ok(None);
    };
    let uid = with(|c| c.user_id.clone());
    let mut attempt = 0;
    loop {
        attempt += 1;
        let entries = match api::get::<MemberLogResponse>(&format!("/vaults/{vault_id}/log")).await
        {
            Ok(r) => r.entries,
            Err(e) if e.status == 0 || e.status >= 500 => return Ok(None),
            Err(e) => return Err(e.message),
        };
        let seen = storage::log_seen(&uid, vault_id);
        let log = MemberLog::verify(
            vault_id,
            &anchor,
            entries,
            seen.as_ref().map(|(s, h)| (*s, h.as_slice())),
        )
        .map_err(|e| {
            format!(
                "Security warning: this vault's membership history doesn't check out ({e}). The server may have been tampered with; tell the vault's owner and your administrator."
            )
        })?;
        match log.check_keyring(ring) {
            Ok(()) => {
                storage::set_log_seen(&uid, vault_id, log.head());
                return Ok(Some(log));
            }
            // A rotation between reading the keys and the log.
            Err(KeyCheckError::Unannounced(_)) if attempt == 1 => continue,
            Err(e) => {
                return Err(format!(
                    "Security warning: {e}. The server may be trying to read new notes, so this vault isn't opened. Tell the vault's owner and your administrator."
                ));
            }
        }
    }
}

/// Reload the vault's log after a change (keeps the old one if offline).
pub async fn reload_log(vault_id: &str) {
    let Some((owner, ring)) = with(|c| {
        Some((
            c.vault_infos.get(vault_id)?.owner_id.clone(),
            c.replicas.get(vault_id)?.keyring.clone(),
        ))
    }) else {
        return;
    };
    match load_log(vault_id, &owner, &ring).await {
        Ok(Some(log)) => {
            with(|c| c.member_logs.insert(vault_id.to_string(), log));
        }
        Ok(None) => {}
        Err(e) => error(e),
    }
}

/// Sign `body` as the vault's next log entry (`None`: no log yet).
pub fn log_entry(vault_id: &str, body: LogBody) -> Option<LogEntry> {
    with(|c| {
        let log = c.member_logs.get(vault_id)?;
        log.next(
            c.account.as_ref()?,
            &c.user_id,
            vault_id,
            body,
            util::now_ms(),
        )
    })
}

pub fn member_log(vault_id: &str) -> MemberLog {
    with(|c| c.member_logs.get(vault_id).cloned().unwrap_or_default())
}

/// Whether the owner's log lists the member with this role and key.
pub fn vouched(vault_id: &str, m: &MemberInfo) -> bool {
    with(|c| c.member_logs.get(vault_id).is_none_or(|l| l.vouches_for(m)))
}

/// Owner: start the log of a vault from before it, with a checkpoint of the
/// members as they are now (their keys checked against our pins).
pub(super) async fn start_log(vault_id: String) {
    let Ok(members) = api::get::<Vec<MemberInfo>>(&format!("/vaults/{vault_id}/members")).await
    else {
        return;
    };
    let mut list = Vec::new();
    for m in &members {
        let key = match &m.user.account_key {
            Some(k) => match k.verify() {
                Ok(k) => {
                    let st = with(|c| {
                        engine::check_pin(&mut c.prefs.pins, &m.user.id, &k.fingerprint())
                    });
                    if st == engine::PinStatus::Changed {
                        util::warn("not starting the membership log: a member's key changed");
                        return;
                    }
                    Some(k)
                }
                Err(_) => None,
            },
            None => None,
        };
        list.push((m.user.id.clone(), m.role, key));
    }
    save_prefs();
    let Some(ring) = with(|c| c.replicas.get(&vault_id).map(|r| r.keyring.clone())) else {
        return;
    };
    let Some(body) = rules::checkpoint(&vault_id, &ring, &list) else {
        return;
    };
    let Some(entry) = log_entry(&vault_id, body) else {
        return;
    };
    if api::post::<_, ()>(
        &format!("/vaults/{vault_id}/log"),
        &AppendLogRequest { entry },
    )
    .await
    .is_ok()
    {
        reload_log(&vault_id).await;
    }
}

/// Owner: pin a member's (new) account key in the log (`Rekey`).
pub async fn pin_member_key(
    vault_id: &str,
    user_id: &str,
    role: Role,
    key: &AccountPublic,
) -> Result<(), String> {
    let body = LogBody::Rekey {
        member: rules::log_member(user_id, role, Some(key)),
    };
    let Some(entry) = log_entry(vault_id, body) else {
        return Ok(());
    };
    api::post::<_, ()>(
        &format!("/vaults/{vault_id}/log"),
        &AppendLogRequest { entry },
    )
    .await
    .map_err(|e| e.message)?;
    reload_log(vault_id).await;
    Ok(())
}
