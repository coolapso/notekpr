//! Key envelopes the server stores but cannot open: vault keys encrypted to
//! each member per epoch, and the document keys of individually shared
//! notes (wrapped under the vault key for members — `user_id = ''` — and
//! encrypted to each recipient), plus the chains linking each epoch to the
//! previous one.

use coolpsonotes_core::protocol::{
    ChainLink, KeyEnvelope, ObjectChainLink, ObjectKeyEnvelope, ObjectKeysResponse, Role,
    VaultKeysResponse,
};
use sqlx::{Row, SqliteExecutor};

use crate::db::Db;
use crate::error::{AppError, AppResult};

// --- Vault keys ---

/// A member's envelopes for every epoch, the key chain, and the document
/// keys of the vault's individually shared notes (callers check membership).
pub async fn vault_keys(db: &Db, vault_id: &str, user_id: &str) -> AppResult<VaultKeysResponse> {
    let current: i64 = sqlx::query_scalar("SELECT key_epoch FROM vaults WHERE id = ?")
        .bind(vault_id)
        .fetch_one(db)
        .await?;
    let envelopes = sqlx::query(
        "SELECT epoch, envelope FROM vault_keys WHERE vault_id = ? AND user_id = ? ORDER BY epoch",
    )
    .bind(vault_id)
    .bind(user_id)
    .fetch_all(db)
    .await?
    .iter()
    .map(|r| KeyEnvelope {
        user_id: user_id.to_string(),
        epoch: r.get::<i64, _>("epoch") as u32,
        envelope: r.get("envelope"),
        signature: None,
    })
    .collect();
    let chain =
        sqlx::query("SELECT epoch, link FROM vault_key_chain WHERE vault_id = ? ORDER BY epoch")
            .bind(vault_id)
            .fetch_all(db)
            .await?
            .iter()
            .map(|r| ChainLink {
                epoch: r.get::<i64, _>("epoch") as u32,
                link: r.get("link"),
            })
            .collect();
    // Document keys of notes, wrapped under vault keys.
    let objects = sqlx::query("SELECT object_id, epoch, envelope FROM object_keys WHERE vault_id = ? AND user_id = '' ORDER BY object_id, epoch")
        .bind(vault_id)
        .fetch_all(db)
        .await?
        .iter()
        .map(|r| ObjectKeyEnvelope { object_id: r.get("object_id"), epoch: r.get::<i64, _>("epoch") as u32, envelope: r.get("envelope") })
        .collect();
    let object_chain = sqlx::query("SELECT object_id, epoch, link FROM object_key_chain WHERE vault_id = ? ORDER BY object_id, epoch")
        .bind(vault_id)
        .fetch_all(db)
        .await?
        .iter()
        .map(|r| ObjectChainLink { object_id: r.get("object_id"), epoch: r.get::<i64, _>("epoch") as u32, link: r.get("link") })
        .collect();
    let shared = super::shares::shared_in_vault(db, vault_id).await?;
    Ok(VaultKeysResponse {
        current_epoch: current as u32,
        envelopes,
        chain,
        objects,
        object_chain,
        shared: Some(shared),
    })
}

pub async fn insert_vault_key(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    epoch: i64,
    user_id: &str,
    envelope: &[u8],
    created_by: &str,
    now: i64,
) -> AppResult<()> {
    sqlx::query("INSERT INTO vault_keys (vault_id, epoch, user_id, envelope, created_by, created_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(vault_id)
        .bind(epoch)
        .bind(user_id)
        .bind(envelope)
        .bind(created_by)
        .bind(now)
        .execute(db)
        .await?;
    Ok(())
}

/// Like [`insert_vault_key`], replacing an envelope for the same epoch.
pub async fn put_vault_key(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    epoch: i64,
    user_id: &str,
    envelope: &[u8],
    created_by: &str,
    now: i64,
) -> AppResult<()> {
    sqlx::query(
        "INSERT OR REPLACE INTO vault_keys (vault_id, epoch, user_id, envelope, created_by, created_at) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(vault_id)
    .bind(epoch)
    .bind(user_id)
    .bind(envelope)
    .bind(created_by)
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

/// Drop every vault-key envelope of a (removed) member.
pub async fn delete_vault_keys_of(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    user_id: &str,
) -> AppResult<()> {
    sqlx::query("DELETE FROM vault_keys WHERE vault_id = ? AND user_id = ?")
        .bind(vault_id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn insert_vault_chain_link(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    epoch: i64,
    link: &[u8],
) -> AppResult<()> {
    sqlx::query("INSERT INTO vault_key_chain (vault_id, epoch, link) VALUES (?, ?, ?)")
        .bind(vault_id)
        .bind(epoch)
        .bind(link)
        .execute(db)
        .await?;
    Ok(())
}

// --- Document keys of individually shared notes ---

/// One person's envelopes for a shared note, its key chain and encrypted
/// title (callers check access). `NotFound` if the note isn't shared.
pub async fn document_keys(
    db: &Db,
    vault_id: &str,
    object_id: &str,
    user_id: &str,
    role: Role,
) -> AppResult<ObjectKeysResponse> {
    let row: Option<(Option<i64>, Option<Vec<u8>>)> =
        sqlx::query_as("SELECT key_epoch, encrypted_meta FROM objects WHERE vault_id = ? AND id = ? AND purged_at IS NULL")
            .bind(vault_id)
            .bind(object_id)
            .fetch_optional(db)
            .await?;
    let Some((Some(epoch), meta)) = row else {
        return Err(AppError::NotFound);
    };
    let envelopes = sqlx::query("SELECT epoch, envelope, signature FROM object_keys WHERE vault_id = ? AND object_id = ? AND user_id = ? ORDER BY epoch")
        .bind(vault_id)
        .bind(object_id)
        .bind(user_id)
        .fetch_all(db)
        .await?
        .iter()
        .map(|r| KeyEnvelope { user_id: user_id.to_string(), epoch: r.get::<i64, _>("epoch") as u32, envelope: r.get("envelope"), signature: r.get("signature") })
        .collect();
    // Links within the shared epochs only (sharing started with a key that
    // has no link to the note's earlier, private keys).
    let from = super::shares::share_from(db, vault_id, object_id)
        .await?
        .unwrap_or(coolpsonotes_core::keys::FIRST_DOC_EPOCH);
    let chain = sqlx::query("SELECT epoch, link FROM object_key_chain WHERE vault_id = ? AND object_id = ? AND epoch > ? ORDER BY epoch")
        .bind(vault_id)
        .bind(object_id)
        .bind(from as i64)
        .fetch_all(db)
        .await?
        .iter()
        .map(|r| ChainLink { epoch: r.get::<i64, _>("epoch") as u32, link: r.get("link") })
        .collect();
    Ok(ObjectKeysResponse {
        object_id: object_id.to_string(),
        role,
        current_epoch: epoch as u32,
        envelopes,
        chain,
        encrypted_meta: meta,
    })
}

/// A document-key envelope; `user_id = ""` is the copy wrapped under the
/// vault key for members.
#[allow(clippy::too_many_arguments)]
pub async fn insert_document_key(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
    epoch: i64,
    user_id: &str,
    envelope: &[u8],
    created_by: &str,
    now: i64,
) -> AppResult<()> {
    sqlx::query("INSERT INTO object_keys (vault_id, object_id, epoch, user_id, envelope, created_by, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(vault_id)
        .bind(object_id)
        .bind(epoch)
        .bind(user_id)
        .bind(envelope)
        .bind(created_by)
        .bind(now)
        .execute(db)
        .await?;
    Ok(())
}

/// Like [`insert_document_key`], replacing an envelope for the same epoch.
#[allow(clippy::too_many_arguments)]
pub async fn put_document_key(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
    epoch: i64,
    user_id: &str,
    envelope: &[u8],
    created_by: &str,
    now: i64,
) -> AppResult<()> {
    sqlx::query(
        "INSERT OR REPLACE INTO object_keys (vault_id, object_id, epoch, user_id, envelope, created_by, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(vault_id)
    .bind(object_id)
    .bind(epoch)
    .bind(user_id)
    .bind(envelope)
    .bind(created_by)
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

/// Store the owner's grant signature for someone's key of a note (not for
/// the members' copy, `user_id = ''`). Returns whether such a key exists.
pub async fn set_grant_signature(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
    user_id: &str,
    epoch: i64,
    signature: &[u8],
) -> AppResult<bool> {
    let r = sqlx::query(
        "UPDATE object_keys SET signature = ? WHERE vault_id = ? AND object_id = ? AND user_id = ? AND epoch = ? AND user_id != ''",
    )
    .bind(signature)
    .bind(vault_id)
    .bind(object_id)
    .bind(user_id)
    .bind(epoch)
    .execute(db)
    .await?;
    Ok(r.rows_affected() == 1)
}

pub async fn insert_document_chain_link(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
    epoch: i64,
    link: &[u8],
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO object_key_chain (vault_id, object_id, epoch, link) VALUES (?, ?, ?, ?)",
    )
    .bind(vault_id)
    .bind(object_id)
    .bind(epoch)
    .bind(link)
    .execute(db)
    .await?;
    Ok(())
}

/// Drop a recipient's envelopes for one note.
pub async fn delete_document_keys_of(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
    user_id: &str,
) -> AppResult<()> {
    sqlx::query("DELETE FROM object_keys WHERE vault_id = ? AND object_id = ? AND user_id = ?")
        .bind(vault_id)
        .bind(object_id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Drop a recipient's envelopes for every note of a vault.
pub async fn delete_vault_document_keys_of(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    user_id: &str,
) -> AppResult<()> {
    sqlx::query("DELETE FROM object_keys WHERE vault_id = ? AND user_id = ?")
        .bind(vault_id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(())
}
