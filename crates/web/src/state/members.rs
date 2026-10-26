//! The vault membership log in the browser (R3; rules in
//! `notekpr_sync::members`): verified whenever a vault's keys load,
//! with the vault keys checked against the owner's commitments. Owners sign
//! every membership change and give vault keys only to members the log
//! vouches for.

use notekpr_core::keys::{AccountPublic, VaultKeyring};
use notekpr_core::memberlog::{KeyCheckError, LogBody, LogEntry};
use notekpr_sync::members::{self as rules, MemberLog};

use super::*;

/// The key a vault's log must be signed with: ours, or the owner's as
/// pinned in this account (trust on first use). `Ok(None)`: the server
/// can't be reached; `Err`: a security problem.
async fn owner_key(owner_id: &str) -> Result<Option<AccountPublic>, String> {
    let known = step(|c| {
        let own = (c.user_id == owner_id)
            .then(|| c.account.as_ref().map(|a| a.public()))
            .flatten();
        own.or_else(|| c.owner_keys.get(owner_id).cloned())
    });
    if let Some(k) = known {
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
    step(|c| {
        match engine::check_pin(&mut c.prefs.pins, owner_id, &key.fingerprint()) {
            engine::PinStatus::Changed => {
                return Err(format!(
                    "Security warning: the encryption key of {}, who owns this vault, changed since you last saw it. Membership changes can't be trusted until you compare fingerprints with them.",
                    owner.display_name
                ));
            }
            engine::PinStatus::New => save_prefs(c),
            engine::PinStatus::Match => {}
        }
        c.owner_keys.insert(owner_id.to_string(), key.clone());
        Ok(Some(key))
    })
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
    let uid = step(|c| c.user_id.clone());
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
    let Some((owner, ring)) = step(|c| {
        Some((
            c.vault_infos.get(vault_id)?.owner_id.clone(),
            c.replicas.get(vault_id)?.keyring.clone(),
        ))
    }) else {
        return;
    };
    match load_log(vault_id, &owner, &ring).await {
        Ok(Some(log)) => {
            let applied = step(|c| {
                c.member_logs.insert(vault_id.to_string(), log);
                apply_authors(c, vault_id)
            });
            after_apply(vault_id, applied);
        }
        Ok(None) => {}
        Err(e) => error(e),
    }
}

/// Check the vault's content against who may write it, from its log, and
/// what waited for a fresh log once more. Returns what that let through
/// (for `after_apply`).
pub(super) fn apply_authors(c: &mut Core, vault_id: &str) -> engine::Applied {
    let authors = c.member_logs.get(vault_id).and_then(|l| l.authors());
    let Some(r) = c.replicas.get_mut(vault_id).filter(|r| r.member) else {
        return engine::Applied::default();
    };
    let rechecked = r.needs_log();
    let (applied, errors) = r.set_authors(authors);
    let refused = errors
        .iter()
        .filter(|e| matches!(e, engine::EngineError::Author(_)))
        .count();
    let needs_keys = rechecked && r.needs_keys();
    if refused > 0 {
        util::warn(&format!(
            "refused {refused} change(s) not signed by someone who may write here"
        ));
    }
    // What passed may still need a key we don't have yet.
    if needs_keys {
        keys_missing(c, vault_id);
    }
    applied
}

/// Content by someone our copy of the log doesn't know yet waits for a
/// fresh copy: reload it, then that content is checked again.
pub(super) fn log_missing(c: &Core, vault_id: &str) {
    if c.replicas.get(vault_id).is_some_and(|r| r.needs_log()) {
        let v = vault_id.to_string();
        leptos::task::spawn_local(async move { reload_log(&v).await });
    }
}

/// Owner: sign `body` into the vault's log.
async fn append(vault_id: &str, body: LogBody) -> Result<(), String> {
    let entry = step(|c| log_entry(c, vault_id, body)).ok_or("the vault has no membership log")?;
    api::post::<_, ()>(
        &format!("/vaults/{vault_id}/log"),
        &AppendLogRequest { entry },
    )
    .await
    .map_err(|e| e.message)?;
    reload_log(vault_id).await;
    Ok(())
}

/// Owner: make sure the vault's log has started and requires every change
/// to be signed by its author (signing in the people single notes are
/// already shared with first, so their edits keep counting).
/// Owner: sign the keys of shared notes given before grants were signed,
/// so the people they are shared with can use them.
pub(super) async fn sign_missing_grants(vault_id: String) {
    let shared = step(|c| {
        c.replicas
            .get(&vault_id)
            .map(|r| r.shared.clone())
            .unwrap_or_default()
    });
    for note in shared {
        let Ok(shares) =
            api::get::<Vec<ObjectShareInfo>>(&format!("/vaults/{vault_id}/objects/{note}/shares"))
                .await
        else {
            continue;
        };
        let grants = step(|c| {
            let ring = c.replicas.get(&vault_id)?.doc_keys.get(&note)?;
            Some(rules::missing_grant_signatures(
                c.account.as_ref()?,
                &vault_id,
                &note,
                ring,
                &shares,
            ))
        })
        .unwrap_or_default();
        if !grants.is_empty() {
            let _ = api::put::<_, ()>(
                &format!("/vaults/{vault_id}/objects/{note}/keys/signatures"),
                &SignGrantsRequest { grants },
            )
            .await;
        }
    }
}

pub async fn sign_vault(vault_id: String) {
    if !step(|c| member_log(c, &vault_id).has_log()) {
        start_log(vault_id.clone()).await;
    }
    if !step(|c| member_log(c, &vault_id).needs_signatures()) {
        return;
    }
    if let Err(e) = require_signatures(&vault_id).await {
        util::warn(&format!("could not require signed changes: {e}"));
    }
}

async fn require_signatures(vault_id: &str) -> Result<(), String> {
    let shared = step(|c| {
        c.replicas
            .get(vault_id)
            .map(|r| r.shared.clone())
            .unwrap_or_default()
    });
    for note in shared {
        let shares: Vec<ObjectShareInfo> =
            match api::get(&format!("/vaults/{vault_id}/objects/{note}/shares")).await {
                Ok(s) => s,
                Err(e) if e.status == 404 => continue,
                Err(e) => return Err(e.message),
            };
        for sh in shares {
            let Some(Ok(key)) = sh.user.account_key.as_ref().map(|k| k.verify()) else {
                continue;
            };
            let (st, logged) = step(|c| {
                (
                    engine::check_pin(&mut c.prefs.pins, &sh.user.id, &key.fingerprint()),
                    member_log(c, vault_id).has_share(&note, &sh.user.id, sh.role, &key),
                )
            });
            if st == engine::PinStatus::Changed {
                return Err(format!("the key of {} changed", sh.user.display_name));
            }
            if !logged {
                append(vault_id, rules::share(&note, &sh.user.id, sh.role, &key)).await?;
            }
        }
    }
    step(|c| save_prefs(c));
    let head = api::get::<VaultInfo>(&format!("/vaults/{vault_id}"))
        .await
        .map_err(|e| e.message)?
        .head_seq;
    append(vault_id, LogBody::RequireSigned { from_seq: head }).await
}

/// Sign `body` as the vault's next log entry (`None`: no log yet).
pub fn log_entry(c: &Core, vault_id: &str, body: LogBody) -> Option<LogEntry> {
    let log = c.member_logs.get(vault_id)?;
    log.next(
        c.account.as_ref()?,
        &c.user_id,
        vault_id,
        body,
        util::now_ms(),
    )
}

/// Sign `body` as the entry after `prev` (signed, not sent yet: two changes
/// in one request), or as the next one.
pub fn log_entry_after(
    c: &Core,
    vault_id: &str,
    prev: Option<&LogEntry>,
    body: LogBody,
) -> Option<LogEntry> {
    let log = c.member_logs.get(vault_id)?;
    log.next_after(
        c.account.as_ref()?,
        &c.user_id,
        vault_id,
        prev,
        body,
        util::now_ms(),
    )
}

pub fn member_log(c: &Core, vault_id: &str) -> MemberLog {
    c.member_logs.get(vault_id).cloned().unwrap_or_default()
}

/// Whether the owner's log lists the member with this role and key.
pub fn vouched(c: &Core, vault_id: &str, m: &MemberInfo) -> bool {
    c.member_logs.get(vault_id).is_none_or(|l| l.vouches_for(m))
}

/// Owner: start the log of a vault from before it, with a checkpoint of the
/// members as they are now (their keys checked against our pins).
async fn start_log(vault_id: String) {
    let Ok(members) = api::get::<Vec<MemberInfo>>(&format!("/vaults/{vault_id}/members")).await
    else {
        return;
    };
    let mut list = Vec::new();
    for m in &members {
        let key = match &m.user.account_key {
            Some(k) => match k.verify() {
                Ok(k) => {
                    let st = step(|c| {
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
    let entry = step(|c| {
        save_prefs(c);
        let ring = c.replicas.get(&vault_id)?.keyring.clone();
        let body = rules::checkpoint(&vault_id, &ring, &list)?;
        log_entry(c, &vault_id, body)
    });
    let Some(entry) = entry else {
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
    let Some(entry) = step(|c| log_entry(c, vault_id, body)) else {
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
