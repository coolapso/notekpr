//! Encrypted exports: everything one person can read, as ciphertext plus
//! their key envelopes, so they can decrypt it offline with
//! `coolpsonotes recover` (see `coolpsonotes_core::recover`). The server
//! assembles stored bytes only; it can't read any of it.

use coolpsonotes_core::protocol::{
    ChangesResponse, EXPORT_FORMAT, EncryptedExport, ExportedAttachment, ExportedSharedNote,
    ExportedVault,
};

use crate::db::{self, Db};
use crate::error::AppResult;
use crate::repo;
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
    let recovery_envelope = repo::users::recovery_envelope(db, user_id).await?;
    let memberships = repo::vaults::memberships(db, user_id).await?;
    let mut vaults_out = Vec::with_capacity(memberships.len());
    for (vault_id, role, encrypted_meta) in memberships {
        let attachments = blobs
            .all(&vault_id)
            .await?
            .into_iter()
            .map(|(id, data)| ExportedAttachment { id, data })
            .collect();
        vaults_out.push(ExportedVault {
            role,
            encrypted_meta,
            keys: repo::keys::vault_keys(db, &vault_id, user_id).await?,
            changes: full_log(db, &vault_id, None, false).await?,
            attachments,
            vault_id,
        });
    }
    let shares = repo::shares::exportable(db, user_id).await?;
    let mut shared_notes = Vec::with_capacity(shares.len());
    for (vault_id, object_id, role) in shares {
        shared_notes.push(ExportedSharedNote {
            keys: repo::keys::document_keys(db, &vault_id, &object_id, user_id, role, false)
                .await?,
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
