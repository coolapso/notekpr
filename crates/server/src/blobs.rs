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
