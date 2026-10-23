//! Encrypted exports: everything one person can read, as ciphertext plus
//! their key envelopes, so they can decrypt it offline with
//! `notekpr recover` (see `notekpr_core::recover`). The server
//! assembles stored bytes only; it can't read any of it. Beside them: the
//! names of the people in it (members, owners, authors), so a recovery can
//! say who wrote each comment.

use std::collections::BTreeSet;

use notekpr_core::protocol::{
    ChangesResponse, EXPORT_FORMAT, EncryptedExport, ExportedAttachment, ExportedSharedNote,
    ExportedUser, ExportedVault, Role,
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

/// Who wrote the changes of a log (as the server knows it: who stored them).
fn authors(changes: &ChangesResponse) -> impl Iterator<Item = String> + '_ {
    changes
        .updates
        .iter()
        .map(|u| u.author_id.clone())
        .chain(changes.snapshots.iter().map(|s| s.author_id.clone()))
}

/// The owner of a vault.
async fn owner_of(db: &Db, vault_id: &str) -> AppResult<Option<String>> {
    Ok(repo::vaults::members(db, vault_id)
        .await?
        .into_iter()
        .find(|m| m.role == Role::Owner)
        .map(|m| m.user_id))
}

/// Export for one user: their vaults (as a member) and the notes shared with
/// them individually. People's names: the members of their vaults, and for
/// shared notes only the vault's owner and the authors of what they get
/// (never the rest of that vault's members).
pub async fn user_export(
    db: &Db,
    blobs: &crate::blobs::Blobs,
    user_id: &str,
) -> AppResult<EncryptedExport> {
    let recovery_envelope = repo::users::recovery_envelope(db, user_id).await?;
    let memberships = repo::vaults::memberships(db, user_id).await?;
    let mut people = BTreeSet::new();
    let mut vaults_out = Vec::with_capacity(memberships.len());
    for (vault_id, role, encrypted_meta) in memberships {
        let attachments = blobs
            .all(&vault_id)
            .await?
            .into_iter()
            .map(|(id, data)| ExportedAttachment { id, data })
            .collect();
        let members = repo::vaults::members(db, &vault_id).await?;
        let owner_id = members
            .iter()
            .find(|m| m.role == Role::Owner)
            .map(|m| m.user_id.clone());
        people.extend(members.into_iter().map(|m| m.user_id));
        let changes = full_log(db, &vault_id, None, false).await?;
        people.extend(authors(&changes));
        vaults_out.push(ExportedVault {
            role,
            encrypted_meta,
            keys: repo::keys::vault_keys(db, &vault_id, user_id).await?,
            changes,
            attachments,
            vault_id,
            owner_id,
        });
    }
    let shares = repo::shares::exportable(db, user_id).await?;
    let mut shared_notes = Vec::with_capacity(shares.len());
    for (vault_id, object_id, role) in shares {
        let changes = full_log(db, &vault_id, Some(&object_id), true).await?;
        let owner_id = owner_of(db, &vault_id).await?;
        people.extend(owner_id.clone());
        people.extend(authors(&changes));
        shared_notes.push(ExportedSharedNote {
            keys: repo::keys::document_keys(db, &vault_id, &object_id, user_id, role, false)
                .await?,
            changes,
            vault_id,
            owner_id,
        });
    }
    let mut users = Vec::with_capacity(people.len());
    for id in people {
        if let Some(u) = repo::users::info(db, &id).await? {
            users.push(ExportedUser {
                id: u.id,
                display_name: u.display_name,
            });
        }
    }
    Ok(EncryptedExport {
        format: EXPORT_FORMAT,
        user_id: user_id.to_string(),
        created_at: db::now(),
        recovery_envelope,
        vaults: vaults_out,
        shared_notes,
        users,
    })
}
