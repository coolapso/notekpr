//! Storage for encrypted attachment bytes, behind one small interface.
//!
//! Today the bytes live in the SQLite database next to their metadata, which
//! keeps a backup a single consistent file. Another store (a directory, S3)
//! can be added as a variant here without touching the routes; the
//! `attachments` table keeps the metadata (size, author, time) either way.

use sqlx::Row;

use crate::db::{self, Db};
use crate::error::{AppError, AppResult, conflict};

pub enum Blobs {
    /// Bytes in the `attachments.data` column.
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
                let r = sqlx::query(
                    "INSERT INTO attachments (vault_id, id, size, author_id, data, created_at) VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(vault_id)
                .bind(id)
                .bind(data.len() as i64)
                .bind(author_id)
                .bind(data)
                .bind(db::now())
                .execute(db)
                .await;
                match r {
                    // Attachments are immutable; re-uploading the same id is a retry.
                    Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                        let size: i64 = sqlx::query_scalar(
                            "SELECT size FROM attachments WHERE vault_id = ? AND id = ?",
                        )
                        .bind(vault_id)
                        .bind(id)
                        .fetch_one(db)
                        .await?;
                        if size == data.len() as i64 {
                            Ok(Stored::AlreadyThere)
                        } else {
                            Err(conflict("attachment id already used"))
                        }
                    }
                    r => {
                        r?;
                        Ok(Stored::Created)
                    }
                }
            }
        }
    }

    pub async fn get(&self, vault_id: &str, id: &str) -> AppResult<Vec<u8>> {
        match self {
            Blobs::Sqlite(db) => {
                sqlx::query_scalar("SELECT data FROM attachments WHERE vault_id = ? AND id = ?")
                    .bind(vault_id)
                    .bind(id)
                    .fetch_optional(db)
                    .await?
                    .ok_or(AppError::NotFound)
            }
        }
    }

    pub async fn delete(&self, vault_id: &str, id: &str) -> AppResult<()> {
        match self {
            Blobs::Sqlite(db) => {
                let n = sqlx::query("DELETE FROM attachments WHERE vault_id = ? AND id = ?")
                    .bind(vault_id)
                    .bind(id)
                    .execute(db)
                    .await?
                    .rows_affected();
                if n == 0 {
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
            Blobs::Sqlite(db) => Ok(sqlx::query(
                "SELECT id, data FROM attachments WHERE vault_id = ? ORDER BY id",
            )
            .bind(vault_id)
            .fetch_all(db)
            .await?
            .iter()
            .map(|r| (r.get("id"), r.get("data")))
            .collect()),
        }
    }
}
