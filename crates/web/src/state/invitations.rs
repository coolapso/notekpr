//! Invitations (docs/protocol.md §5.6): those to this person, shown as
//! banners to accept or decline; and those to the vaults they own, whose
//! accepted ones this app completes — it adds the person (the key, the
//! membership log) without a click: the owner decided when inviting.

use notekpr_core::invitation::NotAdded;
use notekpr_sync::members::{Completion, NotCompleted};

use super::*;

/// The tag [`super::vaults::begin_reload`] runs completions under: one at a
/// time, once more if asked meanwhile.
const COMPLETING: &str = "\0invitations";

/// Reload the invitations, and add whoever accepted one.
pub async fn refresh_invitations() {
    let Ok(list) = api::get::<InvitationsResponse>("/invitations").await else {
        return;
    };
    let accepted = list.sent.iter().any(|i| i.accepted.is_some());
    ui().invitations.set(list);
    if accepted {
        complete_invitations().await;
    }
}

/// Add whoever accepted an invitation to a vault this person owns (or to
/// one of its notes), as far as this app can: it acts on the vault's log as
/// verified just now, checks the invitation and the acceptance, and
/// doesn't use a key other than one it pinned for them (the owner compares
/// fingerprints first, in the sharing dialog).
pub async fn complete_invitations() {
    if !super::vaults::begin_reload(COMPLETING).await {
        return;
    }
    for attempt in 0.. {
        // Someone else's change got in first (another owner's app, or
        // the log moved on): look again, a few times.
        if complete_once().await && attempt < 3 {
            if let Ok(list) = api::get::<InvitationsResponse>("/invitations").await {
                ui().invitations.set(list);
            }
            continue;
        }
        let done = step(|c| {
            let again = c.reload_again.remove(COMPLETING);
            if !again {
                c.reloading.remove(COMPLETING);
            }
            !again
        });
        if done {
            break;
        }
    }
}

/// One round; `true` if one was refused as changed meanwhile (`409`, or
/// `412`: the note's key changed).
async fn complete_once() -> bool {
    let mut raced = false;
    let accepted: Vec<SentInvitation> = ui()
        .invitations
        .get_untracked()
        .sent
        .into_iter()
        .filter(|i| i.accepted.is_some())
        .collect();
    let mut joined = vec![];
    for inv in accepted {
        let v = inv.vault_id.clone();
        let owner = step(|c| {
            c.replicas
                .get(&v)
                .is_some_and(|r| r.member && r.role == Role::Owner)
                && !c.offline_start
        });
        if !owner || fresh_log(&v).await.is_err() {
            continue;
        }
        let made = step(|c| {
            let user = &inv.accepted.as_ref()?.user;
            let pinned = c.prefs.pins.get(&user.id).cloned();
            let r = c.replicas.get(&v)?;
            let note = inv.object_id.as_ref().and_then(|o| r.doc_keys.get(o));
            Some(notekpr_sync::members::completion(
                c.member_logs.get(&v)?,
                c.account.as_ref()?,
                &c.user_id,
                &r.keyring,
                note,
                &inv,
                pinned.as_deref(),
                now_ms(),
            ))
        });
        let (done, user, key) = match made {
            Some(Ok(d)) => d,
            // Shown in the sharing dialog, for the owner to verify.
            Some(Err(NotCompleted::Refused(NotAdded::KeyChanged))) | None => continue,
            Some(Err(e)) => {
                util::warn(&format!("not adding who accepted an invitation: {e}"));
                continue;
            }
        };
        // Trusted from now on, as when sharing with someone the first time.
        step(|c| {
            if engine::check_pin(&mut c.prefs.pins, &user.id, &key.fingerprint())
                == engine::PinStatus::New
            {
                save_prefs(c);
            }
        });
        let sent = match &done {
            Completion::Member(req) => {
                api::post::<_, MemberInfo>(&format!("/vaults/{v}/members"), req)
                    .await
                    .map(|_| ())
            }
            Completion::Share { object_id, req, .. } => api::post::<_, ObjectShareInfo>(
                &format!("/vaults/{v}/objects/{object_id}/shares"),
                req,
            )
            .await
            .map(|_| ()),
        };
        match sent {
            Ok(()) => {
                step(|c| {
                    c.users.insert(user.id.clone(), user.clone());
                    if let (Completion::Share { object_id, .. }, Some(r)) =
                        (&done, c.replicas.get_mut(&v))
                    {
                        r.shared.insert(object_id.clone());
                    }
                });
                reload_log(&v).await;
                // The note's comments so far go along, as the inviter said.
                if let Completion::Share {
                    object_id,
                    with_comments: true,
                    ..
                } = &done
                {
                    crate::actions::carry_comments(&v, object_id).await;
                }
                // Who has an invited address is the server's word: say
                // who accepted it, so a stranger in its place shows.
                joined.push(match &inv.email {
                    Some(a) => format!("{} ({a})", user.display_name),
                    None => user.display_name.clone(),
                });
                ui().shares_rev.update(|x| *x += 1);
                ui().tree_rev.update(|x| *x += 1);
            }
            // Another of the owners' apps did it, or changed the log first:
            // the next list says.
            Err(e) if e.status == 409 => raced = true,
            // The note's key changed meanwhile: with the new one, again.
            Err(e) if e.status == 412 => {
                reload_vault_keys(v.clone()).await;
                raced = true;
            }
            Err(e) => util::warn(&format!("could not add who accepted an invitation: {e}")),
        }
    }
    if !joined.is_empty() {
        info(format!(
            "{} accepted your invitation and can open what you shared now. Not who you meant? Remove them in Share.",
            joined.join(", ")
        ));
    }
    raced
}

/// An accepted invitation this app doesn't complete because the person's
/// key isn't the one pinned for them: the owner compares fingerprints.
pub fn key_changed(c: &Core, inv: &SentInvitation) -> bool {
    inv.accepted.as_ref().is_some_and(|a| {
        let pinned = c.prefs.pins.get(&a.user.id);
        let fp = a
            .user
            .account_key
            .as_ref()
            .and_then(|k| k.verify().ok())
            .map(|k| k.fingerprint());
        pinned.is_some() && fp.is_some() && pinned != fp.as_ref()
    })
}
