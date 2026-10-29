//! Individually shared notes: the note's document-key state and encrypted
//! title (columns of `objects`), and who it is shared with (`object_shares`).
//! Also what an object is (a drawing?).

use notekpr_core::protocol::{ObjectKind, Role};
use sqlx::{Row, SqliteExecutor};

use crate::error::AppResult;

/// `(purged_at, key_epoch)` of an object, `None` if it was never written.
pub async fn object_state(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
) -> AppResult<Option<(Option<i64>, Option<i64>)>> {
    Ok(
        sqlx::query_as("SELECT purged_at, key_epoch FROM objects WHERE vault_id = ? AND id = ?")
            .bind(vault_id)
            .bind(object_id)
            .fetch_optional(db)
            .await?,
    )
}

/// Create the object row if it doesn't exist yet (its first key, or
/// sharing a note nobody wrote to). `kind` counts only then: an object's
/// kind never changes.
pub async fn ensure_object(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
    created_by: &str,
    kind: ObjectKind,
    now: i64,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO objects (vault_id, id, created_by, created_at, updated_at, kind) VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(vault_id, id) DO NOTHING",
    )
    .bind(vault_id)
    .bind(object_id)
    .bind(created_by)
    .bind(now)
    .bind(now)
    .bind(kind.as_str())
    .execute(db)
    .await?;
    Ok(())
}

/// What an object is, also one deleted for good; `None` if there's none.
pub async fn kind(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
) -> AppResult<Option<ObjectKind>> {
    let kind: Option<String> =
        sqlx::query_scalar("SELECT kind FROM objects WHERE vault_id = ? AND id = ?")
            .bind(vault_id)
            .bind(object_id)
            .fetch_optional(db)
            .await?;
    Ok(kind.map(|k| ObjectKind::parse(&k).unwrap_or_default()))
}

/// Move a note to a document key epoch, with its title under that key. No
/// one who lost access knows the new key.
pub async fn set_document_epoch(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
    epoch: i64,
    encrypted_meta: &[u8],
    now: i64,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE objects SET key_epoch = ?, encrypted_meta = ?, rotation_pending = 0, updated_at = ?
         WHERE vault_id = ? AND id = ?",
    )
    .bind(epoch)
    .bind(encrypted_meta)
    .bind(now)
    .bind(vault_id)
    .bind(object_id)
    .execute(db)
    .await?;
    Ok(())
}

/// Someone who lost access to a note still knows its current key: the
/// vault owner's apps rotate it (which clears this).
pub async fn set_rotation_pending(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
) -> AppResult<()> {
    sqlx::query("UPDATE objects SET rotation_pending = 1 WHERE vault_id = ? AND id = ?")
        .bind(vault_id)
        .bind(object_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Every note whose key someone going holds (an agent deleted).
pub async fn set_rotation_pending_for_holder(
    db: impl SqliteExecutor<'_>,
    user_id: &str,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE objects SET rotation_pending = 1
         WHERE (vault_id, id) IN (SELECT vault_id, object_id FROM object_keys WHERE user_id = ?)",
    )
    .bind(user_id)
    .execute(db)
    .await?;
    Ok(())
}

/// Notes whose key is to be rotated, among those that can be: deleted for
/// good ones wait until they are brought back (a vault rotation moves them
/// too).
pub async fn rotation_pending(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM objects
         WHERE vault_id = ? AND rotation_pending = 1 AND key_epoch IS NOT NULL AND purged_at IS NULL
         ORDER BY id",
    )
    .bind(vault_id)
    .fetch_all(db)
    .await?)
}

pub async fn set_title(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
    encrypted_meta: &[u8],
) -> AppResult<()> {
    sqlx::query("UPDATE objects SET encrypted_meta = ? WHERE vault_id = ? AND id = ?")
        .bind(encrypted_meta)
        .bind(vault_id)
        .bind(object_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Highest sequence number of the object's updates still stored.
pub async fn last_update_seq(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
) -> AppResult<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT MAX(seq) FROM updates WHERE vault_id = ? AND object_id = ?")
            .bind(vault_id)
            .bind(object_id)
            .fetch_one(db)
            .await?,
    )
}

/// Highest `covers_seq` of the object's snapshots.
pub async fn newest_snapshot_covers(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
) -> AppResult<Option<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT MAX(covers_seq) FROM snapshots WHERE vault_id = ? AND object_id = ?",
    )
    .bind(vault_id)
    .bind(object_id)
    .fetch_one(db)
    .await?)
}

// --- Shares ---

/// Role granted by an individual share of a note that isn't purged (not by
/// vault membership).
pub async fn role(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
    user_id: &str,
) -> AppResult<Option<Role>> {
    let role: Option<String> = sqlx::query_scalar(
        "SELECT s.role FROM object_shares s JOIN objects o ON o.vault_id = s.vault_id AND o.id = s.object_id
         WHERE s.vault_id = ? AND s.object_id = ? AND s.user_id = ? AND o.purged_at IS NULL",
    )
    .bind(vault_id)
    .bind(object_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    Ok(role.and_then(|r| Role::parse(&r)))
}

/// A note shared with someone, as listed for them.
pub struct SharedWith {
    pub vault_id: String,
    pub object_id: String,
    pub role: Role,
    pub added_by: Option<String>,
    pub added_at: i64,
    pub key_epoch: Option<i64>,
    pub encrypted_meta: Option<Vec<u8>>,
    pub vault_owner_id: String,
}

/// Notes shared with a user individually (not purged), newest first.
pub async fn shared_with(db: impl SqliteExecutor<'_>, user_id: &str) -> AppResult<Vec<SharedWith>> {
    let rows = sqlx::query(
        "SELECT s.vault_id, s.object_id, s.role, s.added_by, s.added_at, o.key_epoch, o.encrypted_meta, v.owner_id
         FROM object_shares s
         JOIN objects o ON o.vault_id = s.vault_id AND o.id = s.object_id AND o.purged_at IS NULL
         JOIN vaults v ON v.id = s.vault_id
         WHERE s.user_id = ? ORDER BY s.added_at DESC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| SharedWith {
            vault_id: r.get("vault_id"),
            object_id: r.get("object_id"),
            role: Role::parse(r.get("role")).unwrap_or(Role::Viewer),
            added_by: r.get("added_by"),
            added_at: r.get("added_at"),
            key_epoch: r.get("key_epoch"),
            encrypted_meta: r.get("encrypted_meta"),
            vault_owner_id: r.get("owner_id"),
        })
        .collect())
}

/// `(vault id, object id, role)` of notes shared with a user that the user
/// can't already read as a vault member (for exports).
pub async fn exportable(
    db: impl SqliteExecutor<'_>,
    user_id: &str,
) -> AppResult<Vec<(String, String, Role)>> {
    let rows = sqlx::query(
        "SELECT s.vault_id, s.object_id, s.role FROM object_shares s
         JOIN objects o ON o.vault_id = s.vault_id AND o.id = s.object_id AND o.purged_at IS NULL
         WHERE s.user_id = ?
           AND NOT EXISTS (SELECT 1 FROM vault_members m WHERE m.vault_id = s.vault_id AND m.user_id = s.user_id)
         ORDER BY s.vault_id, s.object_id",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|s| {
            (
                s.get("vault_id"),
                s.get("object_id"),
                Role::parse(s.get("role")).unwrap_or(Role::Viewer),
            )
        })
        .collect())
}

/// A person a note is shared with, as listed for vault members.
pub struct Recipient {
    pub user_id: String,
    pub role: Role,
    pub added_at: i64,
    pub added_by: Option<String>,
    /// Holds an envelope for the given document key epoch.
    pub has_current_key: bool,
    /// ...with the owner's grant signature.
    pub key_signed: bool,
}

/// The people a note is shared with, in the order they were added.
pub async fn recipients(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
    current_epoch: i64,
) -> AppResult<Vec<Recipient>> {
    let rows = sqlx::query(
        "SELECT s.user_id, s.role, s.added_at, s.added_by,
                EXISTS(SELECT 1 FROM object_keys k WHERE k.vault_id = s.vault_id AND k.object_id = s.object_id
                       AND k.user_id = s.user_id AND k.epoch = ?) AS has_key,
                EXISTS(SELECT 1 FROM object_keys k WHERE k.vault_id = s.vault_id AND k.object_id = s.object_id
                       AND k.user_id = s.user_id AND k.epoch = ? AND k.signature IS NOT NULL) AS key_signed
         FROM object_shares s WHERE s.vault_id = ? AND s.object_id = ? ORDER BY s.added_at",
    )
    .bind(current_epoch)
    .bind(current_epoch)
    .bind(vault_id)
    .bind(object_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| Recipient {
            user_id: r.get("user_id"),
            role: Role::parse(r.get("role")).unwrap_or(Role::Viewer),
            added_at: r.get("added_at"),
            added_by: r.get("added_by"),
            has_current_key: r.get("has_key"),
            key_signed: r.get("key_signed"),
        })
        .collect())
}

/// Recipients of a note who have set up account keys (they must receive
/// its document keys).
pub async fn recipient_ids_with_account_keys(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT s.user_id FROM object_shares s JOIN account_keys k ON k.user_id = s.user_id WHERE s.vault_id = ? AND s.object_id = ?",
    )
    .bind(vault_id)
    .bind(object_id)
    .fetch_all(db)
    .await?)
}

/// `(recipient, object id)` of every share in a vault.
pub async fn in_vault(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
) -> AppResult<Vec<(String, String)>> {
    Ok(
        sqlx::query_as("SELECT user_id, object_id FROM object_shares WHERE vault_id = ?")
            .bind(vault_id)
            .fetch_all(db)
            .await?,
    )
}

/// `(vault id, object id)` of every note shared with a user.
pub async fn of_user(
    db: impl SqliteExecutor<'_>,
    user_id: &str,
) -> AppResult<Vec<(String, String)>> {
    Ok(
        sqlx::query_as("SELECT vault_id, object_id FROM object_shares WHERE user_id = ?")
            .bind(user_id)
            .fetch_all(db)
            .await?,
    )
}

/// `Ok(false)` if the note is already shared with the user.
pub async fn insert(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
    user_id: &str,
    role: &Role,
    added_by: &str,
    now: i64,
) -> AppResult<bool> {
    super::inserted(
        sqlx::query("INSERT INTO object_shares (vault_id, object_id, user_id, role, added_by, added_at) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(vault_id)
            .bind(object_id)
            .bind(user_id)
            .bind(role.as_str())
            .bind(added_by)
            .bind(now)
            .execute(db)
            .await,
    )
}

/// Change a recipient's role; rows affected.
pub async fn set_role(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
    user_id: &str,
    role: &Role,
) -> AppResult<u64> {
    Ok(sqlx::query(
        "UPDATE object_shares SET role = ? WHERE vault_id = ? AND object_id = ? AND user_id = ?",
    )
    .bind(role.as_str())
    .bind(vault_id)
    .bind(object_id)
    .bind(user_id)
    .execute(db)
    .await?
    .rows_affected())
}

/// Stop sharing a note with someone; rows affected.
pub async fn delete(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
    user_id: &str,
) -> AppResult<u64> {
    Ok(sqlx::query(
        "DELETE FROM object_shares WHERE vault_id = ? AND object_id = ? AND user_id = ?",
    )
    .bind(vault_id)
    .bind(object_id)
    .bind(user_id)
    .execute(db)
    .await?
    .rows_affected())
}

/// Drop every share of a vault's notes with one person (who became a member).
pub async fn delete_in_vault_for(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    user_id: &str,
) -> AppResult<()> {
    sqlx::query("DELETE FROM object_shares WHERE vault_id = ? AND user_id = ?")
        .bind(vault_id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(())
}

/// The key epoch from which a note's recipients get its content (set when
/// sharing started), `None` if it was never shared.
pub async fn share_from(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
) -> AppResult<Option<u32>> {
    let b: Option<Option<Vec<u8>>> =
        sqlx::query_scalar("SELECT share_from FROM objects WHERE vault_id = ? AND id = ?")
            .bind(vault_id)
            .bind(object_id)
            .fetch_optional(db)
            .await?;
    Ok(b.flatten()
        .and_then(|b| <[u8; 4]>::try_from(b.as_slice()).ok())
        .map(u32::from_be_bytes))
}

/// Sharing starts at `epoch`: recipients get content encrypted with it or a
/// later key (stored as the ciphertext header's 4 big-endian epoch bytes, so
/// queries compare `substr(payload, 2, 4)` with it).
pub async fn set_share_from(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
    epoch: u32,
) -> AppResult<()> {
    sqlx::query("UPDATE objects SET share_from = ? WHERE vault_id = ? AND id = ?")
        .bind(epoch.to_be_bytes().to_vec())
        .bind(vault_id)
        .bind(object_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Notes of a vault shared with someone right now.
pub async fn shared_in_vault(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT DISTINCT object_id FROM object_shares WHERE vault_id = ? ORDER BY object_id",
    )
    .bind(vault_id)
    .fetch_all(db)
    .await?)
}

/// Where sharing started ([`share_from`]) for each note of a vault shared
/// with someone right now.
pub async fn share_from_in_vault(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
) -> AppResult<std::collections::BTreeMap<String, u32>> {
    let rows: Vec<(String, Vec<u8>)> = sqlx::query_as(
        "SELECT id, share_from FROM objects
         WHERE vault_id = ? AND share_from IS NOT NULL AND purged_at IS NULL
           AND id IN (SELECT object_id FROM object_shares WHERE vault_id = ?)",
    )
    .bind(vault_id)
    .bind(vault_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, b)| Some((id, u32::from_be_bytes(b.as_slice().try_into().ok()?))))
        .collect())
}

/// Notes of a vault with a document key: also those deleted for good that
/// can still be undone (erasing their data drops their key).
pub async fn keyed_in_vault(db: impl SqliteExecutor<'_>, vault_id: &str) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM objects WHERE vault_id = ? AND key_epoch IS NOT NULL ORDER BY id",
    )
    .bind(vault_id)
    .fetch_all(db)
    .await?)
}

/// Whether anything of a note was stored yet (updates or snapshots).
pub async fn has_content(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: &str,
) -> AppResult<bool> {
    let n: i64 = sqlx::query_scalar(
        "SELECT (SELECT COUNT(*) FROM updates WHERE vault_id = ?1 AND object_id = ?2)
              + (SELECT COUNT(*) FROM snapshots WHERE vault_id = ?1 AND object_id = ?2)",
    )
    .bind(vault_id)
    .bind(object_id)
    .fetch_one(db)
    .await?;
    Ok(n > 0)
}
