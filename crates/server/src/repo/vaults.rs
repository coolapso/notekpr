//! Vaults, their members and roles.

use notekpr_core::protocol::{Limit, Role, VaultInfo, VaultKind};
use sqlx::{Row, SqliteExecutor};

use crate::error::AppResult;

fn vault_row(r: &sqlx::sqlite::SqliteRow) -> VaultInfo {
    let role = Role::parse(r.get("role")).unwrap_or(Role::Viewer);
    // Only the owner can rotate keys.
    let rotation_pending = role == Role::Owner && r.get::<bool, _>("rotation_pending");
    VaultInfo {
        id: r.get("id"),
        owner_id: r.get("owner_id"),
        role,
        encrypted_meta: r.get("encrypted_meta"),
        key_epoch: r.get::<i64, _>("key_epoch") as u32,
        head_seq: r.get("head_seq"),
        archived: r.get("archived"),
        member_count: r.get::<i64, _>("member_count") as u32,
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
        viewers_comment: r.get("viewers_comment"),
        kind: VaultKind::parse(r.get("kind")).unwrap_or_default(),
        // Filled in by the routes (it depends on the instance's settings).
        note_limit: Limit::Unlimited,
        rotation_pending,
        // Filled in by the routes, for the owner.
        notes_rotation_pending: vec![],
    }
}

/// The vaults a user is a member of, with their role, oldest first.
pub async fn list_for_member(
    db: impl SqliteExecutor<'_>,
    user_id: &str,
) -> AppResult<Vec<VaultInfo>> {
    let rows = sqlx::query(
        "SELECT v.id, v.owner_id, m.role, v.encrypted_meta, v.key_epoch, v.head_seq, v.archived,
                v.created_at, v.updated_at, v.viewers_comment, v.kind, v.rotation_pending,
                (SELECT COUNT(*) FROM vault_members m2 WHERE m2.vault_id = v.id) AS member_count
         FROM vaults v JOIN vault_members m ON m.vault_id = v.id AND m.user_id = ?
         ORDER BY v.created_at",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(vault_row).collect())
}

/// One vault as seen by a member (`None` for non-members).
pub async fn get_for_member(
    db: impl SqliteExecutor<'_>,
    user_id: &str,
    vault_id: &str,
) -> AppResult<Option<VaultInfo>> {
    Ok(sqlx::query(
        "SELECT v.id, v.owner_id, m.role, v.encrypted_meta, v.key_epoch, v.head_seq, v.archived,
                v.created_at, v.updated_at, v.viewers_comment, v.kind, v.rotation_pending,
                (SELECT COUNT(*) FROM vault_members m2 WHERE m2.vault_id = v.id) AS member_count
         FROM vaults v JOIN vault_members m ON m.vault_id = v.id AND m.user_id = ?
         WHERE v.id = ?",
    )
    .bind(user_id)
    .bind(vault_id)
    .fetch_optional(db)
    .await?
    .as_ref()
    .map(vault_row))
}

/// New vault at key epoch 1; `Ok(false)` if the id is taken (or, for
/// quick notes, the owner has them already).
pub async fn insert(
    db: impl SqliteExecutor<'_>,
    id: &str,
    owner_id: &str,
    encrypted_meta: &[u8],
    kind: VaultKind,
    now: i64,
) -> AppResult<bool> {
    super::inserted(
        sqlx::query(
            "INSERT INTO vaults (id, owner_id, encrypted_meta, key_epoch, head_seq, kind, created_at, updated_at)
             VALUES (?, ?, ?, 1, 0, ?, ?, ?)",
        )
        .bind(id)
        .bind(owner_id)
        .bind(encrypted_meta)
        .bind(kind.as_str())
        .bind(now)
        .bind(now)
        .execute(db)
        .await,
    )
}

/// A vault's kind and owner (`None`: no such vault).
pub async fn kind_and_owner(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
) -> AppResult<Option<(VaultKind, String)>> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT kind, owner_id FROM vaults WHERE id = ?")
            .bind(vault_id)
            .fetch_optional(db)
            .await?;
    Ok(row.map(|(k, o)| (VaultKind::parse(&k).unwrap_or_default(), o)))
}

/// Whether `vault_id` is someone's quick notes.
pub async fn is_quick_notes(db: impl SqliteExecutor<'_>, vault_id: &str) -> AppResult<bool> {
    Ok(kind_and_owner(db, vault_id)
        .await?
        .is_some_and(|(k, _)| k == VaultKind::QuickNotes))
}

/// The id of `owner_id`'s quick-notes vault, if they have one.
pub async fn quick_notes_of(
    db: impl SqliteExecutor<'_>,
    owner_id: &str,
) -> AppResult<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT id FROM vaults WHERE owner_id = ? AND kind = 'quick_notes'")
            .bind(owner_id)
            .fetch_optional(db)
            .await?,
    )
}

/// People who have quick notes.
pub async fn quick_notes_owners(db: impl SqliteExecutor<'_>) -> AppResult<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT owner_id FROM vaults WHERE kind = 'quick_notes'")
            .fetch_all(db)
            .await?,
    )
}

/// Notes in a vault that aren't deleted for good: its objects apart from
/// the index and the notes' comments (`c_…`; note ids never contain `_`).
pub async fn note_count(db: impl SqliteExecutor<'_>, vault_id: &str) -> AppResult<u64> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM objects
         WHERE vault_id = ? AND purged_at IS NULL AND id <> 'index' AND substr(id, 1, 2) <> 'c_'",
    )
    .bind(vault_id)
    .fetch_one(db)
    .await?;
    Ok(n as u64)
}

pub async fn set_meta(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    encrypted_meta: &[u8],
    now: i64,
) -> AppResult<()> {
    sqlx::query("UPDATE vaults SET encrypted_meta = ?, updated_at = ? WHERE id = ?")
        .bind(encrypted_meta)
        .bind(now)
        .bind(vault_id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn set_archived(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    archived: bool,
    now: i64,
) -> AppResult<()> {
    sqlx::query("UPDATE vaults SET archived = ?, updated_at = ? WHERE id = ?")
        .bind(archived)
        .bind(now)
        .bind(vault_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Whether viewers may comment on the vault's notes.
pub async fn viewers_comment(db: impl SqliteExecutor<'_>, vault_id: &str) -> AppResult<bool> {
    Ok(
        sqlx::query_scalar("SELECT viewers_comment FROM vaults WHERE id = ?")
            .bind(vault_id)
            .fetch_optional(db)
            .await?
            .unwrap_or(false),
    )
}

pub async fn set_viewers_comment(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    allowed: bool,
    now: i64,
) -> AppResult<()> {
    sqlx::query("UPDATE vaults SET viewers_comment = ?, updated_at = ? WHERE id = ?")
        .bind(allowed)
        .bind(now)
        .bind(vault_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Move to a new key epoch, with the vault name re-encrypted under it. No
/// one who lost access knows the new key.
pub async fn set_key_epoch(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    epoch: i64,
    encrypted_meta: &[u8],
    now: i64,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE vaults SET key_epoch = ?, encrypted_meta = ?, rotation_pending = 0, updated_at = ?
         WHERE id = ?",
    )
    .bind(epoch)
    .bind(encrypted_meta)
    .bind(now)
    .bind(vault_id)
    .execute(db)
    .await?;
    Ok(())
}

/// Someone who lost access still knows the vault's current key: its owner's
/// apps rotate it (which clears this).
pub async fn set_rotation_pending(db: impl SqliteExecutor<'_>, vault_id: &str) -> AppResult<()> {
    sqlx::query("UPDATE vaults SET rotation_pending = 1 WHERE id = ?")
        .bind(vault_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Every vault whose key someone going holds (an agent deleted): its
/// owner's apps rotate it.
pub async fn set_rotation_pending_for_holder(
    db: impl SqliteExecutor<'_>,
    user_id: &str,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE vaults SET rotation_pending = 1
         WHERE id IN (SELECT vault_id FROM vault_keys WHERE user_id = ?)",
    )
    .bind(user_id)
    .execute(db)
    .await?;
    Ok(())
}

/// Delete a vault with everything in it (foreign keys cascade).
pub async fn delete(db: impl SqliteExecutor<'_>, vault_id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM vaults WHERE id = ?")
        .bind(vault_id)
        .execute(db)
        .await?;
    Ok(())
}

/// The vault's current key epoch (`None` if the vault doesn't exist).
pub async fn key_epoch(db: impl SqliteExecutor<'_>, vault_id: &str) -> AppResult<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT key_epoch FROM vaults WHERE id = ?")
            .bind(vault_id)
            .fetch_optional(db)
            .await?,
    )
}

/// The log sequence number after which content must be signed (`None`:
/// signatures not required yet).
pub async fn signed_from(db: impl SqliteExecutor<'_>, vault_id: &str) -> AppResult<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT signed_from FROM vaults WHERE id = ?")
            .bind(vault_id)
            .fetch_optional(db)
            .await?
            .flatten(),
    )
}

/// `(head_seq, signed_from)` of a vault.
pub async fn head_and_signed_from(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
) -> AppResult<(i64, Option<i64>)> {
    Ok(
        sqlx::query_as("SELECT head_seq, signed_from FROM vaults WHERE id = ?")
            .bind(vault_id)
            .fetch_one(db)
            .await?,
    )
}

pub async fn set_signed_from(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    from_seq: i64,
) -> AppResult<()> {
    sqlx::query("UPDATE vaults SET signed_from = ? WHERE id = ?")
        .bind(from_seq)
        .bind(vault_id)
        .execute(db)
        .await?;
    Ok(())
}

// --- Members ---

/// A user's role in a vault (`None` if not a member, or no such vault).
pub async fn member_role(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    user_id: &str,
) -> AppResult<Option<Role>> {
    let role: Option<String> =
        sqlx::query_scalar("SELECT role FROM vault_members WHERE vault_id = ? AND user_id = ?")
            .bind(vault_id)
            .bind(user_id)
            .fetch_optional(db)
            .await?;
    Ok(role.and_then(|r| Role::parse(&r)))
}

/// `Ok(false)` if the user is already a member.
pub async fn insert_member(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    user_id: &str,
    role: &Role,
    added_by: &str,
    now: i64,
) -> AppResult<bool> {
    super::inserted(
        sqlx::query("INSERT INTO vault_members (vault_id, user_id, role, added_by, added_at) VALUES (?, ?, ?, ?, ?)")
            .bind(vault_id)
            .bind(user_id)
            .bind(role.as_str())
            .bind(added_by)
            .bind(now)
            .execute(db)
            .await,
    )
}

/// Change a member's role; rows affected.
pub async fn set_member_role(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    user_id: &str,
    role: &Role,
) -> AppResult<u64> {
    Ok(
        sqlx::query("UPDATE vault_members SET role = ? WHERE vault_id = ? AND user_id = ?")
            .bind(role.as_str())
            .bind(vault_id)
            .bind(user_id)
            .execute(db)
            .await?
            .rows_affected(),
    )
}

/// Remove a member other than the owner; rows affected.
pub async fn delete_member(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    user_id: &str,
) -> AppResult<u64> {
    Ok(sqlx::query(
        "DELETE FROM vault_members WHERE vault_id = ? AND user_id = ? AND role != 'owner'",
    )
    .bind(vault_id)
    .bind(user_id)
    .execute(db)
    .await?
    .rows_affected())
}

pub async fn member_ids(db: impl SqliteExecutor<'_>, vault_id: &str) -> AppResult<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT user_id FROM vault_members WHERE vault_id = ?")
            .bind(vault_id)
            .fetch_all(db)
            .await?,
    )
}

/// Members who have set up account keys (they must receive vault keys).
pub async fn member_ids_with_account_keys(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT m.user_id FROM vault_members m JOIN account_keys k ON k.user_id = m.user_id WHERE m.vault_id = ?",
    )
    .bind(vault_id)
    .fetch_all(db)
    .await?)
}

pub struct Member {
    pub user_id: String,
    pub role: Role,
    pub added_at: i64,
    pub added_by: Option<String>,
    /// Holds an envelope for the vault's current key epoch.
    pub has_current_key: bool,
}

/// Members in the order they were added.
pub async fn members(db: impl SqliteExecutor<'_>, vault_id: &str) -> AppResult<Vec<Member>> {
    let rows = sqlx::query(
        "SELECT m.user_id, m.role, m.added_at, m.added_by,
                EXISTS(SELECT 1 FROM vault_keys k WHERE k.vault_id = m.vault_id AND k.user_id = m.user_id
                       AND k.epoch = v.key_epoch) AS has_key
         FROM vault_members m JOIN vaults v ON v.id = m.vault_id
         WHERE m.vault_id = ? ORDER BY m.added_at",
    )
    .bind(vault_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| Member {
            user_id: r.get("user_id"),
            role: Role::parse(r.get("role")).unwrap_or(Role::Viewer),
            added_at: r.get("added_at"),
            added_by: r.get("added_by"),
            has_current_key: r.get("has_key"),
        })
        .collect())
}

/// Vault ids a user is a member of.
pub async fn ids_of_member(db: impl SqliteExecutor<'_>, user_id: &str) -> AppResult<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT vault_id FROM vault_members WHERE user_id = ?")
            .bind(user_id)
            .fetch_all(db)
            .await?,
    )
}

/// `(vault id, role, encrypted vault meta)` of a user's memberships, by id.
pub async fn memberships(
    db: impl SqliteExecutor<'_>,
    user_id: &str,
) -> AppResult<Vec<(String, Role, Vec<u8>)>> {
    let rows = sqlx::query(
        "SELECT m.vault_id, m.role, v.encrypted_meta FROM vault_members m JOIN vaults v ON v.id = m.vault_id
         WHERE m.user_id = ? ORDER BY m.vault_id",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|m| {
            (
                m.get("vault_id"),
                Role::parse(m.get("role")).unwrap_or(Role::Viewer),
                m.get("encrypted_meta"),
            )
        })
        .collect())
}
