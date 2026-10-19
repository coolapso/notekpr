//! A person's devices: public keys, approval status and the account secret
//! encrypted to each active device.

use coolpsonotes_core::protocol::{DeviceInfo, DeviceStatus};
use sqlx::{Row, SqliteExecutor};

use crate::error::AppResult;

/// A user's devices that are not revoked; `current` marks the caller's.
pub async fn list(
    db: impl SqliteExecutor<'_>,
    user_id: &str,
    current: Option<&str>,
) -> AppResult<Vec<DeviceInfo>> {
    let rows = sqlx::query(
        "SELECT id, name, status, enc_public, created_at, approved_at, last_seen_at FROM devices
         WHERE user_id = ? AND status != 'revoked' ORDER BY created_at",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let id: String = r.get("id");
            DeviceInfo {
                current: current == Some(id.as_str()),
                id,
                name: r.get("name"),
                status: DeviceStatus::parse(r.get("status")).unwrap_or(DeviceStatus::Pending),
                enc_public: r.get("enc_public"),
                created_at: r.get("created_at"),
                approved_at: r.get("approved_at"),
                last_seen_at: r.get("last_seen_at"),
            }
        })
        .collect())
}

pub async fn count_pending(db: impl SqliteExecutor<'_>, user_id: &str) -> AppResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM devices WHERE user_id = ? AND status = 'pending'")
            .bind(user_id)
            .fetch_one(db)
            .await?,
    )
}

/// Register a device: active (approved at `now`) if `envelope` is given,
/// pending otherwise. `Ok(false)` if the device id is taken.
pub async fn insert(
    db: impl SqliteExecutor<'_>,
    id: &str,
    user_id: &str,
    name: &str,
    enc_public: &[u8],
    envelope: Option<&[u8]>,
    now: i64,
) -> AppResult<bool> {
    let status = if envelope.is_some() {
        "active"
    } else {
        "pending"
    };
    super::inserted(
        sqlx::query(
            "INSERT INTO devices (id, user_id, name, enc_public, status, account_envelope, created_at, approved_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(user_id)
        .bind(name)
        .bind(enc_public)
        .bind(status)
        .bind(envelope)
        .bind(now)
        .bind(envelope.map(|_| now))
        .execute(db)
        .await,
    )
}

/// The account a device belongs to, unless it was revoked.
pub async fn owner_unless_revoked(
    db: impl SqliteExecutor<'_>,
    device_id: &str,
) -> AppResult<Option<String>> {
    Ok(
        sqlx::query_scalar("SELECT user_id FROM devices WHERE id = ? AND status != 'revoked'")
            .bind(device_id)
            .fetch_optional(db)
            .await?,
    )
}

/// `(status, account envelope)` of one of the user's devices.
pub async fn envelope(
    db: impl SqliteExecutor<'_>,
    id: &str,
    user_id: &str,
) -> AppResult<Option<(String, Option<Vec<u8>>)>> {
    Ok(
        sqlx::query_as("SELECT status, account_envelope FROM devices WHERE id = ? AND user_id = ?")
            .bind(id)
            .bind(user_id)
            .fetch_optional(db)
            .await?,
    )
}

/// `(status, public key)` of one of the user's devices.
pub async fn public_key(
    db: impl SqliteExecutor<'_>,
    id: &str,
    user_id: &str,
) -> AppResult<Option<(String, Vec<u8>)>> {
    Ok(
        sqlx::query_as("SELECT status, enc_public FROM devices WHERE id = ? AND user_id = ?")
            .bind(id)
            .bind(user_id)
            .fetch_optional(db)
            .await?,
    )
}

pub async fn activate(
    db: impl SqliteExecutor<'_>,
    id: &str,
    envelope: &[u8],
    now: i64,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE devices SET status = 'active', account_envelope = ?, approved_at = ? WHERE id = ?",
    )
    .bind(envelope)
    .bind(now)
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

/// Rename one of the user's devices that isn't revoked; rows affected.
pub async fn rename(
    db: impl SqliteExecutor<'_>,
    id: &str,
    user_id: &str,
    name: &str,
) -> AppResult<u64> {
    Ok(sqlx::query(
        "UPDATE devices SET name = ? WHERE id = ? AND user_id = ? AND status != 'revoked'",
    )
    .bind(name)
    .bind(id)
    .bind(user_id)
    .execute(db)
    .await?
    .rows_affected())
}

/// Revoke one of the user's devices and drop its envelope; rows affected.
pub async fn revoke(
    db: impl SqliteExecutor<'_>,
    id: &str,
    user_id: &str,
    now: i64,
) -> AppResult<u64> {
    Ok(sqlx::query(
        "UPDATE devices SET status = 'revoked', account_envelope = NULL, revoked_at = ?
         WHERE id = ? AND user_id = ? AND status != 'revoked'",
    )
    .bind(now)
    .bind(id)
    .bind(user_id)
    .execute(db)
    .await?
    .rows_affected())
}

pub async fn touch(db: impl SqliteExecutor<'_>, id: &str, now: i64) -> AppResult<()> {
    sqlx::query("UPDATE devices SET last_seen_at = ? WHERE id = ?")
        .bind(now)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}
