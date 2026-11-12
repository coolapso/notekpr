//! Invitations to vaults and single notes (docs/protocol.md §5.6).
//!
//! Open invitations are those not ended and not expired; a declined one
//! stays open for the owners (it looks unanswered), not for the person.
//! Ended ones (cancelled or done) are kept, without their address, only so
//! they count towards the inviter's daily limit, then deleted.

use notekpr_core::protocol::Role;
use sqlx::{Row, SqliteExecutor};

use crate::error::AppResult;

const DAY_MS: i64 = 24 * 3600 * 1000;

/// One invitation as stored.
#[derive(Clone, Debug)]
pub struct Invitation {
    pub id: String,
    pub vault_id: String,
    pub object_id: Option<String>,
    pub invited_by: String,
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub role: Role,
    pub created_at: i64,
    pub signature: Vec<u8>,
    pub sent_at: i64,
    pub expires_at: i64,
    pub state: String,
    pub accepted_by: Option<String>,
    pub accepted_at: Option<i64>,
    pub acceptance: Option<Vec<u8>>,
    pub with_comments: bool,
}

fn row(r: &sqlx::sqlite::SqliteRow) -> Invitation {
    Invitation {
        id: r.get("id"),
        vault_id: r.get("vault_id"),
        object_id: r.get("object_id"),
        invited_by: r.get("invited_by"),
        user_id: r.get("user_id"),
        email: r.get("email"),
        role: Role::from(r.get::<String, _>("role")),
        created_at: r.get("created_at"),
        signature: r.get("signature"),
        sent_at: r.get("sent_at"),
        expires_at: r.get("expires_at"),
        state: r.get("state"),
        accepted_by: r.get("accepted_by"),
        accepted_at: r.get("accepted_at"),
        acceptance: r.get("acceptance"),
        with_comments: r.get("with_comments"),
    }
}

/// Store a new invitation; `false` if its id is taken.
pub async fn insert(db: impl SqliteExecutor<'_>, i: &Invitation) -> AppResult<bool> {
    let done = sqlx::query(
        "INSERT INTO invitations (id, vault_id, object_id, invited_by, user_id, email, role,
             created_at, signature, sent_at, expires_at, with_comments)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO NOTHING",
    )
    .bind(&i.id)
    .bind(&i.vault_id)
    .bind(&i.object_id)
    .bind(&i.invited_by)
    .bind(&i.user_id)
    .bind(&i.email)
    .bind(i.role.as_str())
    .bind(i.created_at)
    .bind(&i.signature)
    .bind(i.sent_at)
    .bind(i.expires_at)
    .bind(i.with_comments)
    .execute(db)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn get(db: impl SqliteExecutor<'_>, id: &str) -> AppResult<Option<Invitation>> {
    Ok(sqlx::query(
        "SELECT id, vault_id, object_id, invited_by, user_id, email, role, created_at,
                signature, sent_at, expires_at, state, accepted_by, accepted_at, acceptance,
                with_comments
         FROM invitations WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db)
    .await?
    .map(|r| row(&r)))
}

/// Whether an open invitation to this vault (or note) for this account or
/// address exists.
pub async fn open_for(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    object_id: Option<&str>,
    user_id: Option<&str>,
    email: Option<&str>,
    now: i64,
) -> AppResult<bool> {
    let row: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM invitations
         WHERE vault_id = ?1 AND object_id IS ?2 AND state <> 'ended' AND expires_at > ?5
           AND ((?3 IS NOT NULL AND user_id = ?3) OR (?4 IS NOT NULL AND email = ?4))
         LIMIT 1",
    )
    .bind(vault_id)
    .bind(object_id)
    .bind(user_id)
    .bind(email)
    .bind(now)
    .fetch_optional(db)
    .await?;
    Ok(row.is_some())
}

/// How many of `inviter`'s invitations are open, and how many they sent in
/// the day before `now`.
pub async fn counts(db: impl SqliteExecutor<'_>, inviter: &str, now: i64) -> AppResult<(u64, u64)> {
    let r = sqlx::query(
        "SELECT COALESCE(SUM(state <> 'ended' AND expires_at > ?2), 0) AS open,
                COALESCE(SUM(sent_at > ?3), 0) AS today
         FROM invitations WHERE invited_by = ?1",
    )
    .bind(inviter)
    .bind(now)
    .bind(now - DAY_MS)
    .fetch_one(db)
    .await?;
    Ok((
        r.get::<i64, _>("open").max(0) as u64,
        r.get::<i64, _>("today").max(0) as u64,
    ))
}

/// Open invitations to `user_id` (by account, or by `email`, their
/// address), not declined, and those they accepted.
pub async fn received(
    db: impl SqliteExecutor<'_>,
    user_id: &str,
    email: Option<&str>,
    now: i64,
) -> AppResult<Vec<Invitation>> {
    Ok(sqlx::query(
        "SELECT id, vault_id, object_id, invited_by, user_id, email, role, created_at,
                signature, sent_at, expires_at, state, accepted_by, accepted_at, acceptance,
                with_comments
         FROM invitations
         WHERE expires_at > ?3 AND (
             (state = 'pending' AND (user_id = ?1 OR (user_id IS NULL AND ?2 IS NOT NULL AND email = ?2)))
             OR (state = 'accepted' AND accepted_by = ?1))
         ORDER BY sent_at, id",
    )
        .bind(user_id)
        .bind(email)
        .bind(now)
        .fetch_all(db)
        .await?
        .iter()
        .map(row)
        .collect())
}

/// Open invitations to the vaults `owner` owns and their notes.
pub async fn sent(
    db: impl SqliteExecutor<'_>,
    owner: &str,
    now: i64,
) -> AppResult<Vec<Invitation>> {
    Ok(sqlx::query(
        "SELECT id, vault_id, object_id, invited_by, user_id, email, role, created_at,
                signature, sent_at, expires_at, state, accepted_by, accepted_at, acceptance,
                with_comments
         FROM invitations
         WHERE state <> 'ended' AND expires_at > ?2
           AND vault_id IN (SELECT vault_id FROM vault_members WHERE user_id = ?1 AND role = 'owner')
         ORDER BY sent_at, id",
    )
        .bind(owner)
        .bind(now)
        .fetch_all(db)
        .await?
        .iter()
        .map(row)
        .collect())
}

/// Someone accepted (`signature`: theirs); it now waits `until` for an
/// owner's app.
pub async fn accept(
    db: impl SqliteExecutor<'_>,
    id: &str,
    user_id: &str,
    signature: &[u8],
    now: i64,
    until: i64,
) -> AppResult<u64> {
    Ok(sqlx::query(
        "UPDATE invitations SET state = 'accepted', accepted_by = ?2, accepted_at = ?3,
             acceptance = ?4, expires_at = ?5
         WHERE id = ?1 AND state = 'pending'",
    )
    .bind(id)
    .bind(user_id)
    .bind(now)
    .bind(signature)
    .bind(until)
    .execute(db)
    .await?
    .rows_affected())
}

/// Declined (or an acceptance taken back): it looks unanswered to the
/// owners and is gone for the person.
pub async fn decline(db: impl SqliteExecutor<'_>, id: &str) -> AppResult<u64> {
    Ok(sqlx::query(
        "UPDATE invitations SET state = 'declined', accepted_by = NULL, accepted_at = NULL,
             acceptance = NULL
         WHERE id = ? AND state IN ('pending', 'accepted')",
    )
    .bind(id)
    .execute(db)
    .await?
    .rows_affected())
}

/// Cancelled or done: its address is forgotten.
pub async fn end(db: impl SqliteExecutor<'_>, id: &str) -> AppResult<u64> {
    Ok(sqlx::query(
        "UPDATE invitations SET state = 'ended', email = NULL, acceptance = NULL
         WHERE id = ? AND state <> 'ended'",
    )
    .bind(id)
    .execute(db)
    .await?
    .rows_affected())
}

/// The role of `user_id`'s accepted invitation `id` to this vault (or
/// note), still waiting.
pub async fn accepted_role(
    db: impl SqliteExecutor<'_>,
    id: &str,
    vault_id: &str,
    object_id: Option<&str>,
    user_id: &str,
    now: i64,
) -> AppResult<Option<Role>> {
    let role: Option<String> = sqlx::query_scalar(
        "SELECT role FROM invitations
         WHERE id = ? AND vault_id = ? AND object_id IS ? AND accepted_by = ?
           AND state = 'accepted' AND expires_at > ?",
    )
    .bind(id)
    .bind(vault_id)
    .bind(object_id)
    .bind(user_id)
    .bind(now)
    .fetch_optional(db)
    .await?;
    Ok(role.map(Role::from))
}

/// End the open invitations `inviter` made to this vault and its notes
/// (they are no longer an owner of it); who they were for.
pub async fn end_by(
    db: impl SqliteExecutor<'_>,
    vault_id: &str,
    inviter: &str,
) -> AppResult<Vec<(Option<String>, Option<String>)>> {
    let rows = sqlx::query(
        "UPDATE invitations SET state = 'ended', email = NULL, acceptance = NULL
         WHERE vault_id = ? AND invited_by = ? AND state <> 'ended'
         RETURNING user_id, accepted_by",
    )
    .bind(vault_id)
    .bind(inviter)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| (r.get("user_id"), r.get("accepted_by")))
        .collect())
}

/// Forget the addresses of invitations that expired, and delete those no
/// daily limit counts any more.
pub async fn expire(db: &crate::db::Db, now: i64) -> AppResult<u64> {
    sqlx::query(
        "UPDATE invitations SET state = 'ended', email = NULL, acceptance = NULL
         WHERE expires_at <= ? AND state <> 'ended'",
    )
    .bind(now)
    .execute(db)
    .await?;
    Ok(
        sqlx::query("DELETE FROM invitations WHERE state = 'ended' AND sent_at <= ?")
            .bind(now - DAY_MS)
            .execute(db)
            .await?
            .rows_affected(),
    )
}

/// Active people with this address (lowercased), to tell them at once.
pub async fn people_with_email(db: impl SqliteExecutor<'_>, email: &str) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM users WHERE kind = 'human' AND disabled = 0 AND lower(email) = ?",
    )
    .bind(email)
    .fetch_all(db)
    .await?)
}
