//! Principals (people and agents), their OIDC identities, public account
//! keys, private preferences, and the people directory.

use notekpr_core::protocol::{
    AdminUser, Allowance, PrincipalKind, PublicAccountKey, PublicUser, UserInfo,
};
use sqlx::{Row, SqliteExecutor};

use super::{allowance, allowance_column};
use crate::db::Db;
use crate::error::AppResult;

/// What sharing decisions need to know about a principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// `human` or `agent`.
    pub kind: String,
    /// The person an agent belongs to.
    pub owner_id: Option<String>,
    pub disabled: bool,
}

impl Principal {
    pub fn is_agent(&self) -> bool {
        self.kind == "agent"
    }
}

pub async fn principal(db: impl SqliteExecutor<'_>, id: &str) -> AppResult<Option<Principal>> {
    Ok(
        sqlx::query("SELECT kind, owner_id, disabled FROM users WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await?
            .map(|r| Principal {
                kind: r.get("kind"),
                owner_id: r.get("owner_id"),
                disabled: r.get("disabled"),
            }),
    )
}

pub async fn exists(db: impl SqliteExecutor<'_>, id: &str) -> AppResult<bool> {
    let row: Option<i64> = sqlx::query_scalar("SELECT 1 FROM users WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await?;
    Ok(row.is_some())
}

pub async fn info(db: impl SqliteExecutor<'_>, id: &str) -> AppResult<Option<UserInfo>> {
    Ok(sqlx::query(
        "SELECT id, display_name, email, kind, is_admin, created_at FROM users WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db)
    .await?
    .map(|row| UserInfo {
        id: row.get("id"),
        display_name: row.get("display_name"),
        email: row.get("email"),
        kind: PrincipalKind::parse(row.get("kind")).unwrap_or(PrincipalKind::Human),
        is_admin: row.get("is_admin"),
        created_at: row.get("created_at"),
    }))
}

/// Public profile with the account key, as shown to people one works with.
pub async fn public(db: &Db, id: &str) -> AppResult<Option<PublicUser>> {
    let Some(r) = sqlx::query("SELECT id, display_name, email, kind FROM users WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(PublicUser {
        id: r.get("id"),
        display_name: r.get("display_name"),
        email: r.get("email"),
        kind: PrincipalKind::parse(r.get("kind")).unwrap_or(PrincipalKind::Human),
        account_key: account_key(db, id).await?,
    }))
}

/// Users whose id, e-mail or display name is exactly `user` (ignoring case),
/// for the operator's command line.
pub async fn find(db: impl SqliteExecutor<'_>, user: &str) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM users WHERE id = ?1 OR lower(email) = lower(?1) OR lower(display_name) = lower(?1)",
    )
    .bind(user)
    .fetch_all(db)
    .await?)
}

// --- OIDC identities and provisioning ---

pub async fn identity_user(
    db: impl SqliteExecutor<'_>,
    issuer: &str,
    subject: &str,
) -> AppResult<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT user_id FROM identities WHERE issuer = ? AND subject = ?")
            .bind(issuer)
            .bind(subject)
            .fetch_optional(db)
            .await?,
    )
}

/// Whether an existing user is disabled (errors if the user doesn't exist).
pub async fn is_disabled(db: impl SqliteExecutor<'_>, id: &str) -> AppResult<bool> {
    Ok(
        sqlx::query_scalar("SELECT disabled FROM users WHERE id = ?")
            .bind(id)
            .fetch_one(db)
            .await?,
    )
}

/// Refresh profile data at sign-in; `make_admin` only ever grants the role.
pub async fn record_login(
    db: impl SqliteExecutor<'_>,
    id: &str,
    display_name: &str,
    email: Option<&str>,
    make_admin: bool,
    now: i64,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE users SET display_name = ?, email = COALESCE(?, email), last_login_at = ?,
         is_admin = CASE WHEN ? THEN 1 ELSE is_admin END WHERE id = ?",
    )
    .bind(display_name)
    .bind(email)
    .bind(now)
    .bind(make_admin)
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// People with an identity from `dev_issuer` (`dev = true`) or from any
/// other issuer (`dev = false`).
pub async fn count_humans(
    db: impl SqliteExecutor<'_>,
    dev_issuer: &str,
    dev: bool,
) -> AppResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM users u WHERE u.kind = 'human' AND EXISTS
           (SELECT 1 FROM identities i WHERE i.user_id = u.id AND (i.issuer = ?) = ?)",
    )
    .bind(dev_issuer)
    .bind(dev)
    .fetch_one(db)
    .await?)
}

pub async fn insert_human(
    db: impl SqliteExecutor<'_>,
    id: &str,
    display_name: &str,
    email: Option<&str>,
    is_admin: bool,
    now: i64,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO users (id, kind, display_name, email, is_admin, created_at, last_login_at)
         VALUES (?, 'human', ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(display_name)
    .bind(email)
    .bind(is_admin)
    .bind(now)
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn insert_identity(
    db: impl SqliteExecutor<'_>,
    issuer: &str,
    subject: &str,
    user_id: &str,
    now: i64,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO identities (issuer, subject, user_id, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(issuer)
    .bind(subject)
    .bind(user_id)
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

// --- Account keys ---

pub async fn account_key(
    db: impl SqliteExecutor<'_>,
    user_id: &str,
) -> AppResult<Option<PublicAccountKey>> {
    Ok(sqlx::query(
        "SELECT key_version, enc_public, sign_public, signature FROM account_keys WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await?
    .map(|r| PublicAccountKey {
        user_id: user_id.to_string(),
        key_version: r.get::<i64, _>("key_version") as u32,
        enc_public: r.get("enc_public"),
        sign_public: r.get("sign_public"),
        signature: r.get("signature"),
    }))
}

pub async fn has_account_key(db: impl SqliteExecutor<'_>, user_id: &str) -> AppResult<bool> {
    let row: Option<i64> = sqlx::query_scalar("SELECT 1 FROM account_keys WHERE user_id = ?")
        .bind(user_id)
        .fetch_optional(db)
        .await?;
    Ok(row.is_some())
}

pub async fn has_recovery(db: impl SqliteExecutor<'_>, user_id: &str) -> AppResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT recovery_envelope IS NOT NULL FROM account_keys WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await?
    .unwrap_or(false))
}

pub async fn recovery_envelope(
    db: impl SqliteExecutor<'_>,
    user_id: &str,
) -> AppResult<Option<Vec<u8>>> {
    let env: Option<Option<Vec<u8>>> =
        sqlx::query_scalar("SELECT recovery_envelope FROM account_keys WHERE user_id = ?")
            .bind(user_id)
            .fetch_optional(db)
            .await?;
    Ok(env.flatten())
}

pub async fn insert_account_key(
    db: impl SqliteExecutor<'_>,
    key: &PublicAccountKey,
    recovery_envelope: Option<&[u8]>,
    now: i64,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO account_keys (user_id, key_version, enc_public, sign_public, signature, recovery_envelope, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&key.user_id)
    .bind(key.key_version as i64)
    .bind(&key.enc_public)
    .bind(&key.sign_public)
    .bind(&key.signature)
    .bind(recovery_envelope)
    .bind(now)
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

/// Replace the recovery envelope. With a `version`, only if it is higher
/// than the stored one; without, only while none is stored (older clients).
/// Returns whether it was replaced.
pub async fn set_recovery_envelope(
    db: impl SqliteExecutor<'_>,
    user_id: &str,
    envelope: &[u8],
    version: Option<u64>,
    now: i64,
) -> AppResult<bool> {
    let r = match version {
        Some(v) => sqlx::query(
            "UPDATE account_keys SET recovery_envelope = ?, recovery_version = ?, updated_at = ?
                 WHERE user_id = ? AND recovery_version < ?",
        )
        .bind(envelope)
        .bind(v as i64)
        .bind(now)
        .bind(user_id)
        .bind(v as i64)
        .execute(db)
        .await?,
        None => {
            sqlx::query(
                "UPDATE account_keys SET recovery_envelope = ?, updated_at = ?
                 WHERE user_id = ? AND recovery_version = 0",
            )
            .bind(envelope)
            .bind(now)
            .bind(user_id)
            .execute(db)
            .await?
        }
    };
    Ok(r.rows_affected() == 1)
}

// --- Private preferences (one blob encrypted with the account key) ---

/// `(ciphertext, version)`.
pub async fn prefs(
    db: impl SqliteExecutor<'_>,
    user_id: &str,
) -> AppResult<Option<(Vec<u8>, i64)>> {
    Ok(
        sqlx::query_as("SELECT ciphertext, version FROM user_prefs WHERE user_id = ?")
            .bind(user_id)
            .fetch_optional(db)
            .await?,
    )
}

pub async fn prefs_version(db: impl SqliteExecutor<'_>, user_id: &str) -> AppResult<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT version FROM user_prefs WHERE user_id = ?")
            .bind(user_id)
            .fetch_optional(db)
            .await?,
    )
}

pub async fn put_prefs(
    db: impl SqliteExecutor<'_>,
    user_id: &str,
    ciphertext: &[u8],
    version: i64,
    now: i64,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO user_prefs (user_id, ciphertext, version, updated_at) VALUES (?, ?, ?, ?)
         ON CONFLICT(user_id) DO UPDATE SET ciphertext = excluded.ciphertext, version = excluded.version, updated_at = excluded.updated_at",
    )
    .bind(user_id)
    .bind(ciphertext)
    .bind(version)
    .bind(now)
    .execute(db)
    .await?;
    Ok(())
}

// --- Directory ---

/// Active people whose name starts with `like` (an escaped `LIKE` pattern)
/// or whose e-mail is `term`, plus the caller's own matching agents.
pub async fn search(
    db: impl SqliteExecutor<'_>,
    caller: &str,
    term: &str,
    like: &str,
) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM users WHERE id != ? AND disabled = 0 AND (
            (kind = 'human' AND (lower(display_name) LIKE ? ESCAPE '\\' OR lower(email) = ?))
            OR (kind = 'agent' AND owner_id = ? AND lower(display_name) LIKE ? ESCAPE '\\'))
         ORDER BY kind, display_name LIMIT 10",
    )
    .bind(caller)
    .bind(like)
    .bind(term)
    .bind(caller)
    .bind(like)
    .fetch_all(db)
    .await?)
}

/// Like [`search`], but only people the caller already shares a vault or
/// a note with, and whether to show their address (below).
pub async fn search_known(
    db: impl SqliteExecutor<'_>,
    caller: &str,
    term: &str,
    like: &str,
) -> AppResult<Vec<(String, bool)>> {
    // Whether each is found by their address and shown with it: people
    // sharing a vault with the caller, and those a note of the caller's
    // vaults is shared with (members see them in its shares). A note shared
    // on its own shows its recipients only whoever shared it and the
    // vault's owners (who they learn anyway, from the owner proof) — never
    // its other members —, by name, without their address.
    Ok(sqlx::query_as(
        "SELECT u.id,
                EXISTS (SELECT 1 FROM vault_members a JOIN vault_members b ON a.vault_id = b.vault_id
                        WHERE a.user_id = ?1 AND b.user_id = u.id)
                OR EXISTS (SELECT 1 FROM object_shares s JOIN vault_members m ON m.vault_id = s.vault_id
                           WHERE s.user_id = u.id AND m.user_id = ?1) AS with_address
         FROM users u WHERE u.id != ?1 AND u.disabled = 0 AND (
            (u.kind = 'human' AND (
                ((lower(u.display_name) LIKE ?2 ESCAPE '\\' OR lower(u.email) = ?3)
                 AND (EXISTS (SELECT 1 FROM vault_members a JOIN vault_members b ON a.vault_id = b.vault_id
                              WHERE a.user_id = ?1 AND b.user_id = u.id)
                      OR EXISTS (SELECT 1 FROM object_shares s JOIN vault_members m ON m.vault_id = s.vault_id
                                 WHERE s.user_id = u.id AND m.user_id = ?1)))
                OR (lower(u.display_name) LIKE ?2 ESCAPE '\\'
                    AND EXISTS (SELECT 1 FROM object_shares s
                                WHERE s.user_id = ?1
                                  AND (s.added_by = u.id
                                       OR EXISTS (SELECT 1 FROM vault_members o
                                                  WHERE o.vault_id = s.vault_id AND o.user_id = u.id
                                                    AND o.role = 'owner'))))))
            OR (u.kind = 'agent' AND u.owner_id = ?1 AND lower(u.display_name) LIKE ?2 ESCAPE '\\'))
         ORDER BY u.kind, u.display_name LIMIT 10",
    )
    .bind(caller)
    .bind(like)
    .bind(term)
    .fetch_all(db)
    .await?)
}

/// Are `a` and `b` members of a common vault?
pub async fn share_a_vault(db: impl SqliteExecutor<'_>, a: &str, b: &str) -> AppResult<bool> {
    let row: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM vault_members a JOIN vault_members b ON a.vault_id = b.vault_id
         WHERE a.user_id = ? AND b.user_id = ? LIMIT 1",
    )
    .bind(a)
    .bind(b)
    .fetch_optional(db)
    .await?;
    Ok(row.is_some())
}

/// Does `a` know `b` through a note shared on its own? A member of its
/// vault knows whom it is shared with (they see them in its shares);
/// someone it is shared with knows only who shared it, the vault's owners
/// (the owner proof shows them) and who wrote in the note or its comments —
/// never the vault's other members.
pub async fn share_a_note(db: impl SqliteExecutor<'_>, a: &str, b: &str) -> AppResult<bool> {
    let row: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM object_shares s JOIN vault_members m ON m.vault_id = s.vault_id
         WHERE s.user_id = ?2 AND m.user_id = ?1
         UNION ALL
         SELECT 1 FROM object_shares s
         WHERE s.user_id = ?1 AND (
            s.added_by = ?2
            OR EXISTS (SELECT 1 FROM vault_members o
                       WHERE o.vault_id = s.vault_id AND o.user_id = ?2 AND o.role = 'owner')
            OR EXISTS (SELECT 1 FROM updates u
                       WHERE u.vault_id = s.vault_id AND u.object_id IN (s.object_id, 'c_' || s.object_id)
                         AND u.author_id = ?2)
            OR EXISTS (SELECT 1 FROM snapshots n
                       WHERE n.vault_id = s.vault_id AND n.object_id = s.object_id AND n.author_id = ?2))
         LIMIT 1",
    )
    .bind(a)
    .bind(b)
    .fetch_optional(db)
    .await?;
    Ok(row.is_some())
}

// --- Administration ---

/// Every account, or only those with this e-mail address (any case) or
/// OIDC identity (`issuer`, `subject`).
pub async fn admin_list(
    db: impl SqliteExecutor<'_>,
    email: Option<&str>,
    identity: Option<(&str, &str)>,
) -> AppResult<Vec<AdminUser>> {
    let (issuer, subject) = identity.unzip();
    let rows = sqlx::query(
        "SELECT u.id, u.display_name, u.email, u.kind, u.owner_id, u.is_admin, u.disabled, u.created_at, u.last_login_at,
                u.vault_allowance, u.agent_allowance, u.quick_note_allowance, u.quick_notes_storage_allowance,
                u.rate_allowance, u.write_allowance, u.suspended_until,
                (SELECT COUNT(*) FROM vault_members m JOIN vaults v ON v.id = m.vault_id
                 WHERE m.user_id = u.id AND v.kind = 'regular') AS vault_count,
                (SELECT COUNT(*) FROM devices d WHERE d.user_id = u.id AND d.status = 'active') AS device_count,
                (SELECT COUNT(*) FROM vaults v WHERE v.owner_id = u.id AND v.kind = 'regular') AS owned_vault_count,
                (SELECT COUNT(*) FROM users a WHERE a.owner_id = u.id AND a.kind = 'agent') AS agent_count,
                (SELECT COUNT(*) FROM objects o JOIN vaults v ON v.id = o.vault_id
                 WHERE v.owner_id = u.id AND v.kind = 'quick_notes' AND o.purged_at IS NULL
                   AND o.id <> 'index' AND substr(o.id, 1, 2) <> 'c_') AS quick_note_count,
                (SELECT COALESCE(SUM(v.storage_bytes), 0) FROM vaults v
                 WHERE v.owner_id = u.id AND v.kind = 'regular') AS storage_bytes,
                (SELECT COALESCE(SUM(v.storage_bytes), 0) FROM vaults v
                 WHERE v.owner_id = u.id AND v.kind = 'quick_notes') AS quick_notes_storage_bytes
         FROM users u
         WHERE (?1 IS NULL OR lower(u.email) = lower(?1))
           AND (?2 IS NULL OR EXISTS (SELECT 1 FROM identities i
                                      WHERE i.user_id = u.id AND i.issuer = ?2 AND i.subject = ?3))
         ORDER BY u.kind, u.created_at",
    )
    .bind(email)
    .bind(issuer)
    .bind(subject)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| AdminUser {
            id: r.get("id"),
            display_name: r.get("display_name"),
            email: r.get("email"),
            kind: PrincipalKind::parse(r.get("kind")).unwrap_or(PrincipalKind::Human),
            owner_id: r.get("owner_id"),
            is_admin: r.get("is_admin"),
            disabled: r.get("disabled"),
            created_at: r.get("created_at"),
            last_login_at: r.get("last_login_at"),
            vault_count: r.get("vault_count"),
            device_count: r.get("device_count"),
            owned_vault_count: r.get("owned_vault_count"),
            agent_count: r.get("agent_count"),
            quick_note_count: r.get("quick_note_count"),
            storage_bytes: r.get::<i64, _>("storage_bytes").max(0) as u64,
            quick_notes_storage_bytes: r.get::<i64, _>("quick_notes_storage_bytes").max(0) as u64,
            vault_allowance: allowance(r.get("vault_allowance")),
            agent_allowance: allowance(r.get("agent_allowance")),
            quick_note_allowance: allowance(r.get("quick_note_allowance")),
            quick_notes_storage_allowance: allowance(r.get("quick_notes_storage_allowance")),
            rate_allowance: allowance(r.get("rate_allowance")),
            write_allowance: allowance(r.get("write_allowance")),
            suspended_until: r.get("suspended_until"),
            // Counted in memory (`person_limits.rs`), filled in by the route.
            refused: Default::default(),
        })
        .collect())
}

/// A person's own limits, set by an administrator.
#[derive(Clone, Copy, Debug, Default)]
pub struct Allowances {
    pub vaults: Allowance,
    pub agents: Allowance,
    pub quick_notes: Allowance,
}

pub async fn allowances(db: impl SqliteExecutor<'_>, id: &str) -> AppResult<Allowances> {
    let row: Option<(Option<i64>, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT vault_allowance, agent_allowance, quick_note_allowance FROM users WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    let (v, a, q) = row.unwrap_or_default();
    Ok(Allowances {
        vaults: allowance(v),
        agents: allowance(a),
        quick_notes: allowance(q),
    })
}

/// Only people have allowances (agents belong to one).
pub async fn set_vault_allowance(
    db: impl SqliteExecutor<'_>,
    id: &str,
    a: Allowance,
) -> AppResult<()> {
    sqlx::query("UPDATE users SET vault_allowance = ? WHERE id = ? AND kind = 'human'")
        .bind(allowance_column(a))
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn set_agent_allowance(
    db: impl SqliteExecutor<'_>,
    id: &str,
    a: Allowance,
) -> AppResult<()> {
    sqlx::query("UPDATE users SET agent_allowance = ? WHERE id = ? AND kind = 'human'")
        .bind(allowance_column(a))
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// A person's own request rate (requests per second).
pub async fn set_rate_allowance(
    db: impl SqliteExecutor<'_>,
    id: &str,
    a: Allowance,
) -> AppResult<()> {
    sqlx::query("UPDATE users SET rate_allowance = ? WHERE id = ? AND kind = 'human'")
        .bind(allowance_column(a))
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// A person's own daily write allowance (MiB).
pub async fn set_write_allowance(
    db: impl SqliteExecutor<'_>,
    id: &str,
    a: Allowance,
) -> AppResult<()> {
    sqlx::query("UPDATE users SET write_allowance = ? WHERE id = ? AND kind = 'human'")
        .bind(allowance_column(a))
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Suspend a person until then (ms), or lift it (`None`): whether there is
/// such a person (agents have no suspension of their own).
pub async fn set_suspended(
    db: impl SqliteExecutor<'_>,
    id: &str,
    until: Option<i64>,
) -> AppResult<bool> {
    let r = sqlx::query("UPDATE users SET suspended_until = ? WHERE id = ? AND kind = 'human'")
        .bind(until)
        .bind(id)
        .execute(db)
        .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn set_quick_note_allowance(
    db: impl SqliteExecutor<'_>,
    id: &str,
    a: Allowance,
) -> AppResult<()> {
    sqlx::query("UPDATE users SET quick_note_allowance = ? WHERE id = ? AND kind = 'human'")
        .bind(allowance_column(a))
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// How much their quick notes may take (MiB).
pub async fn set_quick_notes_storage_allowance(
    db: impl SqliteExecutor<'_>,
    id: &str,
    a: Allowance,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE users SET quick_notes_storage_allowance = ? WHERE id = ? AND kind = 'human'",
    )
    .bind(allowance_column(a))
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// Vaults this person owns.
pub async fn owned_vaults(db: impl SqliteExecutor<'_>, id: &str) -> AppResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM vaults WHERE owner_id = ? AND kind = 'regular'")
            .bind(id)
            .fetch_one(db)
            .await?,
    )
}

/// `(kind, disabled)` of an account.
pub async fn kind_and_disabled(
    db: impl SqliteExecutor<'_>,
    id: &str,
) -> AppResult<Option<(String, bool)>> {
    Ok(
        sqlx::query_as("SELECT kind, disabled FROM users WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await?,
    )
}

pub async fn set_disabled(db: impl SqliteExecutor<'_>, id: &str, disabled: bool) -> AppResult<()> {
    sqlx::query("UPDATE users SET disabled = ? WHERE id = ?")
        .bind(disabled)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Delete a person's account with everything it has: the foreign keys
/// cascade to the vaults they own (with all in them), their agents,
/// memberships, shares, devices, sessions, keys and sign-in identities.
pub async fn delete_person(db: impl SqliteExecutor<'_>, id: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM users WHERE id = ? AND kind = 'human'")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Only people can be administrators.
pub async fn set_admin(db: impl SqliteExecutor<'_>, id: &str, is_admin: bool) -> AppResult<()> {
    sqlx::query("UPDATE users SET is_admin = ? WHERE id = ? AND kind = 'human'")
        .bind(is_admin)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}
