//! Encrypted exports: everything one person can read, as ciphertext plus
//! their key envelopes, so they can decrypt it offline with
//! `coolpsonotes recover` (see `coolpsonotes_core::recover`). The server
//! assembles stored bytes only; it can't read any of it.

use coolpsonotes_core::protocol::{
    ChangesResponse, EXPORT_FORMAT, EncryptedExport, ExportedAttachment, ExportedSharedNote,
    ExportedVault, Role,
};
use sqlx::Row;

use crate::db::{self, Db};
use crate::error::AppResult;
use crate::routes::{shares, vaults};
use crate::sync;

/// The whole log of a vault (or of one object): the latest snapshot of each
/// object plus every update after it, across all pages.
async fn full_log(
    db: &Db,
    vault_id: &str,
    object: Option<&str>,
    doc_only: bool,
) -> AppResult<ChangesResponse> {
    let mut all =
        sync::changes_since(db, vault_id, object, 0, sync::MAX_PAGE, true, doc_only).await?;
    while all.has_more {
        let page = sync::changes_since(
            db,
            vault_id,
            object,
            all.next_since,
            sync::MAX_PAGE,
            false,
            doc_only,
        )
        .await?;
        all.updates.extend(page.updates);
        all.purged.extend(page.purged);
        all.head_seq = page.head_seq;
        all.has_more = page.has_more;
        all.next_since = page.next_since;
    }
    Ok(all)
}

/// Export for one user: their vaults (as a member) and the notes shared with
/// them individually.
pub async fn user_export(
    db: &Db,
    blobs: &crate::blobs::Blobs,
    user_id: &str,
) -> AppResult<EncryptedExport> {
    let recovery_envelope: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT recovery_envelope FROM account_keys WHERE user_id = ?")
            .bind(user_id)
            .fetch_optional(db)
            .await?
            .flatten();
    let memberships = sqlx::query(
        "SELECT m.vault_id, m.role, v.encrypted_meta FROM vault_members m JOIN vaults v ON v.id = m.vault_id
         WHERE m.user_id = ? ORDER BY m.vault_id",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    let mut vaults_out = Vec::with_capacity(memberships.len());
    for m in memberships {
        let vault_id: String = m.get("vault_id");
        let attachments = blobs
            .all(&vault_id)
            .await?
            .into_iter()
            .map(|(id, data)| ExportedAttachment { id, data })
            .collect();
        vaults_out.push(ExportedVault {
            role: Role::parse(m.get("role")).unwrap_or(Role::Viewer),
            encrypted_meta: m.get("encrypted_meta"),
            keys: vaults::vault_keys_for(db, &vault_id, user_id).await?,
            changes: full_log(db, &vault_id, None, false).await?,
            attachments,
            vault_id,
        });
    }
    let shares = sqlx::query(
        "SELECT s.vault_id, s.object_id, s.role FROM object_shares s
         JOIN objects o ON o.vault_id = s.vault_id AND o.id = s.object_id AND o.purged_at IS NULL
         WHERE s.user_id = ?
           AND NOT EXISTS (SELECT 1 FROM vault_members m WHERE m.vault_id = s.vault_id AND m.user_id = s.user_id)
         ORDER BY s.vault_id, s.object_id",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    let mut shared_notes = Vec::with_capacity(shares.len());
    for s in shares {
        let (vault_id, object_id): (String, String) = (s.get("vault_id"), s.get("object_id"));
        let role = Role::parse(s.get("role")).unwrap_or(Role::Viewer);
        shared_notes.push(ExportedSharedNote {
            keys: shares::object_keys_for(db, &vault_id, &object_id, user_id, role).await?,
            changes: full_log(db, &vault_id, Some(&object_id), true).await?,
            vault_id,
        });
    }
    Ok(EncryptedExport {
        format: EXPORT_FORMAT,
        user_id: user_id.to_string(),
        created_at: db::now(),
        recovery_envelope,
        vaults: vaults_out,
        shared_notes,
    })
}
