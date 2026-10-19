//! Authorization checks shared by REST and WebSocket handlers.
//!
//! The rows come from [`crate::repo`]; the decisions are made here. Checks
//! that read one row accept any executor, so a write can run them inside its
//! own transaction.

use coolpsonotes_core::protocol::Role;
use sqlx::SqliteExecutor;

use crate::db::Db;
use crate::error::{AppError, AppResult, forbidden};
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
/// membership (`true`) or only from an individual share (`false`).
pub async fn object_access(
    db: &Db,
    vault_id: &str,
    object_id: &str,
    user_id: &str,
) -> AppResult<(Role, bool)> {
    if let Some(r) = member_role_opt(db, vault_id, user_id).await? {
        return Ok((r, true));
    }
    Ok((
        share_role(db, vault_id, object_id, user_id)
            .await?
            .ok_or(AppError::NotFound)?,
        false,
    ))
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
