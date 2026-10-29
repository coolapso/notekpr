//! Authorization checks shared by REST and WebSocket handlers.
//!
//! The rows come from [`crate::repo`]; the decisions are made here. Checks
//! that read one row accept any executor, so a write can run them inside its
//! own transaction.
//!
//! # Writes and changing permissions
//!
//! * Every change to memberships, roles or note shares runs under the
//!   server's write lock, in a `BEGIN IMMEDIATE` transaction.
//! * A write whose permission depends on them checks it again *inside* its
//!   write transaction, reading through the transaction (`&mut *tx`); a
//!   check before taking the lock only fails fast. So a write can't land
//!   after the caller was removed or demoted, even with another process
//!   writing the same database.
//! * Where the written data isn't in that transaction (attachment bytes in
//!   [`crate::blobs`]), the check is repeated under the write lock instead.
//! * `sync::push` and `sync::create_snapshot` check under the write lock just
//!   before their transaction. Within one process that is equivalent (every
//!   change above holds the same lock); with several processes writing one
//!   database those checks must move into the transaction too.

use notekpr_core::protocol::{
    INDEX_OBJECT_ID, ObjectKind, ObjectRef, Role, VaultKind, comments_note,
};
use sqlx::SqliteExecutor;

use crate::db::Db;
use crate::error::{AppError, AppResult, bad, forbidden};
use crate::repo;

/// The caller's role in a vault. Non-members get `NotFound` so that vault ids
/// cannot be probed.
pub async fn member_role(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    user_id: &str,
) -> AppResult<Role> {
    member_role_opt(db, vault_id, user_id)
        .await?
        .ok_or(AppError::NotFound)
}

pub async fn member_role_opt(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    user_id: &str,
) -> AppResult<Option<Role>> {
    repo::vaults::member_role(db, vault_id, user_id).await
}

/// Role granted by an individual note share (not by vault membership).
pub async fn share_role(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
    user_id: &str,
) -> AppResult<Option<Role>> {
    repo::shares::role(db, vault_id, object_id, user_id).await
}

/// Access to one object: through vault membership or an individual share.
pub async fn object_role(
    db: &Db,
    vault_id: &str,
    object_id: &str,
    user_id: &str,
) -> AppResult<Role> {
    Ok(object_access(db, vault_id, object_id, user_id).await?.0)
}

/// Like [`object_role`], also telling whether access comes from vault
/// membership (`true`) or only from an individual share (`false`). A note's
/// comments (`c_<note>`) are readable by whoever can read the note.
pub async fn object_access(
    db: &Db,
    vault_id: &str,
    object_id: &str,
    user_id: &str,
) -> AppResult<(Role, bool)> {
    if let Some(r) = member_role_opt(db, vault_id, user_id).await? {
        return Ok((r, true));
    }
    let note = comments_note(object_id).unwrap_or(object_id);
    Ok((
        share_role(db, vault_id, note, user_id)
            .await?
            .ok_or(AppError::NotFound)?,
        false,
    ))
}

/// May `user_id` — a vault member with role `member`, or else someone the
/// note may be shared with — write comments on note `note_id`? Anyone who
/// can read it; viewers (of the vault or of the note) only while the
/// vault's owner lets them.
pub async fn comment_access(
    db: &Db,
    member: Option<Role>,
    vault_id: &str,
    note_id: &str,
    user_id: &str,
) -> AppResult<()> {
    let role = match member {
        Some(r) => r,
        None => share_role(db, vault_id, note_id, user_id)
            .await?
            .ok_or(AppError::NotFound)?,
    };
    let live = matches!(
        repo::shares::object_state(db, vault_id, note_id).await?,
        Some((None, _))
    );
    if note_id == INDEX_OBJECT_ID || comments_note(note_id).is_some() || !live {
        return Err(AppError::NotFound);
    }
    match repo::vaults::kind(db, vault_id).await? {
        Some(VaultKind::QuickNotes) => return Err(forbidden("quick notes have no comments")),
        Some(VaultKind::Drawings) => return Err(forbidden("your drawings have no comments")),
        _ => {}
    }
    if !role.can_write() && !repo::vaults::viewers_comment(db, vault_id).await? {
        return Err(forbidden("the vault's owner doesn't let viewers comment"));
    }
    Ok(())
}

pub async fn require_owner(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    user_id: &str,
) -> AppResult<()> {
    if member_role(db, vault_id, user_id).await?.can_manage() {
        Ok(())
    } else {
        Err(forbidden("only the vault owner can do this"))
    }
}

/// The vault's tasks (emptying the trash, deleting for good, the activity
/// log): its owner and admins.
pub async fn require_admin(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    user_id: &str,
) -> AppResult<()> {
    if member_role(db, vault_id, user_id).await?.can_administer() {
        Ok(())
    } else {
        Err(forbidden("only the vault's owner and admins can do this"))
    }
}

/// Quick notes and Drawings are never shared: no members (people or
/// agents), no note shares. Checked after the caller's own access, so
/// nobody else learns what a vault is.
pub async fn require_shareable(db: impl SqliteExecutor<'_>, vault_id: &str) -> AppResult<()> {
    match repo::vaults::kind(db, vault_id).await? {
        Some(VaultKind::QuickNotes) => Err(bad(
            "quick notes are never shared: move the note to a vault first",
        )),
        Some(VaultKind::Drawings) => Err(bad(
            "your drawings are never shared: move the drawing to a vault first",
        )),
        _ => Ok(()),
    }
}

/// May a new drawing in `vault_id`, made by `user_id`, replace `original`
/// (a move), so that it needs no room under the drawings limit? Only if the
/// original is a live drawing in another vault of the same owner, which
/// the caller could delete for good there (as `DELETE …/objects/{o}`), and
/// no other copy replaces it already. Runs inside the creating transaction.
pub async fn may_replace(
    tx: &mut sqlx::SqliteConnection,
    user_id: &str,
    vault_id: &str,
    original: &ObjectRef,
) -> AppResult<bool> {
    let o = &original.object_id;
    if original.vault_id == vault_id
        || o == INDEX_OBJECT_ID
        || comments_note(o).is_some()
        || repo::shares::live_kind(&mut *tx, &original.vault_id, o).await?
            != Some(ObjectKind::Drawing)
    {
        return Ok(false);
    }
    let from = repo::vaults::kind_and_owner(&mut *tx, &original.vault_id).await?;
    let to = repo::vaults::kind_and_owner(&mut *tx, vault_id).await?;
    let (Some((_, from)), Some((_, to))) = (from, to) else {
        return Ok(false);
    };
    Ok(from == to
        && member_role_opt(&mut *tx, &original.vault_id, user_id)
            .await?
            .is_some_and(|r| r.can_administer())
        && !repo::shares::is_being_replaced(&mut *tx, original).await?)
}

pub async fn require_writer(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    user_id: &str,
) -> AppResult<Role> {
    let role = member_role(db, vault_id, user_id).await?;
    if role.can_write() {
        Ok(role)
    } else {
        Err(forbidden("viewers cannot modify this vault"))
    }
}
