//! The vault membership log in the browser (R3; rules in
//! `notekpr_sync::members`): verified whenever a vault's keys load,
//! with the vault keys checked against the owners' commitments. Owners sign
//! every membership change and give vault keys only to members the log
//! vouches for.

use notekpr_core::keys::{AccountPublic, VaultKeyring};
use notekpr_core::memberlog::{self, KeyCheckError, LogBody, LogEntry, LogError};
use notekpr_sync::members::{self as rules, MemberLog};

use super::*;

/// The key a vault's log is checked against: its creator's, who signed its
/// first entry — ours if we made it, else the key that entry pins for them,
/// checked against our pin for that person (trust on first use). `Ok(None)`:
/// no log yet; `Err`: a security problem.
fn creator_key(entries: &[LogEntry]) -> Result<Option<AccountPublic>, String> {
    if entries.is_empty() {
        return Ok(None);
    }
    let (creator, key) = memberlog::creator(entries).ok_or(
        "Security warning: this vault's membership history doesn't start as it should. The server may have been tampered with; tell the vault's owners and your administrator.",
    )?;
    step(|c| {
        if creator == c.user_id {
            return Ok(c.account.as_ref().map(|a| a.public()));
        }
        match engine::check_pin(&mut c.prefs.pins, &creator, &key.fingerprint()) {
            engine::PinStatus::Changed => Err(
                "Security warning: the encryption key of the person who created this vault changed since you last saw it. Membership changes can't be trusted until you compare fingerprints with them.".to_string(),
            ),
            engine::PinStatus::New => {
                save_prefs(c);
                Ok(Some(key))
            }
            engine::PinStatus::Match => Ok(Some(key)),
        }
    })
}

/// Remember where a verified log stands: in this browser, and for our
/// other apps in the synced preferences.
pub(crate) fn note_head(uid: &str, vault_id: &str, head: Option<(u64, Vec<u8>)>) {
    storage::set_log_seen(uid, vault_id, head.clone());
    step(|c| {
        if rules::note_head(&mut c.prefs.log_heads, vault_id, head) {
            save_prefs(c);
        }
    });
}

/// What a vault's keys were checked against ([`check_keys`]).
pub(super) struct Checked {
    pub log: MemberLog,
    /// The log as the server has it now, verified just now. Otherwise an
    /// earlier verified copy (the server couldn't give it): good for
    /// checking keys and authors, never for acting on as an owner.
    pub fresh: bool,
}

/// Why a vault's keys can't be used ([`check_keys`]).
pub(super) enum Unchecked {
    /// The server can't give the log now, and no earlier copy covers the
    /// keys: try again later.
    Unavailable(String),
    /// A security problem, to show instead of the vault.
    Refused(String),
}

const LOG_UNAVAILABLE: &str = "Who is in this vault can't be checked right now, so it isn't opened yet. It opens once the server answers again.";

/// Check `ring` against the vault's membership log before anything uses
/// it: the log as the server has it, verified now, or, when the server
/// can't give it, the copy verified earlier (this session's, else the one
/// kept for offline use). Keys no verified log vouches for are never used.
pub(super) async fn check_keys(vault_id: &str, ring: &VaultKeyring) -> Result<Checked, Unchecked> {
    match load_log(vault_id, ring).await {
        Ok(Some(log)) => return Ok(Checked { log, fresh: true }),
        Err(e) => return Err(Unchecked::Refused(e)),
        Ok(None) => {}
    }
    let earlier = match step(|c| c.member_logs.get(vault_id).cloned()) {
        Some(log) => Some(log),
        None => match step(|c| offline::stored_log(c, vault_id)) {
            Some(entries) => Some(verify_stored(vault_id, entries)?),
            None => None,
        },
    };
    let Some(log) = earlier else {
        return Err(Unchecked::Unavailable(LOG_UNAVAILABLE.into()));
    };
    match log.check_keyring(ring) {
        Ok(()) => Ok(Checked { log, fresh: false }),
        // Keys newer than our copy of the log: wait for the server's.
        Err(KeyCheckError::Unannounced(_)) => Err(Unchecked::Unavailable(LOG_UNAVAILABLE.into())),
        Err(e) => Err(Unchecked::Refused(keys_refused(&e))),
    }
}

/// A log kept for offline use, verified again (it was when kept; only the
/// server's copies are checked for rollback).
fn verify_stored(vault_id: &str, entries: Vec<LogEntry>) -> Result<MemberLog, Unchecked> {
    let anchor = match creator_key(&entries).map_err(Unchecked::Refused)? {
        Some(k) => k,
        None => step(|c| c.account.as_ref().map(|a| a.public()))
            .ok_or_else(|| Unchecked::Refused("no account keys on this device".into()))?,
    };
    MemberLog::verify(vault_id, &anchor, entries, None)
        .map_err(|e| Unchecked::Refused(log_refused(e)))
}

fn log_refused(e: LogError) -> String {
    match e {
        LogError::Newer(_) => "Who is in this vault was changed by a newer version of notekpr: reload the page to open it.".to_string(),
        e => format!(
            "Security warning: this vault's membership history doesn't check out ({e}). The server may have been tampered with; tell the vault's owners and your administrator."
        ),
    }
}

fn keys_refused(e: &KeyCheckError) -> String {
    format!(
        "Security warning: {e}. The server may be trying to read new notes, so this vault isn't opened. Tell the vault's owners and your administrator."
    )
}

/// Fetch and verify a vault's membership log and check `ring` against it.
/// `Ok(None)`: the server can't be reached (nothing is checked or changed:
/// [`check_keys`] decides what that means for keys). `Err`: a security
/// problem, to show instead of the vault.
pub(super) async fn load_log(
    vault_id: &str,
    ring: &VaultKeyring,
) -> Result<Option<MemberLog>, String> {
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
        // Without entries there's nothing the key checks.
        let anchor = match creator_key(&entries)? {
            Some(k) => k,
            None => step(|c| c.account.as_ref().map(|a| a.public()))
                .ok_or("no account keys on this device")?,
        };
        let seen = storage::log_seen(&uid, vault_id);
        let log = MemberLog::verify(
            vault_id,
            &anchor,
            entries,
            seen.as_ref().map(|(s, h)| (*s, h.as_slice())),
        )
        .map_err(log_refused)?;
        // Where our other apps saw it last, for a browser that never did.
        if let Some(head) = step(|c| c.prefs.log_heads.get(vault_id).cloned()) {
            log.check_head(&head).map_err(log_refused)?;
        }
        match log.check_keyring(ring) {
            Ok(()) => {
                note_head(&uid, vault_id, log.head());
                return Ok(Some(log));
            }
            // A rotation between reading the keys and the log.
            Err(KeyCheckError::Unannounced(_)) if attempt == 1 => continue,
            Err(e) => return Err(keys_refused(&e)),
        }
    }
}

/// Reload the vault's log after a change (keeps the old one if offline).
/// `true`: a log verified just now is in place, as owner actions need
/// (giving keys, rotating them); `false`: it couldn't be had.
pub async fn reload_log(vault_id: &str) -> bool {
    let Some(ring) = step(|c| Some(c.replicas.get(vault_id)?.keyring.clone())) else {
        return false;
    };
    match load_log(vault_id, &ring).await {
        Ok(Some(log)) => {
            let applied = step(|c| {
                offline::note(c, |b| {
                    b.logs.insert(vault_id.to_string(), log.entries.clone());
                });
                c.member_logs.insert(vault_id.to_string(), log);
                super::vaults::publish_vaults(c);
                apply_authors(c, vault_id)
            });
            after_apply(vault_id, applied);
            true
        }
        Ok(None) => false,
        Err(e) => {
            error(e);
            false
        }
    }
}

/// Owner actions (giving or renewing keys) act on the log as the server
/// has it now: reload it, or say why not.
pub async fn fresh_log(vault_id: &str) -> Result<(), String> {
    if reload_log(vault_id).await {
        Ok(())
    } else {
        Err("Who is in this vault can't be checked right now; please try again in a moment.".into())
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
        leptos::task::spawn_local(async move {
            reload_log(&v).await;
        });
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
                c.member_logs.get(&vault_id)?,
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
                    member_log(c, vault_id).has_share(&note, &sh.user.id, &sh.role, &key),
                )
            });
            if st == engine::PinStatus::Changed {
                return Err(format!("the key of {} changed", sh.user.display_name));
            }
            if !logged {
                append(
                    vault_id,
                    rules::share(&note, &sh.user.id, sh.role.clone(), &key),
                )
                .await?;
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

/// Whether the owner's log lists the member with this role and key. Without
/// a verified log, nobody is.
pub fn vouched(c: &Core, vault_id: &str, m: &MemberInfo) -> bool {
    c.member_logs
        .get(vault_id)
        .is_some_and(|l| l.vouches_for(m))
}

/// Whether the log has the note shared with the server's recipient `s`, in
/// that role and with that key: only they get its keys. Without a verified
/// log, nobody is.
pub fn vouched_share(c: &Core, vault_id: &str, note: &str, s: &ObjectShareInfo) -> bool {
    c.member_logs
        .get(vault_id)
        .is_some_and(|l| l.vouches_for_share(note, s))
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
        list.push((m.user.id.clone(), m.role.clone(), key));
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
