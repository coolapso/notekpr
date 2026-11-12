//! Sharing by invitation (docs/protocol.md §5.6).
//!
//! Owners invite someone to a vault, or to one of its notes, by an account
//! they know already or by an e-mail address. The answer is the same
//! whether or not an address has an account, so the sharer learns nothing
//! from it. The person accepts (or declines, which tells the owners
//! nothing: it looks unanswered); then an owner's app adds them through
//! `POST …/members` or `…/shares` with the invitation's id, which ends it.
//!
//! The server can make no invitation nor accept one: the inviting owner
//! signs it and the person their acceptance, and owners' apps check both
//! before giving anyone a key (`notekpr_core::invitation`). Never logged:
//! the addresses.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use notekpr_core::invitation::{self, CLOCK_SKEW_MS, Invitee, Terms};
use notekpr_core::protocol::{
    AcceptInvitationRequest, CreateInvitationRequest, InvitationAcceptance, InvitationsResponse,
    ReceivedInvitation, Role, SentInvitation, UserDirectory, UserEventKind,
};

use crate::access;
use crate::auth::Principal;
use crate::db;
use crate::error::{AppError, AppResult, bad, conflict};
use crate::repo;
use crate::repo::invitations::Invitation;
use crate::routes::me::notify_user;
use crate::routes::shares::check_object;
use crate::routes::users::public_user;
use crate::state::AppState;

const MAX_SIGNATURE_BYTES: usize = 64;

fn terms(i: &Invitation) -> Option<Terms<'_>> {
    Some(Terms {
        id: &i.id,
        vault_id: &i.vault_id,
        object_id: i.object_id.as_deref(),
        invited_by: &i.invited_by,
        invitee: match (&i.user_id, &i.email) {
            (Some(u), None) => Invitee::User(u),
            (None, Some(e)) => Invitee::Email(e),
            _ => return None,
        },
        role: &i.role,
        created_at: i.created_at,
        with_comments: i.with_comments,
    })
}

/// Tell whoever an invitation is for, and the vault's owners, that it
/// changed.
async fn notify(state: &AppState, vault_id: &str, people: &[Option<String>], email: Option<&str>) {
    let mut to: Vec<String> = people.iter().flatten().cloned().collect();
    if let Some(a) = email
        && let Ok(ids) = repo::invitations::people_with_email(&state.db, a).await
    {
        to.extend(ids);
    }
    if let Ok(owners) = repo::vaults::owner_ids(&state.db, vault_id).await {
        to.extend(owners);
    }
    to.sort();
    to.dedup();
    for u in to {
        notify_user(state, &u, UserEventKind::InvitationsChanged);
    }
}

/// An invitation as its vault's owners see it.
async fn sent_view(state: &AppState, i: Invitation) -> AppResult<SentInvitation> {
    let user = match &i.user_id {
        Some(id) => {
            let mut u = public_user(&state.db, id).await?;
            // Whom they picked, by name: not their address.
            u.email = None;
            u.account_key = None;
            Some(u)
        }
        None => None,
    };
    let accepted = match (&i.state[..], &i.accepted_by, &i.acceptance) {
        ("accepted", Some(by), Some(sig)) => {
            let mut u = public_user(&state.db, by).await?;
            u.email = None;
            Some(InvitationAcceptance {
                user: u,
                accepted_at: i.accepted_at.unwrap_or_default(),
                signature: sig.clone(),
            })
        }
        _ => None,
    };
    Ok(SentInvitation {
        id: i.id,
        vault_id: i.vault_id,
        object_id: i.object_id,
        user,
        email: i.email,
        role: i.role,
        invited_by: i.invited_by,
        created_at: i.created_at,
        expires_at: i.expires_at,
        signature: i.signature,
        accepted,
        with_comments: i.with_comments,
    })
}

/// An invitation as the person it is for sees it: who sent it, never what
/// it is to (its name is encrypted). For a single note, not the sender's
/// address (people connected only through a note don't see it).
async fn received_view(state: &AppState, i: Invitation) -> AppResult<ReceivedInvitation> {
    let mut from = public_user(&state.db, &i.invited_by).await?;
    if i.object_id.is_some() {
        from.email = None;
    }
    Ok(ReceivedInvitation {
        id: i.id,
        vault_id: i.vault_id,
        object_id: i.object_id,
        from,
        user_id: i.user_id,
        email: i.email,
        role: i.role,
        created_at: i.created_at,
        expires_at: i.expires_at,
        signature: i.signature,
        accepted: i.state == "accepted",
        with_comments: i.with_comments,
    })
}

/// May `who` invite `target` by account? Only someone they could look up
/// (`GET /users/{id}`): people they share with, or anyone in a searchable
/// directory. Others are `404`, as if there were nobody.
async fn may_name(state: &AppState, who: &Principal, target: &str) -> AppResult<()> {
    let p = repo::users::principal(&state.db, target)
        .await?
        .ok_or(AppError::NotFound)?;
    if p.disabled {
        return Err(AppError::NotFound);
    }
    if p.is_agent() {
        return if p.owner_id.as_deref() == Some(who.user_id.as_str()) {
            Err(bad(
                "your agents don't need an invitation: add them to the vault or share the note with them",
            ))
        } else {
            Err(AppError::NotFound)
        };
    }
    let known = repo::users::share_a_vault(&state.db, &who.user_id, target).await?
        || repo::users::share_a_note(&state.db, &who.user_id, target).await?
        || state.instance.settings().user_directory == UserDirectory::Search;
    if known {
        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}

/// `POST /vaults/{v}/invitations` (owner).
pub async fn create(
    State(state): State<AppState>,
    who: Principal,
    Path(v): Path<String>,
    Json(req): Json<CreateInvitationRequest>,
) -> AppResult<(StatusCode, Json<SentInvitation>)> {
    who.require_human()?;
    access::require_owner(&state.db, &v, &who.user_id).await?;
    access::require_shareable(&state.db, &v).await?;
    if !db::valid_id(&req.id) {
        return Err(bad("invalid invitation id"));
    }
    let object = req.object_id.as_deref();
    let role_ok = match object {
        None => matches!(req.role, Role::Admin | Role::Editor | Role::Viewer),
        Some(_) => matches!(req.role, Role::Editor | Role::Viewer),
    };
    if !role_ok {
        return Err(bad(if object.is_some() {
            "notes are shared as editor or viewer"
        } else {
            "invite people as admin, editor or viewer (owners are made from members)"
        }));
    }
    if req.with_comments && object.is_none() {
        return Err(bad("only a note's comments go with it"));
    }
    let email = match (&req.user_id, &req.email) {
        (Some(u), None) => {
            if *u == who.user_id {
                return Err(bad("that's you"));
            }
            may_name(&state, &who, u).await?;
            None
        }
        (None, Some(a)) => {
            Some(invitation::normalize_email(a).ok_or_else(|| bad("not an e-mail address"))?)
        }
        _ => {
            return Err(bad(
                "invite someone by account (user_id) or by address (email)",
            ));
        }
    };
    let now = db::now();
    if (req.created_at - now).abs() > CLOCK_SKEW_MS {
        return Err(bad(
            "the invitation's time is too far from the server's: check the clock",
        ));
    }
    let row = Invitation {
        id: req.id.clone(),
        vault_id: v.clone(),
        object_id: req.object_id.clone(),
        invited_by: who.user_id.clone(),
        user_id: req.user_id.clone(),
        email: email.clone(),
        role: req.role.clone(),
        created_at: req.created_at,
        signature: req.signature.clone(),
        sent_at: now,
        expires_at: now + state.instance.settings().invitation_ms(),
        state: "pending".into(),
        accepted_by: None,
        accepted_at: None,
        acceptance: None,
        with_comments: req.with_comments,
    };
    // Signed by the caller, with the key the server has for them (their
    // apps check it again: this only keeps out what can't be used).
    let key = repo::users::account_key(&state.db, &who.user_id)
        .await?
        .ok_or_else(|| conflict("set up encryption first"))?
        .verify()
        .map_err(|_| conflict("your account key doesn't verify"))?;
    if req.signature.len() != MAX_SIGNATURE_BYTES
        || !invitation::verify(&key, &terms(&row).expect("one of them"), &req.signature)
    {
        return Err(bad("the invitation's signature doesn't verify"));
    }
    if let Some(o) = object {
        check_object(o)?;
    }
    {
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        access::require_owner(&mut *tx, &v, &who.user_id).await?;
        if let Some(o) = object
            && repo::shares::share_from(&mut *tx, &v, o).await?.is_none()
        {
            // (Also when there is no such note.)
            return Err(conflict(
                "start sharing the note (give it a fresh key) first",
            ));
        }
        // Someone they know already in: said, as their apps list them. For
        // an address, nothing is said (it would tell whose it is): they
        // accept, and that's all.
        if let Some(u) = &req.user_id {
            if access::member_role_opt(&mut *tx, &v, u).await?.is_some() {
                return Err(conflict(if object.is_some() {
                    "this person already has access through the vault"
                } else {
                    "already a member"
                }));
            }
            if let Some(o) = object
                && repo::shares::role(&mut *tx, &v, o, u).await?.is_some()
            {
                return Err(conflict("the note is already shared with this person"));
            }
        }
        if repo::invitations::open_for(
            &mut *tx,
            &v,
            object,
            req.user_id.as_deref(),
            email.as_deref(),
            now,
        )
        .await?
        {
            return Err(conflict(
                "invited already: cancel that invitation to send another",
            ));
        }
        let settings = state.instance.settings();
        let (open, today) = repo::invitations::counts(&mut *tx, &who.user_id, now).await?;
        if !settings.invitations_pending.allows_one_more(open) {
            return Err(AppError::LimitReached(format!(
                "you have {open} invitations waiting for an answer, the most this server allows: cancel some, or wait for answers"
            )));
        }
        if !settings.invitations_per_day.allows_one_more(today) {
            return Err(AppError::LimitReached(
                "you sent as many invitations today as this server allows: try again tomorrow"
                    .into(),
            ));
        }
        if !repo::invitations::insert(&mut *tx, &row).await? {
            return Err(conflict("an invitation with this id exists"));
        }
        tx.commit().await?;
    }
    db::audit(
        &state.db,
        Some(&who.user_id),
        None,
        Some(&v),
        "invitation.create",
        req.object_id.as_deref(),
        serde_json::json!({
            "invitation": req.id,
            "role": req.role,
            "user": req.user_id,
        }),
    )
    .await;
    notify(
        &state,
        &v,
        std::slice::from_ref(&req.user_id),
        email.as_deref(),
    )
    .await;
    Ok((StatusCode::CREATED, Json(sent_view(&state, row).await?)))
}

/// `GET /invitations`: to the caller, and to the vaults they own.
pub async fn list(
    State(state): State<AppState>,
    who: Principal,
) -> AppResult<Json<InvitationsResponse>> {
    if !who.is_human() {
        return Ok(Json(InvitationsResponse::default()));
    }
    let now = db::now();
    let email = my_email(&state, &who).await?;
    let mut out = InvitationsResponse::default();
    for i in repo::invitations::received(&state.db, &who.user_id, email.as_deref(), now).await? {
        out.received.push(received_view(&state, i).await?);
    }
    for i in repo::invitations::sent(&state.db, &who.user_id, now).await? {
        out.sent.push(sent_view(&state, i).await?);
    }
    Ok(Json(out))
}

async fn my_email(state: &AppState, who: &Principal) -> AppResult<Option<String>> {
    Ok(repo::users::info(&state.db, &who.user_id)
        .await?
        .and_then(|u| u.email)
        .and_then(|e| invitation::normalize_email(&e)))
}

/// One of the caller's open invitations (`404` otherwise: someone else's
/// is never named).
async fn mine(state: &AppState, who: &Principal, id: &str) -> AppResult<Invitation> {
    who.require_human()?;
    let i = repo::invitations::get(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let email = my_email(state, who).await?;
    let to_me = match i.state.as_str() {
        "pending" => {
            i.user_id.as_deref() == Some(who.user_id.as_str())
                || (i.user_id.is_none() && email.is_some() && i.email == email)
        }
        "accepted" => i.accepted_by.as_deref() == Some(who.user_id.as_str()),
        _ => false,
    };
    if !to_me || i.expires_at <= db::now() {
        return Err(AppError::NotFound);
    }
    Ok(i)
}

/// Whether `user` can already open what `i` is to.
async fn has_access(c: &mut sqlx::SqliteConnection, i: &Invitation, user: &str) -> AppResult<bool> {
    if access::member_role_opt(&mut *c, &i.vault_id, user)
        .await?
        .is_some()
    {
        return Ok(true);
    }
    Ok(match &i.object_id {
        Some(o) => repo::shares::role(&mut *c, &i.vault_id, o, user)
            .await?
            .is_some(),
        None => false,
    })
}

/// `POST /invitations/{id}/accept`: the person signs that they accept, with
/// their account key; an owner's app then adds them.
pub async fn accept(
    State(state): State<AppState>,
    who: Principal,
    Path(id): Path<String>,
    Json(req): Json<AcceptInvitationRequest>,
) -> AppResult<StatusCode> {
    let i = mine(&state, &who, &id).await?;
    if i.state != "pending" {
        return Ok(StatusCode::NO_CONTENT);
    }
    let key = repo::users::account_key(&state.db, &who.user_id)
        .await?
        .ok_or_else(|| conflict("set up encryption first"))?
        .verify()
        .map_err(|_| conflict("your account key doesn't verify"))?;
    let t = terms(&i).ok_or_else(|| bad("malformed invitation"))?;
    if req.signature.len() != MAX_SIGNATURE_BYTES
        || !invitation::verify_acceptance(&key, &t, &who.user_id, &req.signature)
    {
        return Err(bad("the acceptance's signature doesn't verify"));
    }
    let now = db::now();
    {
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        if has_access(&mut tx, &i, &who.user_id).await? {
            // In already: nothing to add.
            repo::invitations::end(&mut *tx, &id).await?;
        } else if repo::invitations::accept(
            &mut *tx,
            &id,
            &who.user_id,
            &req.signature,
            now,
            now + state.instance.settings().invitation_ms(),
        )
        .await?
            == 0
        {
            return Err(AppError::NotFound);
        }
        tx.commit().await?;
    }
    db::audit(
        &state.db,
        Some(&who.user_id),
        None,
        Some(&i.vault_id),
        "invitation.accept",
        i.object_id.as_deref(),
        serde_json::json!({ "invitation": id }),
    )
    .await;
    notify(&state, &i.vault_id, &[Some(who.user_id.clone())], None).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /invitations/{id}/decline`: gone for the person, unanswered for
/// the owners — who aren't told.
pub async fn decline(
    State(state): State<AppState>,
    who: Principal,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let i = mine(&state, &who, &id).await?;
    {
        let _w = state.write_lock.lock().await;
        repo::invitations::decline(&state.db, &id).await?;
    }
    if i.state == "accepted" {
        // They saw it accepted: it looks unanswered again.
        notify(&state, &i.vault_id, &[Some(who.user_id.clone())], None).await;
    } else {
        notify_user(&state, &who.user_id, UserEventKind::InvitationsChanged);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /vaults/{v}/invitations/{id}` (owner): cancel it.
pub async fn cancel(
    State(state): State<AppState>,
    who: Principal,
    Path((v, id)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    who.require_human()?;
    access::require_owner(&state.db, &v, &who.user_id).await?;
    let i = {
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        access::require_owner(&mut *tx, &v, &who.user_id).await?;
        let i = repo::invitations::get(&mut *tx, &id)
            .await?
            .filter(|i| i.vault_id == v && i.state != "ended")
            .ok_or(AppError::NotFound)?;
        repo::invitations::end(&mut *tx, &id).await?;
        tx.commit().await?;
        i
    };
    db::audit(
        &state.db,
        Some(&who.user_id),
        None,
        Some(&v),
        "invitation.cancel",
        i.object_id.as_deref(),
        serde_json::json!({ "invitation": id }),
    )
    .await;
    notify(&state, &v, &[i.user_id, i.accepted_by], i.email.as_deref()).await;
    Ok(StatusCode::NO_CONTENT)
}

/// For `POST …/members` and `…/shares`: someone joins by an invitation
/// they accepted (`id`), as its role; it ends in the same transaction.
pub async fn complete(
    tx: &mut sqlx::Transaction<'static, sqlx::Sqlite>,
    id: Option<&str>,
    vault_id: &str,
    object_id: Option<&str>,
    user_id: &str,
    role: &Role,
) -> AppResult<()> {
    let Some(id) = id else {
        return Err(bad(
            "people join by invitation: invite them (POST /vaults/{v}/invitations), and once they accepted add them with its id",
        ));
    };
    match repo::invitations::accepted_role(&mut **tx, id, vault_id, object_id, user_id, db::now())
        .await?
    {
        Some(r) if r == *role => {}
        Some(_) => return Err(bad("not the role they were invited as")),
        None => return Err(conflict("no accepted invitation for this person")),
    }
    repo::invitations::end(&mut **tx, id).await?;
    Ok(())
}

/// After [`complete`]: tell them and the owners.
pub async fn completed(state: &AppState, vault_id: &str, user_id: &str) {
    notify(state, vault_id, &[Some(user_id.to_string())], None).await;
}

/// Someone stopped being an owner of `vault_id`: their invitations to it
/// end (owners' apps wouldn't complete them any more).
pub async fn end_by(
    tx: &mut sqlx::Transaction<'static, sqlx::Sqlite>,
    vault_id: &str,
    user_id: &str,
) -> AppResult<Vec<Option<String>>> {
    Ok(repo::invitations::end_by(&mut **tx, vault_id, user_id)
        .await?
        .into_iter()
        .flat_map(|(a, b)| [a, b])
        .collect())
}

/// After [`end_by`].
pub async fn ended(state: &AppState, vault_id: &str, people: &[Option<String>]) {
    if !people.is_empty() {
        notify(state, vault_id, people, None).await;
    }
}
