//! Sharing by invitation (docs/protocol.md §5.6): inviting someone to a
//! vault or a note, accepting or declining an invitation, cancelling one;
//! and, as an owner, adding whoever accepted — wrapping the key for them
//! and signing them into the membership log, without asking: the owner
//! decided when inviting. The sync daemon does that whenever someone
//! accepts; `notekpr invitations` does it too.

use anyhow::{Result, anyhow, bail};
use notekpr_core::engine::{self, PinStatus};
use notekpr_core::invitation::{Invitee, NotAdded};
use notekpr_core::protocol::{
    AcceptInvitationRequest, InvitationsResponse, MemberInfo, ObjectShareInfo, PublicUser,
    ReceivedInvitation, Role, SentInvitation,
};
use notekpr_sync::members::{self, Completion, NotCompleted};

use crate::hub::Hub;
use crate::manage;
use crate::session::Session;
use crate::session::SharedVault;

/// Whom to invite.
#[derive(Clone, Debug)]
pub enum To {
    /// Someone found among the people one shares with (or, with a
    /// searchable directory, anyone).
    User(PublicUser),
    /// Anyone, by their full address.
    Email(String),
}

impl To {
    pub fn label(&self) -> String {
        match self {
            To::User(u) => u.display_name.clone(),
            To::Email(a) => notekpr_core::invitation::normalize_email(a).unwrap_or(a.clone()),
        }
    }
}

/// Whether `reference` is an e-mail address (it is then invited as such).
pub fn is_address(reference: &str) -> bool {
    notekpr_core::invitation::normalize_email(reference).is_some()
}

/// Invite someone to the vault or, with `object_id`, to that note alone
/// (its sharing must have started: a fresh key).
pub async fn invite(
    session: &Session,
    vault: &SharedVault,
    object_id: Option<&str>,
    to: &To,
    role: Role,
    with_comments: bool,
) -> Result<SentInvitation> {
    let id = {
        let v = vault.lock().await;
        manage::require_owner(&v)?;
        v.id().to_string()
    };
    let invitee = match to {
        To::User(u) => Invitee::User(&u.id),
        To::Email(a) => Invitee::Email(a),
    };
    let req = members::invitation(
        session.account(),
        &session.user_id,
        &id,
        object_id,
        invitee,
        role,
        with_comments,
        notekpr_core::now_ms(),
    )
    .ok_or_else(|| anyhow!("'{}' isn't an e-mail address", to.label()))?;
    Ok(session
        .api
        .post(&format!("/vaults/{id}/invitations"), &req)
        .await?)
}

pub async fn list(session: &Session) -> Result<InvitationsResponse> {
    Ok(session.api.get("/invitations").await?)
}

/// The one invitation whose id is (or starts with) `reference`.
fn pick<'a, T>(list: &'a [T], id: impl Fn(&T) -> &str, reference: &str) -> Result<&'a T> {
    let found: Vec<&T> = list
        .iter()
        .filter(|i| id(i) == reference || id(i).starts_with(reference))
        .collect();
    match found[..] {
        [one] => Ok(one),
        [] => bail!("no invitation '{reference}': `notekpr invitations` lists them"),
        _ => bail!("'{reference}' fits several invitations: give more of its id"),
    }
}

fn pinned(session: &Session, user_id: &str) -> Result<Option<String>> {
    session.profiles.update(|p| p.pins.get(user_id).cloned())
}

/// Accept an invitation to us, signed with our key; its inviter's key is
/// pinned from now on (as when sharing with someone the first time).
pub async fn accept(session: &Session, reference: &str) -> Result<ReceivedInvitation> {
    let all = list(session).await?.received;
    let inv = pick(&all, |i| &i.id, reference)?.clone();
    if inv.accepted {
        return Ok(inv);
    }
    let key = inv.verified_inviter().ok_or_else(|| {
        anyhow!(
            "this invitation isn't signed with {}'s own key: it can't be accepted",
            inv.from.display_name
        )
    })?;
    let signature = members::acceptance(
        session.account(),
        &session.user_id,
        &inv,
        pinned(session, &inv.from.id)?.as_deref(),
    )
    .ok_or_else(|| {
        anyhow!(
            "WARNING: this invitation is signed with a key of {} other than the one pinned for them \
             ({}). Ask them over a trusted channel before anything else.",
            inv.from.display_name,
            key.fingerprint()
        )
    })?;
    session.profiles.update(|p| {
        engine::check_pin(&mut p.pins, &inv.from.id, &key.fingerprint());
    })?;
    session
        .api
        .post::<_, ()>(
            &format!("/invitations/{}/accept", inv.id),
            &AcceptInvitationRequest { signature },
        )
        .await?;
    Ok(inv)
}

/// Decline an invitation (or take back an acceptance): its sender isn't
/// told.
pub async fn decline(session: &Session, reference: &str) -> Result<ReceivedInvitation> {
    let all = list(session).await?.received;
    let inv = pick(&all, |i| &i.id, reference)?.clone();
    session
        .api
        .post::<_, ()>(&format!("/invitations/{}/decline", inv.id), &())
        .await?;
    Ok(inv)
}

/// Cancel an invitation to a vault we own (or to one of its notes).
pub async fn cancel(session: &Session, reference: &str) -> Result<SentInvitation> {
    let all = list(session).await?.sent;
    let inv = pick(&all, |i| &i.id, reference)?.clone();
    session
        .api
        .delete(&format!("/vaults/{}/invitations/{}", inv.vault_id, inv.id))
        .await?;
    Ok(inv)
}

/// What [`complete`] did.
#[derive(Default, Debug)]
pub struct Completed {
    /// Who was added (display names).
    pub added: Vec<String>,
    /// Who accepted with a key other than the one pinned for them: added
    /// once the owner compared fingerprints (`invitations verify`).
    pub key_changed: Vec<(String, String)>,
    /// Vaults with comments to send (a note's, which went with it).
    pub to_push: Vec<String>,
}

/// As an owner: add whoever accepted an invitation to one of `vaults` (or
/// to one of its notes) — on the vault's log as fetched just now, with the
/// invitation and the acceptance checked, never to a key other than the
/// one pinned for them.
pub async fn complete(
    session: &Session,
    vaults: &[SharedVault],
    list: &InvitationsResponse,
) -> Result<Completed> {
    let mut out = Completed::default();
    let mut list = list.clone();
    // Someone else's change got in first (another owner's app, the log
    // moved on, a note's key renewed): look again, a few times.
    for attempt in 0..3 {
        if !complete_once(session, vaults, &list, &mut out).await? || attempt == 2 {
            break;
        }
        list = self::list(session).await?;
    }
    out.key_changed.sort();
    out.key_changed.dedup();
    Ok(out)
}

/// One round of [`complete`]; `true` if one was refused as changed
/// meanwhile.
async fn complete_once(
    session: &Session,
    vaults: &[SharedVault],
    list: &InvitationsResponse,
    out: &mut Completed,
) -> Result<bool> {
    let mut raced = false;
    for inv in list.sent.iter().filter(|i| i.accepted.is_some()) {
        let mut found = None;
        for v in vaults {
            if v.lock().await.id() == inv.vault_id {
                found = Some(v.clone());
            }
        }
        let Some(vault) = found else {
            continue;
        };
        if manage::require_owner(&*vault.lock().await).is_err() {
            continue;
        }
        if let Err(e) = session.reload_member_log(&vault).await {
            tracing::warn!("not adding who accepted an invitation: {e:#}");
            continue;
        }
        let user = &inv.accepted.as_ref().expect("filtered").user;
        let pin = pinned(session, &user.id)?;
        let made = {
            let v = vault.lock().await;
            let note = inv
                .object_id
                .as_ref()
                .and_then(|o| v.replica.doc_keys.get(o));
            members::completion(
                &v.log,
                session.account(),
                &session.user_id,
                &v.replica.keyring,
                note,
                inv,
                pin.as_deref(),
                notekpr_core::now_ms(),
            )
        };
        let (done, user, key) = match made {
            Ok(d) => d,
            Err(NotCompleted::Refused(NotAdded::KeyChanged)) => {
                let fp = user
                    .account_key
                    .as_ref()
                    .and_then(|k| k.verify().ok())
                    .map(|k| k.fingerprint())
                    .unwrap_or_default();
                out.key_changed.push((user.display_name.clone(), fp));
                continue;
            }
            Err(e) => {
                tracing::warn!("not adding who accepted an invitation: {e}");
                continue;
            }
        };
        session.profiles.update(|p| {
            if engine::check_pin(&mut p.pins, &user.id, &key.fingerprint()) == PinStatus::New {
                tracing::debug!("pinned the key of {}", user.id);
            }
        })?;
        let id = &inv.vault_id;
        let sent = match &done {
            Completion::Member(req) => session
                .api
                .post::<_, MemberInfo>(&format!("/vaults/{id}/members"), req)
                .await
                .map(|_| ()),
            Completion::Share { object_id, req, .. } => session
                .api
                .post::<_, ObjectShareInfo>(
                    &format!("/vaults/{id}/objects/{object_id}/shares"),
                    req,
                )
                .await
                .map(|_| ()),
        };
        match sent {
            Ok(()) => {
                session.reload_member_log(&vault).await?;
                if let Completion::Share {
                    object_id,
                    with_comments,
                    ..
                } = &done
                {
                    // Our keys as the server has them now: the note shared,
                    // and where sharing started.
                    session.refresh_keys(&vault).await?;
                    // Its comments so far go along, as the inviter said.
                    if *with_comments {
                        let mut v = vault.lock().await;
                        if !v.replica.carry_comments(object_id)?.is_empty() {
                            v.mark_comments_dirty(object_id);
                            out.to_push.push(inv.vault_id.clone());
                        }
                        v.save()?;
                    }
                }
                // Who has an invited address is the server's word: say who
                // accepted it, so a stranger in its place shows.
                out.added.push(match &inv.email {
                    Some(a) => format!("{} ({}) for {a}", user.display_name, user.id),
                    None => user.display_name.clone(),
                });
            }
            // Another owner's app did it first, or changed the log, or the
            // note's key changed: look again, with the keys as they are now.
            Err(e) if e.status() == Some(409) || e.status() == Some(412) => {
                tracing::debug!("adding who accepted an invitation: {e}");
                if e.status() == Some(412) {
                    session.refresh_keys(&vault).await?;
                }
                raced = true;
            }
            Err(e) => tracing::warn!("could not add who accepted an invitation: {e}"),
        }
    }
    Ok(raced)
}

/// What the sync daemon does when it connects, and whenever invitations
/// change: say who invites us, and add whoever accepted.
pub async fn on_change(hub: &Hub) {
    let list = match list(&hub.session).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("listing invitations: {e:#}");
            return;
        }
    };
    for r in list.received.iter().filter(|r| !r.accepted) {
        crate::ui::status(&format!(
            "{} invites you to {}: `notekpr invitations accept {}`",
            r.from.display_name,
            if r.object_id.is_some() {
                "a note"
            } else {
                "a vault"
            },
            short(&r.id)
        ));
    }
    match complete(&hub.session, &hub.vaults(), &list).await {
        Ok(done) => {
            for id in &done.to_push {
                let _ = hub.local_changed(id, vec![], false).await;
            }
            if !done.added.is_empty() {
                crate::ui::status(&format!(
                    "{} accepted your invitation and can open what you shared now (not who you meant? \
                     `vault unshare`)",
                    done.added.join(", ")
                ));
            }
            for (name, fp) in done.key_changed {
                crate::ui::warn(&format!(
                    "{name} accepted your invitation, but their key changed since you pinned it (now {fp}): \
                     compare it with them, then run `notekpr invitations verify <id>`"
                ));
            }
        }
        Err(e) => tracing::warn!("adding who accepted an invitation: {e:#}"),
    }
}

/// An invitation's id, short enough to type.
pub fn short(id: &str) -> &str {
    &id[..id.len().min(8)]
}
