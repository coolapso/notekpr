//! Storage for encrypted attachment bytes, behind one small interface.
//!
//! The database keeps every attachment's row (`attachments`: size, author,
//! times, deleted for good) and, with the database store, its bytes too: a
//! backup is then one consistent file. With a file store (a folder, or
//! S3-compatible storage: `--file-store`), the bytes are a file there under
//! a name of their own (`file_key`: the vault, the attachment and a random
//! part, so two uploads never write the same file), and the rows stay what
//! counts.
//!
//! Storing takes two steps: [`Blobs::stage`] records the upload, then
//! writes its file before the write lock (a slow upload holds nobody up);
//! [`Blobs::commit`] adds the row under the lock and claims the file. A
//! file never claimed — refused, a retry of an attachment stored already, a
//! crash in between — is deleted by [`Blobs::delete_due`] after a while.
//! Deleting a row records its file in the same transaction (a trigger, so
//! deleted vaults and accounts too), deleted from the store after
//! `--keep-deleted-files-days`, so a recent backup of the database still
//! finds its files. The store is never listed: the server deletes only
//! files it recorded, so a bucket can hold several servers' files under
//! prefixes.
//!
//! Attachments still in the database when the server runs with a file store
//! move there by themselves ([`Blobs::move_out`]).

use std::sync::Arc;

use anyhow::Context;
use axum::body::{Body, Bytes};
use object_store::aws::AmazonS3Builder;
use object_store::local::LocalFileSystem;
use object_store::path::Path;
use object_store::prefix::PrefixStore;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};

use crate::config::{Config, FileStore};
use crate::db::{self, Db};
use crate::error::{AppError, AppResult, conflict};
use crate::repo::attachments;

/// An upload never claimed by a row is deleted this long after it started.
const UNCLAIMED_MS: i64 = 3600 * 1000;
/// Files deleted from the store per transaction.
const DELETE_BATCH: i64 = 200;

pub struct Blobs {
    db: Db,
    files: Option<Files>,
    keep_deleted_ms: i64,
}

struct Files {
    store: Arc<dyn ObjectStore>,
    /// For logs: the folder, or the bucket and prefix (never credentials).
    name: String,
}

pub enum Stored {
    Created,
    /// Same id and size already stored: an idempotent retry.
    AlreadyThere,
}

/// An attachment on its way in ([`Blobs::stage`]).
pub struct Staged {
    vault_id: String,
    id: String,
    size: usize,
    /// The database store's bytes, written by [`Blobs::commit`].
    data: Option<Bytes>,
    /// The file store's file, written already.
    key: Option<String>,
}

impl Blobs {
    /// The store `config` names. Refused if the database names files and
    /// the store holds none of its newest (another store, or none), or if
    /// it is the database store while files are elsewhere.
    pub async fn open(config: &Config, db: Db) -> anyhow::Result<Self> {
        let files = match config.file_store() {
            FileStore::Database => None,
            FileStore::Directory(dir) => {
                std::fs::create_dir_all(&dir)
                    .with_context(|| format!("creating the file store {}", dir.display()))?;
                let store = LocalFileSystem::new_with_prefix(&dir)
                    .with_context(|| format!("the file store {}", dir.display()))?
                    .with_fsync(true)
                    .with_automatic_cleanup(true);
                Some(Files {
                    store: Arc::new(store),
                    name: dir.display().to_string(),
                })
            }
            FileStore::S3 { bucket, prefix } => {
                let s3 = AmazonS3Builder::from_env()
                    .with_bucket_name(&bucket)
                    .build()
                    .context("the S3 file store")?;
                let store: Arc<dyn ObjectStore> = if prefix.is_empty() {
                    Arc::new(s3)
                } else {
                    Arc::new(PrefixStore::new(s3, prefix.as_str()))
                };
                Some(Files {
                    store,
                    name: format!("s3://{bucket}/{prefix}"),
                })
            }
        };
        let blobs = Blobs {
            db,
            files,
            keep_deleted_ms: config.keep_deleted_files_ms(),
        };
        blobs.check().await?;
        Ok(blobs)
    }

    async fn check(&self) -> anyhow::Result<()> {
        let keys = attachments::newest_file_keys(&self.db, 5).await?;
        let Some(files) = &self.files else {
            anyhow::ensure!(
                keys.is_empty(),
                "attachments are kept in a file store, not in the database: start the server with \
                 --file-store naming that store (moving them back into the database isn't supported)"
            );
            return Ok(());
        };
        tracing::info!(store = %files.name, "attachments are kept in a file store");
        if keys.is_empty() {
            return Ok(());
        }
        for key in &keys {
            match files.store.head(&Path::from(key.as_str())).await {
                Ok(_) => return Ok(()),
                Err(object_store::Error::NotFound { .. }) => {}
                // Unreachable for now: attachments wait, the rest works.
                Err(e) => {
                    tracing::warn!(error = %e, store = %files.name, "could not check the file store");
                    return Ok(());
                }
            }
        }
        anyhow::bail!(
            "the file store {} holds none of this server's newest attachments: \
             is --file-store the store this database uses?",
            files.name
        )
    }

    /// First step of storing an attachment, before the write lock: a file
    /// store's file is written (recorded first, so it is deleted unless
    /// [`commit`](Self::commit) claims it); the database store's bytes
    /// wait for `commit`.
    pub async fn stage(&self, vault_id: &str, id: &str, data: Bytes) -> AppResult<Staged> {
        let mut staged = Staged {
            vault_id: vault_id.to_string(),
            id: id.to_string(),
            size: data.len(),
            data: None,
            key: None,
        };
        let Some(files) = &self.files else {
            staged.data = Some(data);
            return Ok(staged);
        };
        let key = format!(
            "{vault_id}/{id}.{}",
            data_encoding::HEXLOWER.encode(&notekpr_core::crypto::random_bytes::<8>())
        );
        attachments::record_upload(&self.db, &key, db::now()).await?;
        files
            .store
            .put(&Path::from(key.as_str()), PutPayload::from_bytes(data))
            .await
            .map_err(|e| files.error(e))?;
        staged.key = Some(key);
        Ok(staged)
    }

    /// Second step, under the write lock: the attachment's row.
    pub async fn commit(&self, staged: &Staged, author_id: &str) -> AppResult<Stored> {
        let (v, id) = (staged.vault_id.as_str(), staged.id.as_str());
        let now = db::now();
        let created = match (&staged.key, &staged.data) {
            (Some(key), _) => {
                let mut tx = self.db.begin_with("BEGIN IMMEDIATE").await?;
                // Unclaimed for too long, its file is gone or going.
                if !attachments::claim_upload(&mut *tx, key).await? {
                    return Err(AppError::Unavailable(
                        "the upload took too long: send it again".into(),
                    ));
                }
                let row =
                    attachments::insert_file(&mut *tx, v, id, staged.size, author_id, key, now);
                if !row.await? {
                    // Rolled back: the upload stays recorded, to delete.
                    false
                } else {
                    tx.commit().await?;
                    true
                }
            }
            (None, Some(data)) => {
                attachments::insert(&self.db, v, id, author_id, data, now).await?
            }
            (None, None) => unreachable!("staged without bytes"),
        };
        if created {
            return Ok(Stored::Created);
        }
        // Attachments are immutable; re-uploading the same id is a retry.
        if attachments::size(&self.db, v, id).await? == Some(staged.size as i64) {
            Ok(Stored::AlreadyThere)
        } else {
            Err(conflict("attachment id already used"))
        }
    }

    /// Let go of a staged attachment that wasn't stored: its file is
    /// deleted now (or, failing that, with the unclaimed ones).
    pub async fn discard(&self, staged: Staged) {
        if let (Some(files), Some(key)) = (&self.files, staged.key) {
            match files.store.delete(&Path::from(key.as_str())).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {
                    let _ = attachments::forget_deletion(&self.db, &key).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, store = %files.name, "could not delete an unused file")
                }
            }
        }
    }

    /// An attachment's size and bytes (streamed from a file store).
    pub async fn get(&self, vault_id: &str, id: &str) -> AppResult<(u64, Body)> {
        let (key, data) = attachments::stored(&self.db, vault_id, id)
            .await?
            .ok_or(AppError::NotFound)?;
        let Some(key) = key else {
            return Ok((data.len() as u64, Body::from(data)));
        };
        let files = self.files(&key)?;
        let file = files.get(&key).await?;
        let size = file.meta.size;
        Ok((size, Body::from_stream(file.into_stream())))
    }

    /// Delete for good, undoable for a while: the bytes stay, out of
    /// reach, until [`erase_purged_before`](Self::erase_purged_before).
    /// Repeating it is fine; `NotFound` if there is no such attachment.
    pub async fn purge(&self, vault_id: &str, id: &str, now: i64) -> AppResult<()> {
        if attachments::purge(&self.db, vault_id, id, now).await? {
            Ok(())
        } else {
            Err(AppError::NotFound)
        }
    }

    /// Undo [`purge`](Self::purge) if that was at `since` or later;
    /// `NotFound` otherwise.
    pub async fn restore(&self, vault_id: &str, id: &str, since: i64) -> AppResult<()> {
        if attachments::restore(&self.db, vault_id, id, since).await? {
            Ok(())
        } else {
            Err(AppError::NotFound)
        }
    }

    /// Attachments of a vault deleted for good at `since` or later, as
    /// `(id, purged_at)`, newest first.
    pub async fn purged_since(&self, vault_id: &str, since: i64) -> AppResult<Vec<(String, i64)>> {
        attachments::purged_since(&self.db, vault_id, since).await
    }

    /// Delete for real what was deleted for good before `before` (a file
    /// store's files later: [`delete_due`](Self::delete_due)).
    pub async fn erase_purged_before(&self, before: i64) -> AppResult<u64> {
        attachments::erase_purged_before(&self.db, before).await
    }

    pub async fn delete(&self, vault_id: &str, id: &str) -> AppResult<()> {
        if attachments::delete(&self.db, vault_id, id).await? == 0 {
            Err(AppError::NotFound)
        } else {
            Ok(())
        }
    }

    /// Every attachment of a vault, for exports.
    pub async fn all(&self, vault_id: &str) -> AppResult<Vec<(String, Vec<u8>)>> {
        let mut out = Vec::new();
        for (id, key, data) in attachments::all(&self.db, vault_id).await? {
            match key {
                None => out.push((id, data)),
                Some(key) => {
                    let files = self.files(&key)?;
                    let file = files.get(&key).await?;
                    let bytes = file.bytes().await.map_err(|e| files.error(e))?;
                    out.push((id, bytes.to_vec()));
                }
            }
        }
        Ok(out)
    }

    /// Delete from the file store what is due: uploads never claimed, and
    /// files the database let go of longer ago than the grace period.
    /// Returns how many.
    pub async fn delete_due(&self, now: i64) -> AppResult<u64> {
        let Some(files) = &self.files else {
            return Ok(0);
        };
        let mut n = 0;
        loop {
            let due = attachments::due_deletions(
                &self.db,
                now - UNCLAIMED_MS,
                now - self.keep_deleted_ms,
                DELETE_BATCH,
            )
            .await?;
            for (key, since, upload) in &due {
                // Taken off the list first, so an upload can't be claimed
                // while its file goes.
                if !attachments::take_deletion(&self.db, key, *upload).await? {
                    continue;
                }
                match files.store.delete(&Path::from(key.as_str())).await {
                    Ok(()) | Err(object_store::Error::NotFound { .. }) => n += 1,
                    Err(e) => {
                        attachments::record_deletion(&self.db, key, *since, *upload).await?;
                        return Err(files.error(e));
                    }
                }
            }
            if (due.len() as i64) < DELETE_BATCH {
                return Ok(n);
            }
        }
    }

    /// Move attachments kept in the database to the file store, one at a
    /// time (resumable: each is moved in its own transaction, and its bytes
    /// leave the database then). Returns how many.
    pub async fn move_out(&self) -> AppResult<u64> {
        let Some(files) = &self.files else {
            return Ok(0);
        };
        let left = attachments::count_in_database(&self.db).await?;
        if left == 0 {
            return Ok(0);
        }
        tracing::info!(attachments = left, store = %files.name, "moving attachments to the file store");
        let mut n = 0;
        loop {
            let batch = attachments::in_database(&self.db, 50).await?;
            if batch.is_empty() {
                break;
            }
            for (v, id) in batch {
                // Deleted meanwhile.
                let Some(data) = attachments::database_bytes(&self.db, &v, &id).await? else {
                    continue;
                };
                let staged = self.stage(&v, &id, data.into()).await?;
                let key = staged.key.clone().expect("a file store stages files");
                let mut tx = self.db.begin_with("BEGIN IMMEDIATE").await?;
                if attachments::claim_upload(&mut *tx, &key).await?
                    && attachments::moved_to_file(&mut *tx, &v, &id, &key).await?
                {
                    tx.commit().await?;
                    n += 1;
                } else {
                    drop(tx);
                    self.discard(staged).await;
                }
            }
        }
        tracing::info!(attachments = n, store = %files.name, "moved attachments to the file store");
        Ok(n)
    }

    fn files(&self, key: &str) -> AppResult<&Files> {
        self.files.as_ref().ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!(
                "attachment file {key} is in a file store, but none is configured"
            ))
        })
    }
}

impl Files {
    async fn get(&self, key: &str) -> AppResult<object_store::GetResult> {
        match self.store.get(&Path::from(key)).await {
            Ok(file) => Ok(file),
            Err(object_store::Error::NotFound { .. }) => {
                tracing::error!(store = %self.name, file = key, "an attachment's file is missing from the file store");
                Err(AppError::NotFound)
            }
            Err(e) => Err(self.error(e)),
        }
    }

    /// The store can't be reached or refused: clients try again later.
    fn error(&self, e: object_store::Error) -> AppError {
        tracing::warn!(error = %e, store = %self.name, "file store");
        AppError::Unavailable("attachments can't be stored or read right now".into())
    }
}
