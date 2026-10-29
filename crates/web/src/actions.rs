//! User-facing operations. Each one edits CRDT state locally (encrypting the
//! resulting update for sync) or calls the REST API for membership and keys.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

use leptos::prelude::Update;
use leptos::prelude::*;
use notekpr_core::doc::{EntryKind, IndexDoc, NoteDoc};
use notekpr_core::engine::{self, ObjectMeta, PinStatus, VaultMeta, VaultPicture, VaultReplica};
use notekpr_core::memberlog::{LogBody, LogEntry};
use notekpr_core::moving::{self, Unmovable, What};
use notekpr_core::protocol::*;
use notekpr_core::textdiff::Edit;
use notekpr_core::{markdown, order, paths};
use wasm_bindgen::JsCast;

use crate::editor::syntax::Target;
use crate::editor::{self, Host};
use crate::state::{self, Core, Route, error, info, local_update, navigate, step, success};
use crate::{api, knowledge, render, util, zipfile};

fn now() -> i64 {
    util::now_ms()
}

// ------------------------------------------------------------------ vaults

pub async fn create_vault(name: String) -> Option<String> {
    if notekpr_core::quick::is_reserved_name(&name) {
        error(notekpr_core::quick::reserved_name_message());
        return None;
    }
    match create_vault_of(&name, VaultKind::Regular).await {
        Ok(v) => v,
        Err(e) => {
            error(format!("Could not create the vault: {e}"));
            None
        }
    }
}

/// Create a vault of `kind` named `name`. `Ok(None)`: nothing to make (no
/// name, no keys) or, for quick notes, they exist already.
pub async fn create_vault_of(name: &str, kind: VaultKind) -> Result<Option<String>, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Ok(None);
    }
    let made = step(|c| {
        c.account
            .as_ref()
            .map(|a| engine::new_vault(a, &c.user_id, &VaultMeta::named(&name), now()))
    });
    let (mut req, ring) = match made {
        Some(Ok(x)) => x,
        Some(Err(e)) => return Err(e.to_string()),
        None => return Ok(None),
    };
    req.kind = kind;
    let info: VaultInfo = match api::post("/vaults", &req).await {
        Ok(i) => i,
        Err(e) if e.status == 409 && kind == VaultKind::QuickNotes => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let id = info.id.clone();
    let note_keys = state::ui()
        .server
        .get_untracked()
        .is_some_and(|s| s.note_keys);
    step(|c| {
        // Its membership log starts with its creation.
        let log = c.account.as_ref().and_then(|a| {
            notekpr_sync::members::MemberLog::verify(
                &id,
                &a.public(),
                req.log_entry.clone().into_iter().collect(),
                None,
            )
            .ok()
        });
        if let Some(log) = log {
            crate::storage::set_log_seen(&c.user_id, &id, log.head());
            c.member_logs.insert(id.clone(), log);
        }
        let r = VaultReplica::new(&id, Role::Owner, ring, VaultMeta::named(&name));
        c.replicas.insert(id.clone(), r);
        c.outboxes.entry(id.clone()).or_default().note_keys = note_keys;
        c.vault_infos.insert(id.clone(), info);
        // A brand-new vault has nothing to catch up on.
        c.loaded.insert(id.clone());
        state::send(
            c,
            &ClientMessage::Subscribe {
                vault_id: id.clone(),
                since: Some(0),
                object_id: None,
            },
        );
    });
    // Every change to the new vault is signed by its author from the start.
    state::sign_vault(id.clone()).await;
    step(|c| state::publish_vaults(c));
    Ok(Some(id))
}

/// Change the vault's metadata (its name, icon or picture; the owner's to
/// change) and store it, encrypted. `false` if that failed (and was said).
pub async fn update_vault_meta(
    vault: &str,
    change: impl FnOnce(&mut VaultMeta) -> Result<(), String>,
) -> bool {
    let made = step(|c| {
        let r = c.replicas.get(vault)?;
        let mut meta = r.meta.clone();
        Some(change(&mut meta).and_then(|()| {
            let ct = meta.encrypt(&r.keyring).map_err(|e| e.to_string())?;
            Ok((meta, ct))
        }))
    });
    let (meta, ct) = match made {
        Some(Ok(x)) => x,
        Some(Err(e)) => {
            error(e);
            return false;
        }
        None => return false,
    };
    match api::patch::<_, VaultInfo>(
        &format!("/vaults/{vault}"),
        &UpdateVaultRequest {
            encrypted_meta: Some(ct),
            archived: None,
            ..Default::default()
        },
    )
    .await
    {
        Ok(i) => {
            step(|c| {
                if let Some(r) = c.replicas.get_mut(vault) {
                    r.meta = meta;
                }
                c.vault_infos.insert(vault.to_string(), i);
                state::publish_vaults(c);
            });
            true
        }
        Err(e) => {
            error(e.message);
            false
        }
    }
}

pub async fn rename_vault(vault: &str, name: &str) -> bool {
    let name = name.trim().to_string();
    update_vault_meta(vault, |m| {
        if notekpr_core::quick::is_reserved_name(&name) {
            return Err(notekpr_core::quick::reserved_name_message());
        }
        m.name = name;
        Ok(())
    })
    .await
}

/// An emoji (or a few characters) for the vault, instead of its picture;
/// blank removes both.
pub async fn set_vault_icon(vault: &str, icon: &str) -> bool {
    update_vault_meta(vault, |m| {
        m.set_icon(icon).map_err(|e| e.to_string())?;
        m.picture = None;
        Ok(())
    })
    .await
}

/// A picture for the vault, made from an image file.
pub async fn set_vault_picture(vault: &str, file: web_sys::File) -> bool {
    match vault_picture_from(&file).await {
        Ok(p) => {
            update_vault_meta(vault, |m| {
                m.picture = Some(p);
                Ok(())
            })
            .await
        }
        Err(e) => {
            error(e);
            false
        }
    }
}

/// The middle square of an image, at most [`VaultPicture::MAX_SIDE`] pixels,
/// encoded again — WebP where the browser can — and smaller until it fits.
async fn vault_picture_from(file: &web_sys::File) -> Result<VaultPicture, String> {
    let img = util::load_image(file)
        .await
        .ok_or("That file isn't an image this browser can open.")?;
    let largest = img
        .natural_width()
        .min(img.natural_height())
        .min(VaultPicture::MAX_SIDE);
    let mut sides = vec![largest];
    sides.extend([96, 64, 48].into_iter().filter(|s| *s < largest));
    // Keeping transparency first; JPEG (on white) only if nothing else fits.
    for (mime, opaque) in [("image/webp", false), ("image/jpeg", true)] {
        for &side in &sides {
            if let Some((ty, data)) = util::encode_square(&img, side, mime, 0.85, opaque)
                && let Ok(p) = VaultPicture::new(&ty, data)
            {
                return Ok(p);
            }
        }
    }
    Err("That picture couldn't be made small enough; try a simpler one.".into())
}

pub async fn set_archived(vault: &str, archived: bool) {
    match api::patch::<_, VaultInfo>(
        &format!("/vaults/{vault}"),
        &UpdateVaultRequest {
            encrypted_meta: None,
            archived: Some(archived),
            ..Default::default()
        },
    )
    .await
    {
        Ok(i) => step(|c| {
            c.vault_infos.insert(vault.to_string(), i);
            state::publish_vaults(c);
        }),
        Err(e) => error(e.message),
    }
}

/// Let viewers (and people the vault's notes are shared with read-only)
/// comment, or not (owner).
pub async fn set_viewers_comment(vault: &str, on: bool) {
    match api::patch::<_, VaultInfo>(
        &format!("/vaults/{vault}"),
        &UpdateVaultRequest {
            viewers_comment: Some(on),
            ..Default::default()
        },
    )
    .await
    {
        Ok(i) => step(|c| {
            c.vault_infos.insert(vault.to_string(), i);
            state::publish_vaults(c);
        }),
        Err(e) => error(e.message),
    }
}

pub async fn delete_vault(vault: &str) {
    match api::delete(&format!("/vaults/{vault}")).await {
        Ok(()) => {
            step(|c| {
                c.replicas.remove(vault);
                c.vault_infos.remove(vault);
                state::publish_vaults(c);
            });
            navigate(Route::Home);
            success("Vault deleted.");
        }
        Err(e) => error(e.message),
    }
}

pub async fn leave_vault(vault: &str) {
    let (uid, log_entry) = step(|c| {
        let body = LogBody::Leave {
            user_id: c.user_id.clone(),
        };
        (c.user_id.clone(), state::log_entry(c, vault, body))
    });
    match api::delete_with(
        &format!("/vaults/{vault}/members/{uid}"),
        &RemoveMemberRequest {
            log_entry,
            ..Default::default()
        },
    )
    .await
    {
        Ok(()) => {
            step(|c| {
                c.replicas.remove(vault);
                c.vault_infos.remove(vault);
                state::publish_vaults(c);
            });
            navigate(Route::Home);
        }
        Err(e) => error(e.message),
    }
}

// ------------------------------------------------------------------ sharing

pub fn fingerprint(user: &PublicUser) -> Option<String> {
    user.account_key
        .as_ref()?
        .verify()
        .ok()
        .map(|p| p.fingerprint())
}

pub fn pin_status(user: &PublicUser) -> Option<PinStatus> {
    let fp = fingerprint(user)?;
    Some(step(|c| match c.prefs.pins.get(&user.id) {
        None => PinStatus::New,
        Some(p) if *p == fp => PinStatus::Match,
        Some(_) => PinStatus::Changed,
    }))
}

/// Explicitly accept a (changed) key after verifying it out of band.
pub fn trust_key(user: &PublicUser) {
    if let Some(fp) = fingerprint(user) {
        step(|c| {
            c.prefs.pins.insert(user.id.clone(), fp);
            state::save_prefs(c);
        });
    }
}

fn check_trust(user: &PublicUser) -> Result<(), String> {
    let Some(fp) = fingerprint(user) else {
        return Ok(());
    };
    step(
        |c| match engine::check_pin(&mut c.prefs.pins, &user.id, &fp) {
            PinStatus::Changed => Err(format!(
                "The encryption key of {} changed since you last shared with them. This can happen if they reset their account — or if someone is intercepting. Compare fingerprints with them before continuing.",
                user.display_name
            )),
            PinStatus::New => {
                state::save_prefs(c);
                Ok(())
            }
            PinStatus::Match => Ok(()),
        },
    )
}

pub async fn share(vault: &str, user: &PublicUser, role: Role) -> Result<(), String> {
    let envelope = if user.account_key.is_some() {
        check_trust(user)?;
        let env = step(|c| {
            c.replicas
                .get(vault)
                .map(|r| engine::share_envelope(&r.keyring, user))
        });
        match env {
            Some(Ok(e)) => Some(e),
            Some(Err(e)) => return Err(e.to_string()),
            None => return Err("vault not loaded".into()),
        }
    } else {
        None
    };
    let pending = envelope.is_none();
    // The owner vouches for them, with their key pinned, in the vault's log.
    let key = match &user.account_key {
        Some(k) => Some(k.verify().map_err(|e| e.to_string())?),
        None => None,
    };
    let member = notekpr_sync::members::log_member(&user.id, role.clone(), key.as_ref());
    api::post::<_, MemberInfo>(
        &format!("/vaults/{vault}/members"),
        &AddMemberRequest {
            user_id: user.id.clone(),
            role,
            key_envelope: envelope,
            log_entry: step(|c| state::log_entry(c, vault, LogBody::Add { member })),
        },
    )
    .await
    .map_err(|e| e.message)?;
    state::reload_log(vault).await;
    step(|c| c.users.insert(user.id.clone(), user.clone()));
    if pending {
        info(format!(
            "{} will get access after their first sign-in.",
            user.display_name
        ));
    } else {
        success(format!("Shared with {}.", user.display_name));
    }
    Ok(())
}

pub async fn set_role(vault: &str, user_id: &str, role: Role) {
    let log_entry = step(|c| {
        state::log_entry(
            c,
            vault,
            LogBody::Role {
                user_id: user_id.to_string(),
                role: role.clone(),
            },
        )
    });
    if let Err(e) = api::patch::<_, ()>(
        &format!("/vaults/{vault}/members/{user_id}"),
        &UpdateMemberRequest { role, log_entry },
    )
    .await
    {
        error(e.message);
    }
    state::reload_log(vault).await;
}

/// Remove a member, with the vault's new key for everyone else in the same
/// request, so they can't read anything written afterwards.
pub async fn remove_member(vault: &str, user_id: &str) {
    for attempt in 0..2 {
        let mut r = match prepare_rotation(vault, Some(user_id)).await {
            Ok(r) => r,
            Err(e) => {
                error(e);
                return;
            }
        };
        let req = RemoveMemberRequest {
            log_entry: r.removal.take(),
            rotation: Some(r.req.clone()),
        };
        match api::delete_with(&format!("/vaults/{vault}/members/{user_id}"), &req).await {
            Ok(()) => {
                rotated(vault, r).await;
                success("Member removed and vault key rotated.");
                return;
            }
            // Membership, notes or the log changed meanwhile: retry once.
            Err(e) if e.status == 409 && attempt == 0 => {}
            Err(e) => {
                error(e.message);
                return;
            }
        }
    }
    error("Could not remove the member: the vault keeps changing; please try again.");
}

pub async fn rotate_keys(vault: &str) -> bool {
    for attempt in 0..2 {
        let r = match prepare_rotation(vault, None).await {
            Ok(r) => r,
            Err(e) => {
                error(e);
                return false;
            }
        };
        match api::post::<_, ()>(&format!("/vaults/{vault}/keys/rotate"), &r.req).await {
            Ok(()) => {
                rotated(vault, r).await;
                return true;
            }
            // Membership or notes changed meanwhile: retry once with fresh keys.
            Err(e) if e.status == 409 && attempt == 0 => {}
            Err(e) => {
                error(format!("Key rotation failed: {e}"));
                return false;
            }
        }
    }
    error("Key rotation failed: the vault keeps changing; please try again.");
    false
}

/// Owner: rotate the keys someone who no longer has access still knows
/// (they left, their agent was deleted, a note was removed from their
/// list): the server marks them, and this renews them.
pub async fn renew_pending_keys(vault: &str) {
    // Already on it (several events can ask at once).
    if !step(|c| c.renewing_keys.insert(vault.to_string())) {
        return;
    }
    if let Err(e) = renew_pending(vault).await {
        error(format!(
            "Could not renew a key someone who left still knows ({e}); it is tried again next time."
        ));
    }
    step(|c| c.renewing_keys.remove(vault));
}

async fn renew_pending(vault: &str) -> Result<(), String> {
    for attempt in 0..2 {
        let info: VaultInfo = api::get(&format!("/vaults/{vault}"))
            .await
            .map_err(|e| e.message)?;
        if info.rotation_pending {
            // Every note's key moves on with the vault's.
            let r = prepare_rotation(vault, None).await?;
            match api::post::<_, ()>(&format!("/vaults/{vault}/keys/rotate"), &r.req).await {
                Ok(()) => {
                    rotated(vault, r).await;
                    return Ok(());
                }
                // Changed meanwhile (perhaps another app of ours rotated):
                // look again.
                Err(e) if e.status == 409 && attempt == 0 => continue,
                Err(e) => return Err(e.message),
            }
        }
        if !info.notes_rotation_pending.is_empty() {
            state::reload_vault_keys(vault.to_string()).await;
        }
        for note in &info.notes_rotation_pending {
            rotate_note_key(vault, note).await?;
        }
        return Ok(());
    }
    Ok(())
}

/// A new vault key, prepared for the server ([`prepare_rotation`]).
struct Rotation {
    req: RotateKeysRequest,
    /// The `Remove` entry signed before the new key's, when removing someone.
    removal: Option<LogEntry>,
    ring: notekpr_core::keys::VaultKeyring,
    rings: HashMap<String, notekpr_core::keys::VaultKeyring>,
    not_given: BTreeSet<String>,
}

/// The vault's new key and every note's, for the members but `removing`
/// (with the removal's log entry, signed before the new key's).
async fn prepare_rotation(vault: &str, removing: Option<&str>) -> Result<Rotation, String> {
    // The rotation must cover exactly the notes the server has keys for.
    state::reload_vault_keys(vault.to_string()).await;
    state::reload_log(vault).await;
    let members: Vec<MemberInfo> = api::get::<Vec<MemberInfo>>(&format!("/vaults/{vault}/members"))
        .await
        .map_err(|e| e.message)?
        .into_iter()
        .filter(|m| Some(m.user.id.as_str()) != removing)
        .collect();
    // The new key goes only to members the owner's log vouches for and
    // whose key we still trust. Anyone else — a key that changed (they
    // reset their account, or someone swaps keys) or someone the owner
    // never added — is left out on purpose and gets it once verified.
    let distrusted: BTreeSet<String> = members
        .iter()
        .filter(|m| m.user.account_key.is_some() && check_trust(&m.user).is_err())
        .map(|m| m.user.id.clone())
        .collect();
    let (members, left_out): (Vec<MemberInfo>, Vec<MemberInfo>) = step(|c| {
        members.into_iter().partition(|m| {
            m.user.account_key.is_none()
                || (state::vouched(c, vault, m) && !distrusted.contains(&m.user.id))
        })
    });
    let mut not_given: BTreeSet<String> = left_out
        .iter()
        .map(|m| m.user.display_name.clone())
        .collect();
    // Removed members knew every note's key too: they all move on, and
    // shared notes' new keys go to their current recipients.
    let (keyed, shared) = step(|c| {
        let Some(r) = c.replicas.get(vault) else {
            return (vec![], BTreeSet::new());
        };
        let o = c.outboxes.get(vault);
        let keyed: Vec<String> = r
            .doc_keys
            .keys()
            .filter(|n| !o.is_some_and(|o| o.waits_for_key(n)))
            .cloned()
            .collect();
        (keyed, r.shared.clone())
    });
    let mut recipients: HashMap<String, Vec<PublicUser>> = HashMap::new();
    let mut recipients_out: HashMap<String, Vec<String>> = HashMap::new();
    for n in keyed.iter().filter(|n| shared.contains(*n)) {
        let users: Vec<PublicUser> =
            match api::get::<Vec<ObjectShareInfo>>(&format!("/vaults/{vault}/objects/{n}/shares"))
                .await
            {
                Ok(s) => s.into_iter().map(|s| s.user).collect(),
                Err(e) if e.status == 404 => vec![],
                Err(e) => return Err(e.message),
            };
        // Never wrap a new key to a key that changed since it was
        // pinned: such people are left out of the note's new key.
        let (users, changed): (Vec<PublicUser>, Vec<PublicUser>) = users
            .into_iter()
            .partition(|u| u.account_key.is_none() || check_trust(u).is_ok());
        not_given.extend(changed.iter().map(|u| u.display_name.clone()));
        recipients_out.insert(n.clone(), changed.into_iter().map(|u| u.id).collect());
        recipients.insert(n.clone(), users);
    }
    let prepared = step(|c| {
        let r = c.replicas.get(vault)?;
        let notes: Vec<_> = keyed
            .iter()
            .filter_map(|n| {
                let meta = r.index.get(n).map(|e| ObjectMeta::of(&e));
                Some((
                    n.clone(),
                    r.doc_keys.get(n)?,
                    meta.unwrap_or_default(),
                    recipients.remove(n).unwrap_or_default(),
                ))
            })
            .collect();
        let mut ring = r.keyring.clone();
        let made = engine::rotate_with_notes(&mut ring, &members, &r.meta, &notes);
        Some(made.map(|(mut req, rings)| {
            let removal = removing.and_then(|u| {
                state::log_entry(
                    c,
                    vault,
                    LogBody::Remove {
                        user_id: u.to_string(),
                    },
                )
            });
            req.log_entry = notekpr_sync::members::rotation(vault, &ring)
                .and_then(|body| state::log_entry_after(c, vault, removal.as_ref(), body));
            // Left out on purpose, so the server knows it's no oversight.
            req.without = left_out
                .iter()
                .filter(|m| m.user.account_key.is_some())
                .map(|m| m.user.id.clone())
                .collect();
            for o in &mut req.objects {
                o.update.without = recipients_out.remove(&o.object_id).unwrap_or_default();
            }
            // Every note's new key for the people it is shared with.
            if let Some(a) = &c.account {
                for o in &mut req.objects {
                    if let Some(next) = rings.get(&o.object_id) {
                        notekpr_sync::members::sign_grants(
                            a,
                            vault,
                            &o.object_id,
                            next,
                            &mut o.update.envelopes,
                        );
                    }
                }
            }
            Rotation {
                req,
                removal,
                ring,
                rings,
                not_given,
            }
        }))
    });
    match prepared {
        Some(Ok(r)) => Ok(r),
        Some(Err(e)) => Err(e.to_string()),
        None => Err("This vault isn't loaded yet; please try again.".into()),
    }
}

/// The server took a rotation: keep its keys.
async fn rotated(vault: &str, r: Rotation) {
    let Rotation {
        ring,
        rings,
        not_given,
        ..
    } = r;
    step(|c| {
        let Some(r) = c.replicas.get_mut(vault) else {
            return;
        };
        r.keyring = ring;
        r.doc_keys.extend(rings);
        // New notes' keys not registered yet were wrapped under
        // the old vault key: new ones replace them.
        if let Some(o) = c.outboxes.get_mut(vault) {
            let waiting: Vec<String> = o.new_keys.iter().map(|k| k.object_id.clone()).collect();
            for n in waiting {
                o.key_refused(r, &n);
            }
        }
        state::persist_new_keys(c, vault, &[]);
    });
    state::reload_log(vault).await;
    if !not_given.is_empty() {
        info(format!(
            "The new key went to everyone except {}: their encryption key changed (or the vault's owner never added them). Compare fingerprints with them, then press “I verified it” in People & sharing to give them access again.",
            not_given.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
}

/// Owner: give keys to members who were added before they had any.
pub async fn grant_missing_keys(vault: &str) {
    let Ok(members) = api::get::<Vec<MemberInfo>>(&format!("/vaults/{vault}/members")).await else {
        return;
    };
    let trusted: Vec<MemberInfo> = members
        .into_iter()
        .filter(|m| m.user.account_key.is_none() || check_trust(&m.user).is_ok())
        .collect();
    // Added before they had keys, or their key changed and we verified the
    // new one since: pin their keys in the log first.
    let to_pin: Vec<MemberInfo> = step(|c| {
        let log = state::member_log(c, vault);
        let changed = trusted.iter().filter(|m| log.key_changed(m));
        log.to_pin(&trusted)
            .into_iter()
            .chain(changed)
            .filter(|m| !m.has_current_key)
            .cloned()
            .collect()
    });
    for m in &to_pin {
        let Some(Ok(key)) = m.user.account_key.as_ref().map(|k| k.verify()) else {
            continue;
        };
        if let Err(e) = state::pin_member_key(vault, &m.user.id, m.role.clone(), &key).await {
            util::warn(&format!("could not pin a member's key: {e}"));
        }
    }
    // Only members the owner's log vouches for get the vault key.
    let env = step(|c| {
        let trusted: Vec<MemberInfo> = trusted
            .into_iter()
            .filter(|m| state::vouched(c, vault, m))
            .collect();
        c.replicas
            .get(vault)
            .map(|r| engine::missing_grants(&r.keyring, &trusted))
    })
    .unwrap_or_default();
    if !env.is_empty()
        && api::post::<_, ()>(
            &format!("/vaults/{vault}/keys/grant"),
            &GrantKeysRequest { envelopes: env },
        )
        .await
        .is_ok()
    {
        info("Shared vault keys with new members.");
    }
    // People single notes are shared with who were left without a note's
    // current key (their key changed): once verified, sharing it again
    // gives it to them.
    let shared: Vec<String> = step(|c| {
        c.replicas
            .get(vault)
            .map(|r| r.shared.iter().cloned().collect())
            .unwrap_or_default()
    });
    for note in shared {
        let Ok(shares) =
            api::get::<Vec<ObjectShareInfo>>(&format!("/vaults/{vault}/objects/{note}/shares"))
                .await
        else {
            continue;
        };
        for s in shares {
            if !s.has_current_key
                && s.user.account_key.is_some()
                && check_trust(&s.user).is_ok()
                && let Err(e) = share_note(vault, &note, &s.user, s.role, false).await
            {
                util::warn(&format!("could not give a note's key again: {e}"));
            }
        }
    }
}

// ------------------------------------------------------------------ sharing single notes

/// Share one note (not the vault) with someone. The note first gets its own
/// key; the person receives only that key.
/// Share a note with someone. `with_comments`: its comments written so far
/// go with it — to everyone it is shared with (docs/comments.md §5).
pub async fn share_note(
    vault: &str,
    note: &str,
    user: &PublicUser,
    role: Role,
    with_comments: bool,
) -> Result<(), String> {
    if user.account_key.is_none() {
        return Err(format!(
            "{} hasn't set up encryption yet. Ask them to sign in once, then share again.",
            user.display_name
        ));
    }
    check_trust(user)?;
    ensure_note_key(vault, note).await?;
    let env = step(|c| {
        let ring = c.replicas.get(vault)?.doc_keys.get(note)?;
        let account = c.account.as_ref()?;
        // Signed by us, so they can tell the key is really ours.
        Some(engine::object_share_envelope(ring, note, user).map(|env| {
            let mut env = [env];
            notekpr_sync::members::sign_grants(account, vault, note, ring, &mut env);
            let [env] = env;
            env
        }))
    });
    let envelope = match env {
        Some(Ok(e)) => e,
        Some(Err(e)) => return Err(e.to_string()),
        None => return Err("The note's key is not available.".into()),
    };
    let req = AddObjectShareRequest {
        user_id: user.id.clone(),
        log_entry: share_entry(vault, note, user, &role),
        role: role.clone(),
        envelope,
    };
    match api::post::<_, ObjectShareInfo>(&format!("/vaults/{vault}/objects/{note}/shares"), &req)
        .await
    {
        Ok(_) => {}
        Err(e) if e.status == 409 && e.message.contains("already shared") => {
            api::patch::<_, ()>(
                &format!("/vaults/{vault}/objects/{note}/shares/{}", user.id),
                &UpdateMemberRequest {
                    log_entry: share_entry(vault, note, user, &role),
                    role,
                },
            )
            .await
            .map_err(|e| e.message)?;
        }
        Err(e) if e.status == 409 && e.message.contains("through the vault") => {
            return Err(format!(
                "{} is a member of this vault and can already open the note.",
                user.display_name
            ));
        }
        // The key changed meanwhile (someone removed a person): reload and let the user retry.
        Err(e) if e.status == 412 => {
            state::reload_vault_keys(vault.to_string()).await;
            return Err("The note's key just changed. Please try again.".into());
        }
        Err(e) => return Err(e.message),
    }
    step(|c| {
        c.users.insert(user.id.clone(), user.clone());
        if let Some(r) = c.replicas.get_mut(vault) {
            r.shared.insert(note.to_string());
        }
    });
    state::reload_log(vault).await;
    if with_comments {
        // Where sharing started comes with the vault's keys.
        state::reload_vault_keys(vault.to_string()).await;
        step(|c| {
            let sent = c
                .replicas
                .get_mut(vault)
                .and_then(|r| r.carry_comments(note).ok())
                .is_some_and(|p| !p.is_empty());
            if sent {
                crate::cache::note_local(c, vault, &comments_object(note));
                state::schedule_flush(c);
            }
        });
    }
    let title = step(|c| {
        state::send_presence(c);
        c.note_title(vault, note).unwrap_or_default()
    });
    state::ui().tree_rev.update(|v| *v += 1);
    state::ui().shares_rev.update(|v| *v += 1);
    success(format!("Shared “{title}” with {}.", user.display_name));
    Ok(())
}

/// Give a note its own document key (once), re-encrypting its current
/// content under it so people it is shared with can read it.
async fn ensure_note_key(vault: &str, note: &str) -> Result<(), String> {
    for attempt in 0..3 {
        if step(|c| state::is_shared_note(c, vault, note)) {
            return Ok(());
        }
        // The new key's first snapshot must cover every stored change of the
        // note, including our own: send them and wait for the server.
        step(state::flush_now);
        for _ in 0..50 {
            if !step(|c| {
                c.replicas
                    .get(vault)
                    .is_some_and(|r| r.pending.iter().any(|p| p.object_id == note))
            }) {
                break;
            }
            util::sleep(100).await;
        }
        let made = step(|c| {
            let r = c.replicas.get(vault)?;
            let meta = r.index.get(note).map(|e| ObjectMeta::of(&e));
            let meta = meta.unwrap_or_default();
            let st = r.note(note).map(|n| n.encode_state()).unwrap_or_default();
            let waiting = c.outboxes.get(vault).is_some_and(|o| o.waits_for_key(note));
            let made = match r.doc_keys.get(note) {
                // Its own key already: a fresh one without a link to it, so
                // recipients never read what was written before.
                Some(doc) if !waiting => {
                    engine::start_sharing_keyed(&r.keyring, doc, note, &meta, &st, r.seq)
                }
                _ => engine::start_sharing(&r.keyring, note, &meta, &st, r.seq),
            };
            Some(made.map(|(ring, mut req)| {
                if let Some(a) = &c.account {
                    notekpr_sync::members::sign_key_update(a, vault, note, &mut req);
                }
                (ring, req)
            }))
        });
        let (ring, req) = match made {
            Some(Ok(x)) => x,
            Some(Err(e)) => return Err(e.to_string()),
            None => return Err("vault not loaded".into()),
        };
        match api::put::<_, ()>(&format!("/vaults/{vault}/objects/{note}/key"), &req).await {
            Ok(()) => {
                step(|c| {
                    if let Some(r) = c.replicas.get_mut(vault) {
                        r.doc_keys.insert(note.to_string(), ring);
                    }
                    // A new note's own key needs no registering any more.
                    if let Some(o) = c.outboxes.get_mut(vault) {
                        o.key_registered(note);
                    }
                    state::persist_new_keys(c, vault, &[]);
                });
                return Ok(());
            }
            // Someone else started sharing it, or new changes arrived: reload and retry.
            Err(e) if (e.status == 409 || e.status == 412) && attempt < 2 => {
                state::reload_vault_keys(vault.to_string()).await;
                util::sleep(300).await;
            }
            Err(e) => return Err(e.message),
        }
    }
    Err("Could not prepare the note for sharing; please try again.".into())
}

/// Stop sharing a note with someone, with the note's new key for everyone
/// else in the same request, so they can't read what is written from now on.
pub async fn unshare_note(vault: &str, note: &str, user_id: &str) -> Result<(), String> {
    for attempt in 0..2 {
        let log_entry = step(|c| {
            state::log_entry(
                c,
                vault,
                LogBody::Unshare {
                    object_id: note.to_string(),
                    user_id: user_id.to_string(),
                },
            )
        });
        let r = note_rotation(vault, note, Some(user_id)).await?;
        let req = RemoveShareRequest {
            log_entry,
            rotation: Some(r.req.clone()),
        };
        match api::delete_with(
            &format!("/vaults/{vault}/objects/{note}/shares/{user_id}"),
            &req,
        )
        .await
        {
            Ok(()) => {
                state::reload_log(vault).await;
                r.store(vault, note);
                state::ui().shares_rev.update(|v| *v += 1);
                return Ok(());
            }
            // Recipients, keys or the log changed meanwhile: retry once.
            Err(e) if (e.status == 409 || e.status == 412) && attempt == 0 => {
                state::reload_vault_keys(vault.to_string()).await;
                state::reload_log(vault).await;
            }
            Err(e) => return Err(e.message),
        }
    }
    Err("The note's recipients keep changing; please try again.".into())
}

/// The log entry for sharing a note with `user` (their key pinned), if the
/// vault has a log.
fn share_entry(vault: &str, note: &str, user: &PublicUser, role: &Role) -> Option<LogEntry> {
    let key = user.account_key.as_ref()?.verify().ok()?;
    step(|c| {
        state::log_entry(
            c,
            vault,
            notekpr_sync::members::share(note, &user.id, role.clone(), &key),
        )
    })
}

pub async fn set_note_share_role(
    vault: &str,
    note: &str,
    user_id: &str,
    role: Role,
) -> Result<(), String> {
    let user = match step(|c| c.users.get(user_id).cloned()) {
        Some(u) => u,
        None => api::get::<PublicUser>(&format!("/users/{user_id}"))
            .await
            .map_err(|e| e.message)?,
    };
    check_trust(&user)?;
    let r = api::patch::<_, ()>(
        &format!("/vaults/{vault}/objects/{note}/shares/{user_id}"),
        &UpdateMemberRequest {
            log_entry: share_entry(vault, note, &user, &role),
            role,
        },
    )
    .await
    .map_err(|e| e.message);
    state::reload_log(vault).await;
    r
}

/// New document key for a shared note, for its current recipients.
pub async fn rotate_note_key(vault: &str, note: &str) -> Result<(), String> {
    for attempt in 0..2 {
        let r = note_rotation(vault, note, None).await?;
        match api::put::<_, ()>(&format!("/vaults/{vault}/objects/{note}/key"), &r.req).await {
            Ok(()) => {
                r.store(vault, note);
                return Ok(());
            }
            // Recipients or keys changed meanwhile: reload and retry once.
            Err(e) if (e.status == 409 || e.status == 412) && attempt == 0 => {
                state::reload_vault_keys(vault.to_string()).await
            }
            Err(e) => return Err(e.message),
        }
    }
    Err("The note's recipients keep changing; please try again.".into())
}

/// A note's new key, prepared for the server ([`note_rotation`]).
struct NoteRotation {
    next: notekpr_core::keys::VaultKeyring,
    req: ObjectKeyUpdate,
    /// Whether anyone keeps the note (or is left out of its key for now).
    shared: bool,
}

impl NoteRotation {
    /// The server took it: keep the key.
    fn store(self, vault: &str, note: &str) {
        step(|c| {
            if let Some(r) = c.replicas.get_mut(vault) {
                r.doc_keys.insert(note.to_string(), self.next);
                if !self.shared {
                    r.shared.remove(note);
                }
            }
        });
        state::ui().tree_rev.update(|v| *v += 1);
    }
}

/// The note's new key for its recipients but `removing`. Never wrapped to a
/// key that changed since it was pinned: such people are left out of it
/// until verified (sharing the note with them again gives it).
async fn note_rotation(
    vault: &str,
    note: &str,
    removing: Option<&str>,
) -> Result<NoteRotation, String> {
    let shares: Vec<ObjectShareInfo> = api::get(&format!("/vaults/{vault}/objects/{note}/shares"))
        .await
        .map_err(|e| e.message)?;
    let (users, changed): (Vec<PublicUser>, Vec<PublicUser>) = shares
        .into_iter()
        .map(|s| s.user)
        .filter(|u| Some(u.id.as_str()) != removing)
        .partition(|u| u.account_key.is_none() || check_trust(u).is_ok());
    let made = step(|c| {
        let r = c.replicas.get(vault)?;
        let ring = r.doc_keys.get(note)?;
        let meta = r.index.get(note).map(|e| ObjectMeta::of(&e));
        let meta = meta.unwrap_or_default();
        let made = engine::rotate_object_key(&r.keyring, ring, note, &meta, &users);
        Some(made.map(|(next, mut req)| {
            if let Some(a) = &c.account {
                notekpr_sync::members::sign_grants(a, vault, note, &next, &mut req.envelopes);
            }
            (next, req)
        }))
    });
    let (next, mut req) = match made {
        Some(Ok(x)) => x,
        Some(Err(e)) => return Err(e.to_string()),
        None => return Err("This note's key isn't loaded yet; please try again.".into()),
    };
    req.without = changed.iter().map(|u| u.id.clone()).collect();
    if !changed.is_empty() {
        info(format!(
            "The note's new key went to everyone except {}: their encryption key changed. Compare fingerprints with them, then share the note with them again.",
            changed
                .iter()
                .map(|u| u.display_name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(NoteRotation {
        next,
        req,
        shared: !users.is_empty() || !changed.is_empty(),
    })
}

/// Shared notes carry their title and icon encrypted for their recipients.
async fn update_note_meta(vault: String, note: String) {
    for _ in 0..2 {
        let ct = step(|c| {
            let r = c.replicas.get(&vault)?;
            let ring = r.doc_keys.get(&note)?;
            let meta = ObjectMeta::of(&r.index.get(&note)?);
            meta.encrypt(ring, &note).ok()
        });
        let Some(encrypted_meta) = ct else { return };
        match api::put::<_, ()>(
            &format!("/vaults/{vault}/objects/{note}/meta"),
            &ObjectMetaUpdate { encrypted_meta },
        )
        .await
        {
            Err(e) if e.status == 412 => state::reload_vault_keys(vault.clone()).await,
            _ => return,
        }
    }
}

/// Remove a note someone shared with us from our list.
pub async fn leave_shared_note(vault: &str, note: &str) {
    let uid = step(|c| c.user_id.clone());
    match api::delete(&format!("/vaults/{vault}/objects/{note}/shares/{uid}")).await {
        Ok(()) | Err(api::Error { status: 404, .. }) => {
            state::navigate(Route::Home);
            state::refresh_shares().await;
        }
        Err(e) => error(e.message),
    }
}

// ------------------------------------------------------------------ notes & folders

fn unique_path(c: &Core, vault: &str, path: &str) -> String {
    let taken: Vec<String> = c
        .live_entries(vault)
        .into_iter()
        .map(|e| e.path.to_lowercase())
        .collect();
    paths::unique(path, |p| taken.contains(&p.to_lowercase()))
}

/// Change the vault's file tree with `f` and send the change; false if
/// nothing changed.
fn change_index(c: &mut Core, vault: &str, f: impl FnOnce(&IndexDoc) -> Option<Vec<u8>>) -> bool {
    let Some(u) = c.replicas.get(vault).and_then(|r| f(&r.index)) else {
        return false;
    };
    local_update(c, vault, INDEX_OBJECT_ID, u);
    true
}

pub async fn create_note_with(
    vault: &str,
    folder: &str,
    title: &str,
    text: &str,
) -> Option<String> {
    if !state::step(|c| c.can_write(vault)) {
        error("You can only read this vault.");
        return None;
    }
    // Quick notes are flat, and hold a limited number of notes.
    if state::step(|c| c.is_quick_notes(vault)) {
        if !folder.trim_matches('/').is_empty() {
            error("Quick notes have no folders: move the note to a vault first.");
            return None;
        }
        if !crate::quick::has_room() {
            error(notekpr_core::quick::full_message(crate::quick::limit()));
            return None;
        }
    }
    let title = if title.trim().is_empty() {
        "Untitled"
    } else {
        title.trim()
    };
    let safe: String = title
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let path = match paths::normalize_note(&paths::join(folder, &safe)) {
        Ok(p) => p,
        Err(e) => {
            error(e.to_string());
            return None;
        }
    };
    let id = step(|c| new_note_at(c, vault, &path, text));
    state::ui().tree_rev.update(|v| *v += 1);
    Some(id)
}

/// A new note at `path` (or a free name beside it) with `text`: its id.
fn new_note_at(c: &mut Core, vault: &str, path: &str, text: &str) -> String {
    let id = state::new_id();
    let path = unique_path(c, vault, path);
    // Its own key, registered before any of its content is sent.
    state::new_note_key(c, vault, &id, paths::title(&path));
    change_index(c, vault, |ix| ix.create(&id, &path, EntryKind::Note, now()));
    c.ensure_note_loaded(vault, &id);
    if !text.is_empty() {
        // Large texts become several updates, each within the server's limit.
        state::set_note_text(c, vault, &id, text);
    }
    id
}

/// Makes a drawing at `path` (or a free name beside it); its id.
fn new_drawing_at(
    c: &mut Core,
    vault: &str,
    path: &str,
    scene: &notekpr_draw::scene::Scene,
) -> String {
    let id = state::new_id();
    let path = unique_path(c, vault, path);
    // Its own key, registered before any of its content is sent.
    state::new_note_key(c, vault, &id, paths::drawing_title(&path));
    change_index(c, vault, |ix| {
        ix.create(&id, &path, EntryKind::Drawing, now())
    });
    c.ensure_note_loaded(vault, &id);
    state::set_drawing(c, vault, &id, scene);
    id
}

pub fn new_note(vault: &str, folder: &str) {
    let (v, f) = (vault.to_string(), folder.to_string());
    leptos::task::spawn_local(async move {
        if let Some(id) = create_note_with(&v, &f, "Untitled", "").await {
            navigate(Route::Note(v, id));
            crate::views::note::focus_title_soon();
        }
    });
}

/// Set a note's or folder's icon ("" removes it). A folder that only
/// exists as the parent of others (`id` `None`) gets an entry for it first.
/// `false` if nothing changed.
pub fn set_entry_icon(vault: &str, id: Option<&str>, path: &str, icon: &str) -> bool {
    let icon = match notekpr_core::doc::clean_icon(icon) {
        Ok(i) => i,
        Err(e) => {
            error(e);
            return false;
        }
    };
    let (changed, shared) = step(|c| {
        if id.is_none() && icon.is_none() {
            return (false, None);
        }
        let id = folder_entry(c, vault, path, id);
        let changed = change_index(c, vault, |ix| ix.set_icon(&id, icon.as_deref()));
        (changed, state::is_shared_note(c, vault, &id).then_some(id))
    });
    // People the note is shared with see its icon too.
    if let Some(note) = shared {
        leptos::task::spawn_local(update_note_meta(vault.to_string(), note));
    }
    if changed {
        state::ui().tree_rev.update(|v| *v += 1);
    }
    changed
}

/// The id of the entry at `path`, else (a folder that only exists as the
/// parent of others) of a new folder entry for it.
fn folder_entry(c: &mut Core, vault: &str, path: &str, id: Option<&str>) -> String {
    if let Some(id) = id {
        return id.to_string();
    }
    let id = state::new_id();
    change_index(c, vault, |ix| {
        ix.create(&id, path, EntryKind::Folder, now())
    });
    id
}

pub fn create_folder(vault: &str, path: &str) {
    let Ok(p) = paths::normalize(path) else {
        error("Invalid folder name");
        return;
    };
    step(|c| new_folder_at(c, vault, &p));
    state::ui().tree_rev.update(|v| *v += 1);
}

/// A new folder at `path` (or a free name beside it): its id.
fn new_folder_at(c: &mut Core, vault: &str, path: &str) -> String {
    let id = state::new_id();
    let path = unique_path(c, vault, path);
    change_index(c, vault, |ix| {
        ix.create(&id, &path, EntryKind::Folder, now())
    });
    id
}

/// Rename a note, attachment or explicit folder. Wiki links pointing to a
/// renamed note are updated in every note of the vault.
pub fn rename_entry(vault: &str, id: &str, new_name: &str) -> Result<(), String> {
    let e = state::step(|c| c.entry(vault, id)).ok_or("not found")?;
    let new_name = new_name.trim();
    if new_name.is_empty() || new_name.contains('/') {
        return Err("Names can't be empty or contain '/'.".into());
    }
    let parent = paths::parent(&e.path).to_string();
    let mut new_path = paths::join(&parent, new_name);
    if e.kind == EntryKind::Note {
        new_path = paths::normalize_note(&new_path).map_err(|e| e.to_string())?;
    } else {
        new_path = paths::normalize(&new_path).map_err(|e| e.to_string())?;
    }
    if new_path == e.path {
        return Ok(());
    }
    if state::step(|c| c.live_entries(vault))
        .iter()
        .any(|x| x.id != id && x.path.eq_ignore_ascii_case(&new_path))
    {
        return Err("Something with that name already exists here.".into());
    }
    if e.kind == EntryKind::Folder {
        step(|c| change_index(c, vault, |ix| ix.rename_folder(&e.path, &new_path, now())));
    } else {
        let old_title = e.title().to_string();
        let old_path = e.path.clone();
        let shared = step(|c| {
            change_index(c, vault, |ix| ix.rename(id, &new_path, now()));
            state::is_shared_note(c, vault, id)
        });
        if shared && paths::title(&new_path) != old_title {
            leptos::task::spawn_local(update_note_meta(vault.to_string(), id.to_string()));
        }
        if e.kind == EntryKind::Note && paths::title(&new_path) != old_title {
            update_links_after_rename(vault, id, &old_path, paths::title(&new_path));
        }
    }
    state::ui().tree_rev.update(|v| *v += 1);
    Ok(())
}

fn update_links_after_rename(vault: &str, id: &str, old_path: &str, new_title: &str) {
    // Resolve against the old tree so links that used the old name still match.
    let mut cands: Vec<(String, String)> = state::step(|c| c.live_entries(vault))
        .into_iter()
        .map(|e| (e.id, e.path))
        .collect();
    for c in &mut cands {
        if c.0 == id {
            c.1 = old_path.to_string();
        }
    }
    let notes = knowledge::notes_with_text(vault);
    let mut changed = 0;
    for (nid, npath, text) in notes {
        let path = if nid == id {
            old_path.to_string()
        } else {
            npath
        };
        let rewritten = markdown::rewrite_links(&text, new_title, |l| {
            markdown::resolve(l, &path, &cands) == Some(id)
        });
        if let Some(new_text) = rewritten
            && step(|c| state::set_note_text(c, vault, &nid, &new_text)) > 0
        {
            changed += 1;
        }
    }
    if changed > 0 {
        info(format!(
            "Updated links in {changed} note{}.",
            if changed == 1 { "" } else { "s" }
        ));
        refresh_open_editor(vault);
    }
}

fn refresh_open_editor(vault: &str) {
    let open = step(|c| {
        c.editor
            .as_ref()
            .map(|(v, n, e)| (v.clone(), n.clone(), e.clone()))
    });
    if let Some((v, n, ed)) = open
        && v == vault
    {
        let t = step(|c| c.replicas.get(&v).map(|r| r.note_text(&n)));
        if let Some(t) = t {
            ed.set_text(t, false);
        }
    }
}

pub fn move_entry(vault: &str, id: &str, folder: &str) -> Result<(), String> {
    let e = state::step(|c| c.entry(vault, id)).ok_or("not found")?;
    let me = order::Child {
        path: e.path.clone(),
        entry: Some(e),
    };
    if step(|c| move_into(c, vault, &me, folder))?.is_some() {
        state::ui().tree_rev.update(|v| *v += 1);
    }
    Ok(())
}

/// Move `me` into `folder`; its new path, `None` if it is already there.
fn move_into(
    c: &mut Core,
    vault: &str,
    me: &order::Child,
    folder: &str,
) -> Result<Option<String>, String> {
    if paths::parent(&me.path) == folder {
        return Ok(None);
    }
    if me.is_folder() && (folder == me.path || paths::is_within(folder, &me.path)) {
        return Err("A folder can't be moved into itself.".into());
    }
    let target = unique_path(c, vault, &paths::join(folder, paths::file_name(&me.path)));
    change_index(c, vault, |ix| match &me.entry {
        Some(e) if e.kind != EntryKind::Folder => ix.rename(&e.id, &target, now()),
        _ => ix.rename_folder(&me.path, &target, now()),
    });
    Ok(Some(target))
}

/// Where an entry dragged in the file tree lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Spot {
    /// Into a folder ("" = the top of the vault), after what is there.
    Into(String),
    /// Just before or after the entry at this path, in its folder; folders
    /// stay above notes, so only next to one of the same kind.
    Before(String),
    After(String),
}

/// Move the entry at `path` (folders without an entry too) to `spot`. Its
/// place among its new siblings is kept in order keys ([`order`]).
pub fn place_entry(vault: &str, path: &str, spot: &Spot) -> Result<(), String> {
    let (folder, anchor) = match spot {
        Spot::Into(f) => (f.as_str(), None),
        Spot::Before(p) => (paths::parent(p), Some((p.as_str(), 0))),
        Spot::After(p) => (paths::parent(p), Some((p.as_str(), 1))),
    };
    let tree = step(|c| order::tree(&c.live_entries(vault)));
    let me = tree
        .get(paths::parent(path))
        .and_then(|l| l.iter().find(|c| c.path == path))
        .ok_or("not found")?;
    // Its kind in that folder, in order, with and without it.
    let kind: Vec<&order::Child> = tree
        .get(folder)
        .into_iter()
        .flatten()
        .filter(|c| c.is_folder() == me.is_folder())
        .collect();
    let others: Vec<&order::Child> = kind.iter().copied().filter(|c| c.path != path).collect();
    let at = match anchor {
        Some((p, after)) => others.iter().position(|c| c.path == p).ok_or("not found")? + after,
        None => others.len(),
    };
    if kind.iter().position(|c| c.path == path) == Some(at) {
        return Ok(());
    }
    step(|c| -> Result<(), String> {
        let moved = move_into(c, vault, me, folder)?;
        if anchor.is_none() {
            return Ok(());
        }
        let keys: Vec<Option<&str>> = others.iter().map(|c| c.order()).collect();
        let place = order::place(&keys, at);
        let new_path = moved.as_deref().unwrap_or(path);
        let my_id = me.entry.as_ref().map(|e| e.id.as_str());
        let mut writes = vec![(folder_entry(c, vault, new_path, my_id), place.key)];
        for (i, key) in place.others {
            let o = others[i];
            let id = folder_entry(c, vault, &o.path, o.entry.as_ref().map(|e| e.id.as_str()));
            writes.push((id, key));
        }
        change_index(c, vault, |ix| {
            ix.set_order(writes.iter().map(|(id, k)| (id.as_str(), Some(k.as_str()))))
        });
        Ok(())
    })?;
    state::ui().tree_rev.update(|v| *v += 1);
    Ok(())
}

/// Whether the entries directly in `folder` were put in order by hand.
pub fn has_custom_order(vault: &str, folder: &str) -> bool {
    step(|c| c.live_entries(vault))
        .iter()
        .any(|e| e.order.is_some() && paths::parent(&e.path) == folder)
}

/// Put the entries directly in `folder` back in A–Z order.
pub fn sort_folder(vault: &str, folder: &str) {
    let changed = step(|c| {
        let ids: Vec<String> = c
            .live_entries(vault)
            .into_iter()
            .filter(|e| e.order.is_some() && paths::parent(&e.path) == folder)
            .map(|e| e.id)
            .collect();
        change_index(c, vault, |ix| {
            ix.set_order(ids.iter().map(|id| (id.as_str(), None)))
        })
    });
    if changed {
        state::ui().tree_rev.update(|v| *v += 1);
    }
}

/// Move a folder that only exists implicitly (no folder entry).
pub fn rename_implicit_folder(vault: &str, old: &str, new_name: &str) -> Result<(), String> {
    let new_path =
        paths::normalize(&paths::join(paths::parent(old), new_name)).map_err(|e| e.to_string())?;
    if step(|c| change_index(c, vault, |ix| ix.rename_folder(old, &new_path, now()))) {
        state::ui().tree_rev.update(|v| *v += 1);
    }
    Ok(())
}

pub fn trash(vault: &str, id: &str) {
    if step(|c| change_index(c, vault, |ix| ix.trash(id, now()))) {
        state::ui().tree_rev.update(|v| *v += 1);
        info("Moved to trash.");
    }
}

pub fn trash_folder(vault: &str, folder: &str) {
    if step(|c| change_index(c, vault, |ix| ix.trash_folder(folder, now()))) {
        state::ui().tree_rev.update(|v| *v += 1);
        info("Folder moved to trash.");
    }
}

pub fn restore(vault: &str, id: &str) {
    if step(|c| change_index(c, vault, |ix| ix.restore(id))) {
        state::ui().tree_rev.update(|v| *v += 1);
        success("Restored.");
    }
}

/// Move an entry to the trash without saying so (the caller does).
pub fn trash_quietly(vault: &str, id: &str) {
    if step(|c| change_index(c, vault, |ix| ix.trash(id, now()))) {
        state::ui().tree_rev.update(|v| *v += 1);
    }
}

/// Delete an entry for good on the server, then in the index: marked
/// while the server keeps it (it can be undone, [`undo_purge`]), removed if
/// nothing of it is there (a folder, or gone already).
async fn purge_entry(vault: &str, id: &str) -> Result<(), api::Error> {
    let entry = step(|c| c.replicas.get(vault).and_then(|r| r.index.get(id)));
    if entry.as_ref().is_some_and(|e| e.purged.is_some()) {
        return Ok(());
    }
    let r = match entry.map(|e| e.kind) {
        Some(EntryKind::Attachment) => api::delete(&format!("/vaults/{vault}/attachments/{id}"))
            .await
            .map(|()| true),
        Some(EntryKind::Note) => api::delete(&format!("/vaults/{vault}/objects/{id}"))
            .await
            .map(|()| true),
        _ => Ok(false),
    };
    match r {
        Ok(true) => step(|c| change_index(c, vault, |ix| ix.mark_purged([id], now()))),
        Ok(false) | Err(api::Error { status: 404, .. }) => {
            step(|c| change_index(c, vault, |ix| ix.remove(id)))
        }
        Err(e) => return Err(e),
    };
    state::ui().tree_rev.update(|v| *v += 1);
    Ok(())
}

/// Like [`purge`], but offline (or with the server away) it just waits:
/// `false`, nothing said.
pub async fn purge_quietly(vault: &str, id: &str) -> bool {
    match purge_entry(vault, id).await {
        Ok(()) => true,
        Err(e) if e.is_offline() => false,
        Err(e) => {
            error(e.message);
            false
        }
    }
}

/// How long deleting something for good can be undone, as said to people
/// ("30 days"), if it can.
pub fn purge_undo_period() -> Option<String> {
    match state::ui()
        .server
        .with_untracked(|s| s.as_ref().map(|s| s.purge_undo_days))
        .unwrap_or(0)
    {
        0 => None,
        1 => Some("1 day".into()),
        n => Some(format!("{n} days")),
    }
}

/// Undo deleting an entry for good, while the server still keeps it (the
/// vault's owner and admins): it is back in the trash.
pub async fn undo_purge(vault: &str, id: &str) -> bool {
    let kind = step(|c| c.replicas.get(vault).and_then(|r| r.index.get(id)))
        .filter(|e| e.purged.is_some())
        .map(|e| e.kind);
    let path = match kind {
        Some(EntryKind::Attachment) => format!("/vaults/{vault}/attachments/{id}/restore"),
        Some(EntryKind::Note) => format!("/vaults/{vault}/objects/{id}/restore"),
        _ => return false,
    };
    match api::post_empty::<()>(&path).await {
        Ok(()) => {
            step(|c| change_index(c, vault, |ix| ix.unmark_purged(id)));
            state::ui().tree_rev.update(|v| *v += 1);
            success("Back in the trash.");
            true
        }
        Err(e) if e.status == 404 => {
            error("This can't be undone any more: it is gone for good.");
            false
        }
        Err(e) => {
            error(e.message);
            false
        }
    }
}

/// Entries deleted for good stay in the index (marked) while the server
/// keeps their data; once their undo period is over and the server let go
/// of it, the vault's owner or an admin drops them. Called once a vault is
/// caught up; asks the server only when there are such entries.
pub async fn forget_erased(vault: &str) {
    let days = state::ui()
        .server
        .with_untracked(|s| s.as_ref().map(|s| s.purge_undo_days))
        .unwrap_or(0) as i64;
    let over = now() - days * 24 * 3600 * 1000;
    let due: Vec<String> = step(|c| {
        if !c.can_administer(vault) {
            return vec![];
        }
        c.replicas
            .get(vault)
            .map(|r| {
                r.index
                    .entries()
                    .into_iter()
                    .filter(|e| e.purged.is_some_and(|at| at <= over))
                    .map(|e| e.id)
                    .collect()
            })
            .unwrap_or_default()
    });
    if due.is_empty() {
        return;
    }
    let Ok(kept) = api::get::<Vec<PurgedItem>>(&format!("/vaults/{vault}/purged")).await else {
        return;
    };
    let gone: Vec<String> = due
        .into_iter()
        .filter(|id| !kept.iter().any(|k| &k.id == id))
        .collect();
    if gone.is_empty() {
        return;
    }
    step(|c| {
        for id in &gone {
            change_index(c, vault, |ix| ix.remove(id));
        }
    });
    state::ui().tree_rev.update(|v| *v += 1);
}

/// The attachments note `id` links to, as `(id, path, only here)`: whether
/// no other note of the vault links to it too.
pub fn linked_attachments(c: &Core, vault: &str, id: &str) -> Vec<(String, String, bool)> {
    let Some(r) = c.replicas.get(vault) else {
        return vec![];
    };
    let live = c.live_entries(vault);
    let cands: Vec<(String, String)> = live
        .iter()
        .filter(|e| e.kind != EntryKind::Folder)
        .map(|e| (e.id.clone(), e.path.clone()))
        .collect();
    let Some(me) = live.iter().find(|e| e.id == id) else {
        return vec![];
    };
    let mine = markdown::linked_ids(&r.note_text(id), &me.path, &cands);
    let others: BTreeSet<String> = live
        .iter()
        .filter(|e| e.kind == EntryKind::Note && e.id != id)
        .flat_map(|e| markdown::linked_ids(&r.note_text(&e.id), &e.path, &cands))
        .collect();
    mine.into_iter()
        .filter_map(|a| {
            let e = live
                .iter()
                .find(|e| e.id == a && e.kind == EntryKind::Attachment)?;
            Some((a.clone(), e.path.clone(), !others.contains(&a)))
        })
        .collect()
}

/// Move a note, or a folder with everything in it, to another vault
/// ([`moving`]): the copies there first, then — once the server has them —
/// the originals go: deleted for good from the quick notes, to the trash of
/// a vault. History, comments and shares stay with the originals. Returns
/// the copies' ids by the originals'.
pub async fn move_to_vault(
    from: &str,
    what: &What,
    to: &str,
    folder: &str,
    name: &str,
) -> Result<HashMap<String, String>, String> {
    let name = name.trim();
    if name.is_empty() || name.contains(['/', '\\']) {
        return Err("Give it a name without '/'.".into());
    }
    if from == to {
        return Err("Pick another vault.".into());
    }
    let (writable, into_quick, online) = step(|c| {
        (
            c.can_write(from) && c.can_write(to),
            c.is_quick_notes(to),
            c.socket.as_ref().is_some_and(|s| s.is_ready()),
        )
    });
    if !writable || into_quick {
        return Err("You can't move notes there.".into());
    }
    if !online {
        return Err("Moving to another vault needs the server: try again once online.".into());
    }
    let plan = step(|c| match c.replicas.get(from) {
        Some(r) => moving::plan(r, what),
        None => Err(Unmovable::Gone),
    })
    .map_err(|e| match (e, what) {
        (Unmovable::Gone, What::Note(_)) => "That note is gone.".to_string(),
        (Unmovable::Gone, What::Folder(_)) => "That folder is gone.".to_string(),
        (Unmovable::Loading, _) => {
            "Some of it hasn't arrived from the server yet: try again in a moment.".to_string()
        }
        (Unmovable::NeedsNewerApp, _) => {
            "It holds things a newer version of notekpr added: reload the page to move it."
                .to_string()
        }
    })?;
    let placed = step(|c| match c.replicas.get(to) {
        Some(r) => plan.place(r, folder, name).map_err(|e| e.to_string()),
        None => Err("That vault hasn't loaded yet.".to_string()),
    })?;
    // The copies: folders, files (downloaded and uploaded again), notes.
    let mut ids = HashMap::new();
    let mut made = Vec::new();
    for copy in &placed.copies {
        let id = match copy.kind {
            EntryKind::Folder => step(|c| new_folder_at(c, to, &copy.path)),
            EntryKind::Attachment => {
                let uploaded = match attachment_bytes(from, &copy.from).await {
                    Some(bytes) => upload_file(to, &copy.path, &bytes).await,
                    None => None,
                };
                let Some((id, _)) = uploaded else {
                    // Nothing half moved: what was made there goes again.
                    step(|c| {
                        change_index(c, to, |ix| {
                            ix.trash_all(made.iter().map(String::as_str), now())
                        })
                    });
                    state::ui().tree_rev.update(|v| *v += 1);
                    return Err("A file could not be copied, so nothing moved.".into());
                };
                id
            }
            EntryKind::Note => step(|c| new_note_at(c, to, &copy.path, &copy.text)),
            EntryKind::Drawing => step(|c| {
                new_drawing_at(c, to, &copy.path, &copy.drawing.clone().unwrap_or_default())
            }),
        };
        step(|c| {
            if let Some(icon) = &copy.icon {
                change_index(c, to, |ix| ix.set_icon(&id, Some(icon)));
            }
            if let Some(key) = &copy.order {
                change_index(c, to, |ix| {
                    ix.set_order([(id.as_str(), Some(key.as_str()))])
                });
            }
        });
        made.push(id.clone());
        ids.insert(copy.from.clone(), id);
    }
    state::ui().tree_rev.update(|v| *v += 1);
    // The originals go once the server has the copies.
    let notes: Vec<&String> = placed
        .copies
        .iter()
        .filter(|c| c.kind == EntryKind::Note)
        .filter_map(|c| ids.get(&c.from))
        .collect();
    let mut stored = false;
    for _ in 0..150 + 10 * notes.len() {
        stored = step(|c| {
            let (Some(r), Some(o)) = (c.replicas.get(to), c.outboxes.get(to)) else {
                return false;
            };
            notes.iter().all(|n| !o.unconfirmed(r, n)) && !o.unconfirmed(r, INDEX_OBJECT_ID)
        });
        if stored {
            break;
        }
        util::sleep(100).await;
    }
    if !stored {
        return Err(
            "The copy isn't on the server yet, so the original stays: remove it once the copy is synced."
                .into(),
        );
    }
    let quick = step(|c| c.is_quick_notes(from));
    step(|c| {
        change_index(c, from, |ix| {
            ix.trash_all(plan.trash.iter().map(String::as_str), now())
        })
    });
    state::ui().tree_rev.update(|v| *v += 1);
    if quick {
        crate::quick::purge_deleted().await;
    }
    Ok(ids)
}

/// Delete a trashed entry for good: the vault's owner and admins only. The
/// server keeps it a while ([`purge_undo_period`]), so it can be undone.
/// Returns whether it is gone.
pub async fn purge(vault: &str, id: &str) -> bool {
    match purge_entry(vault, id).await {
        Ok(()) => true,
        Err(e) => {
            error(e.message);
            false
        }
    }
}

pub async fn empty_trash(vault: &str) {
    let ids: Vec<String> = step(|c| {
        c.replicas.get(vault).map(|r| {
            r.index
                .entries()
                .into_iter()
                .filter(|e| e.in_trash())
                .map(|e| e.id)
                .collect()
        })
    })
    .unwrap_or_default();
    for id in ids {
        if !purge(vault, &id).await {
            return;
        }
    }
    success("Trash emptied.");
}

pub fn toggle_favorite(vault: &str, note: &str) {
    step(|c| {
        let key = (vault.to_string(), note.to_string());
        if let Some(i) = c.prefs.favorites.iter().position(|f| f == &key) {
            c.prefs.favorites.remove(i);
        } else {
            c.prefs.favorites.push(key);
        }
        state::save_prefs(c);
    });
}

pub fn is_favorite(vault: &str, note: &str) -> bool {
    step(|c| {
        c.prefs
            .favorites
            .iter()
            .any(|(v, n)| v == vault && n == note)
    })
}

/// Follow a wiki link; create the note if it doesn't exist yet.
/// Go to a heading of a note being opened (`[[Note#Heading]]`): the caret
/// in the editor, or the heading in reading view. Retries briefly while the
/// note's view is still being set up.
pub fn jump_to_heading(vault: String, note: String, heading: String) {
    fn attempt(vault: String, note: String, heading: String, left: u32) {
        if try_jump(&vault, &note, &heading) || left == 0 {
            return;
        }
        util::set_timeout(100, move || attempt(vault, note, heading, left - 1));
    }
    util::set_timeout(50, move || attempt(vault, note, heading, 20));
}

fn try_jump(vault: &str, note: &str, heading: &str) -> bool {
    if state::current_route().note() != Some((vault, note)) {
        return false;
    }
    let text = step(|c| c.replicas.get(vault).map(|r| r.note_text(note))).unwrap_or_default();
    let want = heading.to_lowercase();
    let outline = knowledge::outline(vault, note, &text);
    let Some(i) = outline
        .iter()
        .position(|(_, t, _)| t.trim().to_lowercase() == want)
    else {
        return true; // no such heading: stay at the top
    };
    if state::ui().mode.get_untracked() == state::ViewMode::Reading {
        return match util::document().query_selector(&format!("article.reading [data-h=\"{i}\"]")) {
            Ok(Some(el)) => {
                el.scroll_into_view();
                true
            }
            _ => false,
        };
    }
    let ed = step(|c| {
        c.editor
            .as_ref()
            .filter(|(v, n, _)| v == vault && n == note)
            .map(|(_, _, e)| e.clone())
    });
    match ed {
        Some(ed) => {
            let off = outline[i].2;
            ed.focus();
            ed.set_selection(off, off);
            true
        }
        None => false,
    }
}

pub fn open_wiki(vault: &str, target: &str, from_path: &str) {
    let (name, heading) = match target.split_once('#') {
        Some((n, h)) => (n.trim(), Some(h.trim()).filter(|h| !h.is_empty())),
        None => (target.trim(), None),
    };
    if name.is_empty() {
        if let (Some(h), Some((v, n))) = (heading, state::current_route().note()) {
            jump_to_heading(v.to_string(), n.to_string(), h.to_string());
        }
        return;
    }
    if let Some(id) = knowledge::resolve_target(vault, name, from_path) {
        match state::step(|c| c.entry(vault, &id)).map(|e| e.kind) {
            Some(EntryKind::Attachment) => {
                let (v, i) = (vault.to_string(), id.clone());
                leptos::task::spawn_local(async move { open_attachment(&v, &i).await });
            }
            _ => {
                if state::current_route().note() != Some((vault, id.as_str())) {
                    navigate(Route::Note(vault.to_string(), id.clone()));
                }
                if let Some(h) = heading {
                    jump_to_heading(vault.to_string(), id, h.to_string());
                }
            }
        }
        return;
    }
    let (folder, title) = if name.contains('/') {
        (
            paths::parent(name).to_string(),
            paths::title(name).to_string(),
        )
    } else {
        (
            paths::parent(from_path).to_string(),
            paths::title(name).to_string(),
        )
    };
    let v = vault.to_string();
    leptos::task::spawn_local(async move {
        if let Some(id) = create_note_with(&v, &folder, &title, "").await {
            navigate(Route::Note(v, id));
        }
    });
}

// ------------------------------------------------------------------ attachments

thread_local! {
    static WAITING_EDITORS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

pub fn cached_attachment_url(vault: &str, id: &str) -> Option<String> {
    step(|c| c.attachment_urls.get(&format!("{vault}:{id}")).cloned())
}

pub async fn attachment_bytes(vault: &str, id: &str) -> Option<Vec<u8>> {
    let ct = api::get_bytes(&format!("/vaults/{vault}/attachments/{id}"))
        .await
        .ok()?;
    step(|c| c.replicas.get(vault)?.decrypt_attachment(id, &ct).ok())
}

/// Whether an entry is a drawing.
pub fn is_drawing(vault: &str, id: &str) -> bool {
    step(|c| c.entry(vault, id)).is_some_and(|e| e.kind == EntryKind::Drawing)
}

/// What an embed shows, as a URL: an attachment, or a drawing's preview.
pub async fn load_embed(vault: &str, id: &str) -> Option<String> {
    if is_drawing(vault, id) {
        step(|c| c.ensure_note_loaded(vault, id));
        crate::drawings::load_preview(vault, id).await
    } else {
        load_attachment(vault, id).await
    }
}

/// Load, decrypt and cache an attachment as a blob URL.
pub async fn load_attachment(vault: &str, id: &str) -> Option<String> {
    let key = format!("{vault}:{id}");
    let start = step(|c| {
        if let Some(u) = c.attachment_urls.get(&key) {
            return Err(Some(u.clone()));
        }
        // Being loaded already.
        if !c.attachment_loading.insert(key.clone()) {
            return Err(None);
        }
        Ok(c.entry(vault, id).map(|e| e.path).unwrap_or_default())
    });
    let name = match start {
        Ok(name) => name,
        Err(done) => return done,
    };
    let bytes = attachment_bytes(vault, id).await;
    let url = bytes.and_then(|b| util::blob_url(&b, util::mime_for(&name)));
    step(|c| {
        c.attachment_loading.remove(&key);
        if let Some(u) = &url {
            c.attachment_urls.insert(key.clone(), u.clone());
        }
    });
    url
}

/// Show an attachment in a new tab if the browser can display it safely
/// (raster images, PDFs); download anything else.
pub async fn open_attachment(vault: &str, id: &str) {
    let name = state::step(|c| c.entry(vault, id))
        .map(|e| e.path)
        .unwrap_or_default();
    if !util::opens_inline(&name) {
        download_attachment(vault, id).await;
        return;
    }
    if let Some(url) = load_attachment(vault, id).await {
        let _ = util::window().open_with_url_and_target_and_features(
            &url,
            "_blank",
            "noopener,noreferrer",
        );
    }
}

pub async fn download_attachment(vault: &str, id: &str) {
    let name = state::step(|c| c.entry(vault, id))
        .map(|e| paths::file_name(&e.path).to_string())
        .unwrap_or_else(|| "file".into());
    // Never hand the browser a type it might render as a document of this origin.
    let mime = if util::opens_inline(&name) {
        util::mime_for(&name)
    } else {
        "application/octet-stream"
    };
    if let Some(b) = attachment_bytes(vault, id).await {
        util::download(&name, mime, &b);
    } else {
        error("Could not download the file.");
    }
}

/// Encrypt and upload files, add them to the index and return embed snippets.
pub async fn upload_files(vault: &str, files: Vec<web_sys::File>, folder: &str) -> Vec<String> {
    let max = state::ui()
        .server
        .get_untracked()
        .map(|s| s.max_attachment_bytes)
        .unwrap_or(25 << 20);
    let mut embeds = Vec::new();
    if step(|c| c.full.contains(vault)) {
        error(step(|c| {
            if c.is_quick_notes(vault) {
                "Your quick notes are full: files can't be added until they have room.".to_string()
            } else {
                format!(
                    "{} is full: files can't be added until it has room.",
                    c.vault_name(vault)
                )
            }
        }));
        return embeds;
    }
    for f in files {
        let name = f.name();
        let size = f.size() as u64;
        if size > max {
            error(format!(
                "{name} is larger than {} and can't be uploaded.",
                util::human_bytes(max)
            ));
            continue;
        }
        let Some(bytes) = util::read_file(&f).await else {
            continue;
        };
        if let Some(path) = upload_bytes(vault, &paths::join(folder, &name), &bytes).await {
            let fname = paths::file_name(&path).to_string();
            embeds.push(if util::is_image(&fname) {
                format!("![[{fname}]]")
            } else {
                format!("[[{fname}]]")
            });
        }
    }
    embeds
}

pub async fn upload_bytes(vault: &str, path: &str, bytes: &[u8]) -> Option<String> {
    upload_file(vault, path, bytes).await.map(|(_, path)| path)
}

/// Upload a file to `path` (or a free name beside it): its id and path.
async fn upload_file(vault: &str, path: &str, bytes: &[u8]) -> Option<(String, String)> {
    // Quick notes are flat.
    let path = if step(|c| c.is_quick_notes(vault)) {
        paths::file_name(path)
    } else {
        path
    };
    let id = state::new_id();
    let ct = step(|c| c.replicas.get(vault)?.encrypt_attachment(&id, bytes).ok())?;
    if let Err(e) = api::put_bytes(&format!("/vaults/{vault}/attachments/{id}"), &ct).await {
        error(format!("Upload failed: {e}"));
        return None;
    }
    let path = paths::normalize(path).ok()?;
    let path = step(|c| {
        let path = unique_path(c, vault, &path);
        let mime = util::mime_for(&path);
        let size = Some(bytes.len() as u64);
        change_index(c, vault, |ix| {
            ix.create_with(&id, &path, EntryKind::Attachment, now(), size, Some(mime))
        });
        path
    });
    state::ui().tree_rev.update(|v| *v += 1);
    Some((id, path))
}

// ------------------------------------------------------------------ export / import

pub async fn export_vault(vault: &str) {
    let name = state::step(|c| c.vault_name(vault));
    info("Preparing export…");
    let (entries, _) = vault_export(vault).await;
    let zip = match zipfile::write(&entries) {
        Ok(z) => z,
        Err(e) => {
            error(format!("Export failed: {e}"));
            return;
        }
    };
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    util::download(&format!("{}.zip", safe.trim()), "application/zip", &zip);
    success(format!("Exported {} items.", entries.len()));
}

/// A vault's files for an export, at their paths in the vault (comments as
/// Markdown under `_comments/`), and the icons of its notes, files and
/// folders by path.
async fn vault_export(vault: &str) -> (Vec<zipfile::Entry>, BTreeMap<String, String>) {
    let mut entries = Vec::new();
    let mut icons = BTreeMap::new();
    // Each note's comments go to `_comments/` (after the vault's own files,
    // which keep their paths); their authors by name.
    let mut commented = Vec::new();
    for e in state::step(|c| c.live_entries(vault)) {
        if let Some(i) = &e.icon {
            icons.insert(e.path.clone(), i.clone());
        }
        match e.kind {
            EntryKind::Note => {
                let (text, has_comments) = step(|c| {
                    let r = c.replicas.get(vault);
                    (
                        r.map(|r| r.note_text(&e.id)).unwrap_or_default(),
                        r.is_some_and(|r| r.comments.contains_key(&e.id)),
                    )
                });
                if has_comments {
                    commented.push((e.id.clone(), e.path.clone()));
                }
                entries.push(zipfile::Entry {
                    name: e.path.clone(),
                    data: text.into_bytes(),
                });
            }
            // As an Excalidraw file.
            EntryKind::Drawing => {
                let scene = step(|c| {
                    c.replicas
                        .get(vault)
                        .and_then(|r| r.notes.get(&e.id))
                        .map(|d| d.drawing().scene())
                        .unwrap_or_default()
                });
                entries.push(zipfile::Entry {
                    name: e.path.clone(),
                    data: notekpr_draw::file::write_excalidraw(&scene).into_bytes(),
                });
            }
            EntryKind::Attachment => match attachment_bytes(vault, &e.id).await {
                Some(b) => entries.push(zipfile::Entry {
                    name: e.path.clone(),
                    data: b,
                }),
                None => error(format!("Skipped {} (download failed)", e.path)),
            },
            EntryKind::Folder => entries.push(zipfile::Entry {
                name: format!("{}/.keep", e.path),
                data: vec![],
            }),
        }
    }
    let mut taken: std::collections::BTreeSet<String> =
        entries.iter().map(|e| e.name.to_lowercase()).collect();
    for (id, path) in commented {
        state::learn_commenters(vault.to_string(), id.clone()).await;
        let Some(md) = step(|c| state::comments_markdown(c, vault, &id, paths::title(&path)))
        else {
            continue;
        };
        let at = paths::unique(&notekpr_core::comments::export_path(&path), |p| {
            taken.contains(&p.to_lowercase())
        });
        taken.insert(at.to_lowercase());
        entries.push(zipfile::Entry {
            name: at,
            data: md.into_bytes(),
        });
    }
    (entries, icons)
}

/// Export the vaults chosen, and the quick notes, as one `.zip` that
/// [`import_account`] reads back — on this server or another (to move
/// there). See `notekpr_core::account_export`.
pub async fn export_account(vaults: Vec<String>, quick_notes: bool) {
    use notekpr_core::account_export::{self, ExportedVault, Manifest};
    info("Preparing export…");
    let names: Vec<String> = step(|c| vaults.iter().map(|v| c.vault_name(v)).collect());
    let folders =
        account_export::folder_names(&names.iter().map(String::as_str).collect::<Vec<_>>());
    let mut manifest = Manifest {
        format: account_export::FORMAT,
        created_at: now(),
        vaults: Vec::new(),
        quick_notes: None,
    };
    let mut files = Vec::new();
    let within = |folder: &str, entries: Vec<zipfile::Entry>| -> Vec<zipfile::Entry> {
        entries
            .into_iter()
            .map(|e| zipfile::Entry {
                name: format!("{folder}/{}", e.name),
                data: e.data,
            })
            .collect()
    };
    for (v, folder) in vaults.iter().zip(folders) {
        let (entries, icons) = vault_export(v).await;
        files.extend(within(&folder, entries));
        let meta = step(|c| c.replicas.get(v).map(|r| r.meta.clone()))
            .unwrap_or_else(|| VaultMeta::named(&folder));
        manifest.vaults.push(ExportedVault {
            folder,
            meta,
            icons,
        });
    }
    if quick_notes && let Some(q) = step(|c| c.quick_notes()) {
        let (entries, _) = vault_export(&q).await;
        files.extend(within(notekpr_core::quick::NAME, entries));
        manifest.quick_notes = Some(notekpr_core::quick::NAME.to_string());
    }
    files.insert(
        0,
        zipfile::Entry {
            name: account_export::MANIFEST.to_string(),
            data: serde_json::to_vec_pretty(&manifest).unwrap_or_default(),
        },
    );
    let zip = match zipfile::write(&files) {
        Ok(z) => z,
        Err(e) => {
            error(format!("Export failed: {e}"));
            return;
        }
    };
    let d = js_sys::Date::new_0();
    let name = format!(
        "notekpr {:04}-{:02}-{:02}.zip",
        d.get_full_year(),
        d.get_month() + 1,
        d.get_date()
    );
    util::download(&name, "application/zip", &zip);
    success(format!(
        "Exported {}{}.",
        util::count(manifest.vaults.len(), "vault", "vaults"),
        if manifest.quick_notes.is_some() {
            " and your quick notes"
        } else {
            ""
        }
    ));
}

/// Import an export made by [`export_account`] (here or on another server):
/// its vaults are created again — a name already taken gets a number — and
/// its quick notes join this account's, as many as there's room for.
pub async fn import_account(files: Vec<(String, Vec<u8>)>) {
    use notekpr_core::account_export;
    let unpacked = match account_export::unpack(files) {
        Ok(u) => u,
        Err(e) => {
            error(e.to_string());
            return;
        }
    };
    let quick: Vec<(String, Vec<u8>)> = unpacked
        .quick_notes
        .unwrap_or_default()
        .into_iter()
        .filter(|(p, _)| notekpr_core::quick::is_top_level(p))
        .collect();
    let quick_count = quick.iter().filter(|(p, _)| paths::is_note(p)).count();
    let mut what: Vec<String> = unpacked
        .vaults
        .iter()
        .map(|(v, _)| format!("“{}”", v.meta.name))
        .collect();
    if quick_count > 0 {
        what.push(util::count(quick_count, "quick note", "quick notes"));
    }
    if what.is_empty() {
        info("This export holds nothing to import.");
        return;
    }
    if !util::confirm(&format!(
        "Import {}? Vaults are created anew, without their history or comments.",
        what.join(", ")
    )) {
        return;
    }
    let (mut vaults, mut notes, mut atts) = (0, 0, 0);
    for (v, files) in unpacked.vaults {
        let taken: std::collections::HashSet<String> = step(|c| {
            c.replicas
                .keys()
                .filter(|id| !c.is_quick_notes(id))
                .map(|id| c.vault_name(id).to_lowercase())
                .collect()
        });
        let name = account_export::unique_name(v.meta.name.trim(), |n| {
            taken.contains(&n.to_lowercase()) || notekpr_core::quick::is_reserved_name(n)
        });
        let id = match create_vault_of(&name, VaultKind::Regular).await {
            Ok(Some(id)) => id,
            Ok(None) => continue,
            Err(e) => {
                error(format!("Could not create the vault “{name}”: {e}"));
                continue;
            }
        };
        vaults += 1;
        let meta = v.meta.clone();
        if meta.description.is_some() || meta.icon.is_some() || meta.picture.is_some() {
            update_vault_meta(&id, |m| {
                m.description = meta.description;
                m.icon = meta.icon;
                m.picture = meta.picture;
                Ok(())
            })
            .await;
        }
        let (n, a, _) = import_files(&id, files, false).await;
        notes += n;
        atts += a;
        for (path, icon) in &v.icons {
            let entry = step(|c| {
                c.replicas
                    .get(&id)
                    .and_then(|r| r.index.find_by_path(path))
                    .map(|e| e.id)
            });
            set_entry_icon(&id, entry.as_deref(), path, icon);
        }
    }
    let mut quick_in = 0;
    if quick_count > 0 {
        let q = match step(|c| c.quick_notes()) {
            Some(q) => Some(q),
            None => create_vault_of(notekpr_core::quick::NAME, VaultKind::QuickNotes)
                .await
                .ok()
                .flatten(),
        };
        if let Some(q) = q {
            // As many notes as there's room for (and every file).
            let room = crate::quick::limit().max().map_or(usize::MAX, |m| {
                (m as usize).saturating_sub(crate::quick::count())
            });
            let mut left = room;
            let quick: Vec<(String, Vec<u8>)> = quick
                .into_iter()
                .filter(|(p, _)| {
                    if !paths::is_note(p) {
                        return true;
                    }
                    let fits = left > 0;
                    left = left.saturating_sub(1);
                    fits
                })
                .collect();
            let (n, a, _) = import_files(&q, quick, false).await;
            quick_in = n;
            atts += a;
            if quick_in < quick_count {
                info(format!(
                    "{} didn't fit: {}",
                    util::count(quick_count - quick_in, "quick note", "quick notes"),
                    notekpr_core::quick::full_message(crate::quick::limit())
                ));
            }
        }
    }
    success(format!(
        "Imported {}, {} and {}{}.",
        util::count(vaults, "vault", "vaults"),
        util::count(notes, "note", "notes"),
        util::count(atts, "file", "files"),
        if quick_in > 0 {
            format!(
                ", and {}",
                util::count(quick_in, "quick note", "quick notes")
            )
        } else {
            String::new()
        }
    ));
}

/// Import Markdown files and attachments, preserving folders.
pub async fn import(vault: &str, files: Vec<(String, Vec<u8>)>) {
    let (notes, atts, total) = import_files(vault, files, true).await;
    success(format!(
        "Imported {notes} notes and {atts} files (of {total})."
    ));
}

/// Bring files into a vault, keeping their folders (an export's empty ones,
/// `folder/.keep`, too): how many notes and files came in, of how many.
/// `strip_top`: files all in one folder (a folder upload, a zip of one) go
/// to the top level.
async fn import_files(
    vault: &str,
    mut files: Vec<(String, Vec<u8>)>,
    strip_top: bool,
) -> (usize, usize, usize) {
    let hidden = |s: &str| s.starts_with('.') || s == "__MACOSX";
    files.retain(|(p, _)| {
        let (dirs, last) = p.rsplit_once('/').unwrap_or(("", p));
        !dirs.split('/').any(hidden) && (!hidden(last) || last == ".keep")
    });
    // Drop a single common top-level folder (from folder uploads or zips).
    let top: Option<String> = files
        .first()
        .and_then(|(p, _)| p.split('/').next().map(str::to_string));
    if let Some(t) = top.filter(|_| strip_top)
        && files.len() > 1
        && files.iter().all(|(p, _)| p.starts_with(&format!("{t}/")))
    {
        for (p, _) in &mut files {
            *p = p[t.len() + 1..].to_string();
        }
    }
    // An export's comments aren't notes, and can't be brought back (they
    // are signed by their authors).
    let before = files.len();
    files.retain(|(p, _)| p.split('/').next() != Some(notekpr_core::comments::EXPORT_DIR));
    if files.len() < before {
        info("The export's comments (its _comments folder) are not imported.");
    }
    // Empty folders.
    let folders: Vec<String> = files
        .iter()
        .filter_map(|(p, _)| notekpr_core::account_export::kept_folder(p))
        .filter_map(|f| paths::normalize(f).ok())
        .collect();
    files.retain(|(p, _)| !p.ends_with("/.keep") && p != ".keep");
    step(|c| {
        for f in &folders {
            let exists = c
                .replicas
                .get(vault)
                .is_some_and(|r| r.index.folders().contains(f));
            if !exists && !c.is_quick_notes(vault) {
                new_folder_at(c, vault, f);
            }
        }
    });
    let total = files.len();
    let mut notes = 0;
    let mut atts = 0;
    let max = state::max_update_bytes();
    for (i, (path, data)) in files.into_iter().enumerate() {
        // Send in pushes of bounded size as we go, and let the page breathe.
        step(|c| {
            if state::outbox_bytes(c, vault) >= max {
                state::flush_now(c);
            }
        });
        if i % 20 == 19 {
            util::sleep(0).await;
        }
        if paths::is_note(&path) {
            let text = String::from_utf8_lossy(&data).replace("\r\n", "\n");
            let folder = paths::parent(&path).to_string();
            let title = paths::title(&path).to_string();
            if create_note_with(vault, &folder, &title, &text)
                .await
                .is_some()
            {
                notes += 1;
            }
        } else if upload_bytes(vault, &path, &data).await.is_some() {
            atts += 1;
        }
    }
    step(state::flush_now);
    state::ui().tree_rev.update(|v| *v += 1);
    (notes, atts, total)
}

pub async fn files_to_import(files: Vec<web_sys::File>) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for f in files {
        let rel = js_sys::Reflect::get(&f, &"webkitRelativePath".into())
            .ok()
            .and_then(|v| v.as_string())
            .filter(|s| !s.is_empty());
        let name = rel.unwrap_or_else(|| f.name());
        let Some(bytes) = util::read_file(&f).await else {
            continue;
        };
        if name.to_lowercase().ends_with(".zip") {
            match zipfile::read(&bytes) {
                Ok(entries) => out.extend(entries.into_iter().map(|e| (e.name, e.data))),
                Err(e) => error(format!("{name}: {e}")),
            }
        } else {
            out.push((name, bytes));
        }
    }
    out
}

// ------------------------------------------------------------------ history

pub fn restore_revision(vault: &str, note: &str, text: &str) {
    if step(|c| state::set_note_text(c, vault, note, text)) > 0 {
        refresh_open_editor(vault);
        success("Restored this version. The previous text remains in history.");
    }
}

// ------------------------------------------------------------------ editor host

pub struct NoteHost {
    pub vault: String,
    pub note: String,
    /// The note is shared with us individually: the rest of its vault
    /// (linked notes, attachments) is out of reach.
    pub shared: bool,
}

impl NoteHost {
    pub fn new(vault: &str, note: &str) -> Rc<NoteHost> {
        Self::with_mode(vault, note, false)
    }

    pub fn for_shared(vault: &str, note: &str) -> Rc<NoteHost> {
        Self::with_mode(vault, note, true)
    }

    fn with_mode(vault: &str, note: &str, shared: bool) -> Rc<NoteHost> {
        state::step(|c| c.ensure_note_loaded(vault, note));
        step(|c| {
            if let Some(n) = c
                .replicas
                .get_mut(vault)
                .and_then(|r| r.notes.get_mut(note))
                && !n.undo_enabled()
            {
                n.enable_undo_with_clock(|| js_sys::Date::now() as u64);
            }
        });
        Rc::new(NoteHost {
            vault: vault.to_string(),
            note: note.to_string(),
            shared,
        })
    }

    fn path(&self) -> String {
        state::step(|c| c.entry(&self.vault, &self.note))
            .map(|e| e.path)
            .unwrap_or_default()
    }

    fn take(&self, f: impl FnOnce(&mut NoteDoc) -> Option<Vec<u8>>) -> Option<String> {
        step(|c| {
            let n = c.replicas.get_mut(&self.vault)?.notes.get_mut(&self.note)?;
            let u = f(n)?;
            let t = n.text();
            local_update(c, &self.vault, &self.note, u);
            Some(t)
        })
    }
}

impl Host for NoteHost {
    fn edit(&self, base: &str, edits: &[Edit]) -> bool {
        // One edit is one update; refuse what the server could never store
        // rather than block the vault's sync with it.
        let inserted = crate::sizes::inserted_bytes(edits);
        let max = state::max_update_bytes();
        if inserted > crate::sizes::insert_budget(max) {
            error(format!(
                "This is too large to add at once ({}; the server accepts {} per change). Add it in smaller parts, or attach it as a file.",
                util::human_bytes(inserted as u64),
                util::human_bytes(max as u64)
            ));
            return false;
        }
        // The editor's text and the CRDT can differ when remote text arrived
        // during an IME composition: the editor's offsets are then relative to
        // an outdated text. Move simple edits onto the current text, or drop
        // them; either way the editor reloads the note's text afterwards.
        let res = step(|c| {
            let n = c.replicas.get_mut(&self.vault)?.notes.get_mut(&self.note)?;
            let current = n.text();
            let in_sync = current == base;
            let rebased;
            let edits = if in_sync {
                edits
            } else {
                match editor::rebase(base, &current, edits) {
                    Some(e) => {
                        rebased = e;
                        &rebased[..]
                    }
                    None => return Some((None, 0, false)),
                }
            };
            // Out-of-range offsets would make yrs panic.
            if !editor::edits_fit(&current, edits) {
                return Some((None, 0, false));
            }
            Some((n.apply_edits(edits), n.len(), in_sync))
        });
        let Some((update, len, in_sync)) = res else {
            return true;
        };
        if !in_sync {
            util::warn(if update.is_some() {
                "the note changed during text input; the input was moved onto the current text"
            } else {
                "the note changed during text input; the input could not be applied"
            });
        }
        if let Some(u) = update {
            let d = step(|c| {
                local_update(c, &self.vault, &self.note, u);
                state::touch(c, &self.vault, &self.note, len);
                c.content_debounce.clone()
            });
            let ui = state::ui();
            d.run(400, move || ui.content_rev.update(|v| *v += 1));
        }
        in_sync
    }

    fn text(&self) -> Option<String> {
        step(|c| {
            c.replicas
                .get(&self.vault)?
                .notes
                .get(&self.note)
                .map(|n| n.text())
        })
    }

    fn undo(&self) -> Option<String> {
        self.take(|n| n.undo())
    }

    fn redo(&self) -> Option<String> {
        self.take(|n| n.redo())
    }

    fn boundary(&self) {
        step(|c| {
            if let Some(n) = c
                .replicas
                .get_mut(&self.vault)
                .and_then(|r| r.notes.get_mut(&self.note))
            {
                n.undo_boundary();
            }
        });
    }

    fn link_exists(&self, target: &str) -> bool {
        !self.shared && knowledge::link_exists(&self.vault, target)
    }

    fn open_link(&self, target: &Target, _new_tab: bool) {
        match target {
            Target::Url(u) if render::safe_external(u) => {
                let _ = util::window().open_with_url_and_target_and_features(
                    u.trim(),
                    "_blank",
                    "noopener,noreferrer",
                );
            }
            Target::Url(_) => info("Only web and e-mail links can be opened from a note."),
            Target::Wiki(_) if self.shared => {
                info("Only this note is shared with you, not the notes it links to.")
            }
            Target::Wiki(t) => open_wiki(&self.vault, t, &self.path()),
        }
    }

    fn image_url(&self, target: &str) -> Option<String> {
        if self.shared {
            return None;
        }
        let id = knowledge::resolve_embed(&self.vault, target)?;
        let cached = if is_drawing(&self.vault, &id) {
            crate::drawings::cached_preview(&self.vault, &id)
        } else {
            cached_attachment_url(&self.vault, &id)
        };
        if let Some(u) = cached {
            return Some(u);
        }
        let v = self.vault.clone();
        leptos::task::spawn_local(async move {
            if load_embed(&v, &id).await.is_some() {
                let ed = step(|c| {
                    c.editor
                        .as_ref()
                        .filter(|(ev, _, _)| *ev == v)
                        .map(|(_, _, e)| e.clone())
                });
                if let Some(ed) = ed {
                    ed.refresh();
                }
            }
        });
        None
    }

    fn selection_changed(&self, anchor: usize, head: usize) {
        step(|c| c.cursor = Some((anchor, head)));
        step(|c| state::send_presence(c));
        crate::views::comments::selection_changed(&self.vault, &self.note, anchor, head);
    }

    fn insert_files(&self, files: Vec<web_sys::File>, at: usize) {
        if self.shared {
            error("Files can't be added to a note shared with you.");
            return;
        }
        let (v, n) = (self.vault.clone(), self.note.clone());
        // The links go where the files were pasted or dropped, wherever that
        // is once the upload is done.
        let anchor = open_editor(&v, &n).map(|ed| ed.anchor(at));
        leptos::task::spawn_local(async move {
            let embeds = upload_files(&v, files, "attachments").await;
            insert_embeds_at(&v, &n, &embeds, anchor.as_ref());
        });
    }
}

fn open_editor(vault: &str, note: &str) -> Option<Rc<editor::Editor>> {
    step(|c| {
        c.editor
            .as_ref()
            .filter(|(v, n, _)| v == vault && n == note)
            .map(|(_, _, e)| e.clone())
    })
}

/// Insert links to uploaded files into the note they were added to — only
/// if that note is still the one open (uploads take a while).
pub fn insert_embeds(vault: &str, note: &str, embeds: &[String]) {
    insert_embeds_at(vault, note, embeds, None);
}

/// [`insert_embeds`] at `at` (taken when the files were added) instead of
/// the selection.
fn insert_embeds_at(vault: &str, note: &str, embeds: &[String], at: Option<&editor::Anchor>) {
    if embeds.is_empty() {
        return;
    }
    match open_editor(vault, note) {
        Some(ed) => match at {
            Some(a) => ed.insert_at(a, &embeds.join("\n")),
            None => ed.insert_text(&embeds.join("\n")),
        },
        None => info(format!(
            "Uploaded {} to “attachments”. The note was closed meanwhile; add {} to link it.",
            if embeds.len() == 1 {
                "the file".to_string()
            } else {
                format!("{} files", embeds.len())
            },
            embeds.join(" ")
        )),
    }
}

/// Pick files with a hidden `<input type=file>`.
pub fn pick_files(
    directory: bool,
    accept: &str,
    on_files: impl FnOnce(Vec<web_sys::File>) + 'static,
) {
    let doc = util::document();
    let input: web_sys::HtmlInputElement = doc.create_element("input").unwrap().unchecked_into();
    input.set_type("file");
    input.set_multiple(true);
    if !accept.is_empty() {
        input.set_accept(accept);
    }
    if directory {
        let _ = input.set_attribute("webkitdirectory", "");
    }
    let cell = Rc::new(RefCell::new(Some(on_files)));
    let inp = input.clone();
    let cb = wasm_bindgen::closure::Closure::wrap(Box::new(move |_e: web_sys::Event| {
        let files: Vec<web_sys::File> = inp
            .files()
            .map(|l| (0..l.length()).filter_map(|i| l.get(i)).collect())
            .unwrap_or_default();
        if let Some(f) = cell.borrow_mut().take() {
            f(files);
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    let _ = input.add_event_listener_with_callback("change", cb.as_ref().unchecked_ref());
    cb.forget();
    input.click();
}
