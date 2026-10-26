//! Storage for encrypted attachment bytes, behind one small interface.
//!
//! Today the bytes live in the SQLite database next to their metadata, which
//! keeps a backup a single consistent file. Another store (a directory, S3)
//! can be added as a variant here without touching the routes; the
//! `attachments` table keeps the metadata (size, author, time) either way.

use crate::db::{self, Db};
use crate::error::{AppError, AppResult, conflict};
use crate::repo::attachments;

pub enum Blobs {
    /// Bytes in the `attachments.data` column (`repo::attachments`).
    Sqlite(Db),
}

pub enum Stored {
    Created,
    /// Same id and size already stored: an idempotent retry.
    AlreadyThere,
}

impl Blobs {
    pub async fn put(
        &self,
        vault_id: &str,
        id: &str,
        author_id: &str,
        data: &[u8],
    ) -> AppResult<Stored> {
        match self {
            Blobs::Sqlite(db) => {
                if attachments::insert(db, vault_id, id, author_id, data, db::now()).await? {
                    return Ok(Stored::Created);
                }
                // Attachments are immutable; re-uploading the same id is a retry.
                if attachments::size(db, vault_id, id).await? == data.len() as i64 {
                    Ok(Stored::AlreadyThere)
                } else {
                    Err(conflict("attachment id already used"))
                }
            }
        }
    }

    pub async fn get(&self, vault_id: &str, id: &str) -> AppResult<Vec<u8>> {
        match self {
            Blobs::Sqlite(db) => attachments::data(db, vault_id, id)
                .await?
                .ok_or(AppError::NotFound),
        }
    }

    /// Delete for good, undoable for a while: the bytes stay, out of
    /// reach, until [`erase_purged_before`](Self::erase_purged_before).
    /// Repeating it is fine; `NotFound` if there is no such attachment.
    pub async fn purge(&self, vault_id: &str, id: &str, now: i64) -> AppResult<()> {
        match self {
            Blobs::Sqlite(db) => {
                if attachments::purge(db, vault_id, id, now).await? {
                    Ok(())
                } else {
                    Err(AppError::NotFound)
                }
            }
        }
    }

    /// Undo [`purge`](Self::purge) if that was at `since` or later;
    /// `NotFound` otherwise.
    pub async fn restore(&self, vault_id: &str, id: &str, since: i64) -> AppResult<()> {
        match self {
            Blobs::Sqlite(db) => {
                if attachments::restore(db, vault_id, id, since).await? {
                    Ok(())
                } else {
                    Err(AppError::NotFound)
                }
            }
        }
    }

    /// Attachments of a vault deleted for good at `since` or later, as
    /// `(id, purged_at)`, newest first.
    pub async fn purged_since(&self, vault_id: &str, since: i64) -> AppResult<Vec<(String, i64)>> {
        match self {
            Blobs::Sqlite(db) => attachments::purged_since(db, vault_id, since).await,
        }
    }

    /// Delete for real what was deleted for good before `before`.
    pub async fn erase_purged_before(&self, before: i64) -> AppResult<u64> {
        match self {
            Blobs::Sqlite(db) => attachments::erase_purged_before(db, before).await,
        }
    }

    pub async fn delete(&self, vault_id: &str, id: &str) -> AppResult<()> {
        match self {
            Blobs::Sqlite(db) => {
                if attachments::delete(db, vault_id, id).await? == 0 {
                    Err(AppError::NotFound)
                } else {
                    Ok(())
                }
            }
        }
    }

    /// Every attachment of a vault, for exports.
    pub async fn all(&self, vault_id: &str) -> AppResult<Vec<(String, Vec<u8>)>> {
        match self {
            Blobs::Sqlite(db) => attachments::all(db, vault_id).await,
        }
    }
}
