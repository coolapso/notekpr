//! The vault membership log (R3): reading it, and checking the signed entry
//! that comes with each membership or key change.
//!
//! The server can't be trusted with the log — clients verify it themselves —
//! but it keeps honest logs consistent: an entry must continue the log, be
//! signed by the caller with their stored account key, and say exactly what
//! the request does. Once a vault has a log, changes without an entry are
//! refused (leaving excepted: it only reduces access).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use notekpr_core::memberlog::{LogBody, LogEntry, LogMember};
use notekpr_core::protocol::{AppendLogRequest, MemberLogResponse, Role, VaultEventKind};
use serde::Deserialize;
use sqlx::Sqlite;

use crate::access;
use crate::auth::Principal;
use crate::db;
use crate::error::{AppError, AppResult, bad, conflict};
use crate::hub::VaultEvent;
use crate::repo;
use crate::state::AppState;

/// What the entry sent with a change must say.
pub(crate) enum Expect<'a> {
    Create,
    Add {
        user_id: &'a str,
        role: &'a Role,
    },
    Role {
        user_id: &'a str,
        role: &'a Role,
    },
    /// The owner removes someone (`Remove`), or someone leaves (`Leave`).
    Remove {
        user_id: &'a str,
    },
    Rotate {
        epoch: u32,
    },
    /// A note is shared with someone, or their role on it changes.
    Share {
        object_id: &'a str,
        user_id: &'a str,
        role: &'a Role,
    },
    /// The owner stops sharing a note with someone, or they remove it from
    /// their list.
    Unshare {
        object_id: &'a str,
        user_id: &'a str,
    },
    /// `POST /vaults/{v}/log`: `Checkpoint`, `Rekey`, `Share` of a note
    /// already shared (when the log starts), `RequireSigned`, or `Remove` of
    /// someone who is no longer a member.
    Standalone,
}

/// Check `entry` against the log and the change, and append it, inside the
/// change's write transaction.
pub(crate) async fn check_and_append(
    tx: &mut sqlx::Transaction<'static, Sqlite>,
    vault_id: &str,
    who: &Principal,
    entry: Option<&LogEntry>,
    expect: Expect<'_>,
    now: i64,
) -> AppResult<()> {
    let head = repo::member_log::head(&mut **tx, vault_id).await?;
    let Some(e) = entry else {
        return match (&head, &expect) {
            // A vault without a log yet (older clients).
            (None, _) => Ok(()),
            (Some(_), Expect::Remove { user_id }) if *user_id == who.user_id => Ok(()),
            (Some(_), Expect::Unshare { user_id, .. }) if *user_id == who.user_id => Ok(()),
            _ => Err(conflict(
                "this vault's membership is signed; update the app to change it",
            )),
        };
    };
    match &head {
        None if e.seq == 0 && e.prev == [0u8; 32] => {}
        Some((seq, hash)) if e.seq == seq + 1 && e.prev == *hash => {}
        None => {
            return Err(conflict(
                "the vault's membership log has not started; sign a checkpoint first",
            ));
        }
        Some(_) => {
            return Err(conflict(
                "the membership log changed meanwhile; reload and try again",
            ));
        }
    }
    if e.signer != who.user_id {
        return Err(bad("sign membership log entries as yourself"));
    }
    let public = repo::users::account_key(&mut **tx, &who.user_id)
        .await?
        .ok_or_else(|| conflict("set up account keys first"))?
        .verify()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("stored account key: {e}")))?;
    e.verify_signature(vault_id, &public)
        .map_err(|_| bad("invalid membership log signature"))?;
    let commit_ok = |c: &[u8]| c.len() == 32;
    let matches = match (&expect, &e.body) {
        (Expect::Create, LogBody::Create { owner, key_commit }) => {
            owner.user_id == who.user_id
                && owner.role == Role::Owner
                && owner.enc_public == public.enc_public
                && owner.sign_public == public.sign_public
                && commit_ok(key_commit)
        }
        (Expect::Add { user_id, role }, LogBody::Add { member }) => {
            member.user_id == *user_id && member.role == **role && keys_match(tx, member).await?
        }
        (
            Expect::Role { user_id, role },
            LogBody::Role {
                user_id: u,
                role: r,
            },
        ) => u == user_id && r == *role,
        (Expect::Remove { user_id }, LogBody::Remove { user_id: u }) => {
            u == user_id && *user_id != who.user_id
        }
        (Expect::Remove { user_id }, LogBody::Leave { user_id: u }) => {
            u == user_id && *user_id == who.user_id
        }
        (
            Expect::Rotate { epoch },
            LogBody::Rotate {
                epoch: n,
                key_commit,
            },
        ) => n == epoch && commit_ok(key_commit),
        (
            Expect::Standalone,
            LogBody::Checkpoint {
                members,
                epoch,
                key_commit,
            },
        ) => {
            head.is_none()
                && commit_ok(key_commit)
                && repo::vaults::key_epoch(&mut **tx, vault_id).await? == Some(*epoch as i64)
                && checkpoint_matches(tx, vault_id, members).await?
        }
        (Expect::Standalone, LogBody::Rekey { member }) => {
            access::member_role_opt(&mut **tx, vault_id, &member.user_id).await?
                == Some(member.role.clone())
                && !member.enc_public.is_empty()
                && keys_match(tx, member).await?
        }
        (
            Expect::Share {
                object_id,
                user_id,
                role,
            },
            LogBody::Share {
                object_id: o,
                member,
            },
        ) => {
            o == object_id
                && member.user_id == *user_id
                && member.role == **role
                && !member.enc_public.is_empty()
                && keys_match(tx, member).await?
        }
        (
            Expect::Unshare { object_id, user_id },
            LogBody::Unshare {
                object_id: o,
                user_id: u,
            },
        ) => o == object_id && u == user_id && *user_id != who.user_id,
        (Expect::Standalone, LogBody::Share { object_id, member }) => {
            head.is_some()
                && access::share_role(&mut **tx, vault_id, object_id, &member.user_id).await?
                    == Some(member.role.clone())
                && !member.enc_public.is_empty()
                && keys_match(tx, member).await?
        }
        (Expect::Standalone, LogBody::RequireSigned { from_seq }) => {
            let (head_seq, signed) =
                repo::vaults::head_and_signed_from(&mut **tx, vault_id).await?;
            head.is_some() && signed.is_none() && (0..=head_seq).contains(from_seq)
        }
        (Expect::Standalone, LogBody::Remove { user_id }) => {
            *user_id != who.user_id
                && access::member_role_opt(&mut **tx, vault_id, user_id)
                    .await?
                    .is_none()
        }
        _ => false,
    };
    if !matches {
        return Err(bad("the membership log entry does not match the change"));
    }
    if let LogBody::RequireSigned { from_seq } = &e.body {
        repo::vaults::set_signed_from(&mut **tx, vault_id, *from_seq).await?;
    }
    repo::member_log::append(&mut **tx, vault_id, e, now).await
}

/// A member's keys in an entry are the ones the server has for them (empty
/// if they have none yet).
async fn keys_match(tx: &mut sqlx::Transaction<'static, Sqlite>, m: &LogMember) -> AppResult<bool> {
    Ok(
        match repo::users::account_key(&mut **tx, &m.user_id).await? {
            None => m.enc_public.is_empty() && m.sign_public.is_empty(),
            Some(k) => k.enc_public == m.enc_public && k.sign_public == m.sign_public,
        },
    )
}

/// A checkpoint lists exactly the vault's members, with their roles and keys.
async fn checkpoint_matches(
    tx: &mut sqlx::Transaction<'static, Sqlite>,
    vault_id: &str,
    members: &[LogMember],
) -> AppResult<bool> {
    let current = repo::vaults::members(&mut **tx, vault_id).await?;
    if current.len() != members.len() {
        return Ok(false);
    }
    for m in members {
        let Some(c) = current.iter().find(|c| c.user_id == m.user_id) else {
            return Ok(false);
        };
        if c.role != m.role || !keys_match(tx, m).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

#[derive(Deserialize)]
pub struct LogQuery {
    #[serde(default)]
    since: u64,
}

/// The vault's membership log (members only).
pub async fn log(
    State(state): State<AppState>,
    who: Principal,
    Path(v): Path<String>,
    Query(q): Query<LogQuery>,
) -> AppResult<Json<MemberLogResponse>> {
    access::member_role(&state.db, &v, &who.user_id).await?;
    Ok(Json(MemberLogResponse {
        entries: repo::member_log::since(&state.db, &v, q.since).await?,
    }))
}

/// Append an entry that changes nothing else (owner).
pub async fn append(
    State(state): State<AppState>,
    who: Principal,
    Path(v): Path<String>,
    Json(req): Json<AppendLogRequest>,
) -> AppResult<StatusCode> {
    who.require_human()?;
    access::require_owner(&state.db, &v, &who.user_id).await?;
    {
        let _w = state.write_lock.lock().await;
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        access::require_owner(&mut *tx, &v, &who.user_id).await?;
        check_and_append(
            &mut tx,
            &v,
            &who,
            Some(&req.entry),
            Expect::Standalone,
            db::now(),
        )
        .await?;
        tx.commit().await?;
    }
    let kind = match req.entry.body {
        LogBody::Checkpoint { .. } => "member_log.checkpoint",
        LogBody::Rekey { .. } => "member_log.rekey",
        LogBody::Share { .. } => "member_log.share",
        LogBody::RequireSigned { .. } => "member_log.require_signed",
        _ => "member_log.remove",
    };
    db::audit(
        &state.db,
        Some(&who.user_id),
        None,
        Some(&v),
        kind,
        req.entry.body.subject(),
        serde_json::json!({ "seq": req.entry.seq }),
    )
    .await;
    state
        .hub
        .publish(&v, VaultEvent::Event(VaultEventKind::MembersChanged));
    Ok(StatusCode::NO_CONTENT)
}
