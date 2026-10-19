//! Turning an [`EncryptedExport`] back into plain files, without a server:
//! the escape hatch behind `coolpsonotes recover`. Given the person's account
//! secret (from their recovery key or a device), every vault they could read
//! becomes a folder of Markdown files and attachments again.
//!
//! Paths come from decrypted vault indexes, which collaborators control:
//! they are normalized here, and the code that writes them to disk must still
//! check that every component stays inside the output directory.

use std::collections::BTreeSet;

use crate::doc::EntryKind;
use crate::engine::{self, ObjectMeta, VaultMeta, VaultReplica};
use crate::keys::{AccountSecret, RecoveryKey};
use crate::paths;
use crate::protocol::{EncryptedExport, ExportedSharedNote, ExportedVault, Role};

/// Where trashed notes and files go inside a recovered vault.
pub const TRASH_DIR: &str = "_trash";
/// Folder for notes shared with the person individually.
pub const SHARED_DIR: &str = "Shared with me";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredFile {
    /// Normalized, vault-relative path (no `..`, no leading `/`).
    pub path: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct RecoveredVault {
    pub vault_id: String,
    pub name: String,
    pub files: Vec<RecoveredFile>,
    /// Explicit (possibly empty) folders.
    pub folders: Vec<String>,
    /// Things that could not be recovered, in words.
    pub problems: Vec<String>,
}

/// Unlock the account in an export with the recovery key.
pub fn unlock_with_recovery_key(
    export: &EncryptedExport,
    recovery_key: &str,
) -> crate::engine::Result<AccountSecret> {
    let rk = RecoveryKey::parse(recovery_key)?;
    let envelope = export.recovery_envelope.as_ref().ok_or_else(|| {
        engine::EngineError::Invalid("this export has no recovery envelope".into())
    })?;
    Ok(rk.unwrap_account(envelope, &export.user_id)?)
}

/// Decrypt everything in the export. Vaults that can't be opened at all are
/// returned with a problem and no files, so one bad vault never hides the rest.
pub fn decrypt_export(account: &AccountSecret, export: &EncryptedExport) -> Vec<RecoveredVault> {
    let mut out: Vec<RecoveredVault> = export
        .vaults
        .iter()
        .map(|v| decrypt_vault(account, &export.user_id, v))
        .collect();
    if !export.shared_notes.is_empty() {
        let mut shared = RecoveredVault {
            vault_id: String::new(),
            name: SHARED_DIR.to_string(),
            ..Default::default()
        };
        let mut taken = BTreeSet::new();
        for n in &export.shared_notes {
            match decrypt_shared_note(account, &export.user_id, n) {
                Ok((title, text)) => {
                    let safe = safe_name(&title);
                    let path = paths::unique(&format!("{safe}.md"), |p| taken.contains(p));
                    taken.insert(path.clone());
                    shared.files.push(RecoveredFile {
                        path,
                        data: text.into_bytes(),
                    });
                }
                Err(e) => shared
                    .problems
                    .push(format!("shared note {}: {e}", n.keys.object_id)),
            }
        }
        out.push(shared);
    }
    out
}

fn decrypt_vault(account: &AccountSecret, user_id: &str, v: &ExportedVault) -> RecoveredVault {
    let mut rv = RecoveredVault {
        vault_id: v.vault_id.clone(),
        name: v.vault_id.clone(),
        ..Default::default()
    };
    let ring = match engine::keyring_from_response(account, user_id, &v.vault_id, &v.keys) {
        Ok(r) => r,
        Err(e) => {
            rv.problems.push(format!("no usable vault key: {e}"));
            return rv;
        }
    };
    match VaultMeta::decrypt(&ring, &v.encrypted_meta) {
        Ok(m) => rv.name = m.name,
        Err(e) => rv.problems.push(format!("vault name unreadable: {e}")),
    }
    let mut replica = VaultReplica::new(&v.vault_id, v.role, ring.clone(), VaultMeta::default());
    replica.doc_keys = engine::object_keyrings(&ring, &v.keys);
    let (_, errors) = replica.apply_changes(&v.changes);
    if !errors.is_empty() {
        rv.problems.push(format!(
            "{} encrypted changes could not be read (missing or damaged keys)",
            errors.len()
        ));
    }
    let mut taken = BTreeSet::new();
    let place = |path: &str, trashed: bool, taken: &mut BTreeSet<String>| -> Option<String> {
        let p = paths::normalize(path).ok()?;
        let p = if trashed {
            paths::join(TRASH_DIR, &p)
        } else {
            p
        };
        let p = paths::unique(&p, |c| taken.contains(&c.to_lowercase()));
        taken.insert(p.to_lowercase());
        Some(p)
    };
    for e in replica.index.entries() {
        let trashed = !e.is_live();
        match e.kind {
            EntryKind::Folder => {
                if let Some(p) = place(&e.path, trashed, &mut taken) {
                    rv.folders.push(p);
                }
            }
            EntryKind::Note => {
                let Some(p) = place(&e.path, trashed, &mut taken) else {
                    rv.problems.push(format!(
                        "skipped a note with an unusable path: {:?}",
                        e.path
                    ));
                    continue;
                };
                rv.files.push(RecoveredFile {
                    path: p,
                    data: replica.note_text(&e.id).into_bytes(),
                });
            }
            EntryKind::Attachment => {
                let Some(blob) = v.attachments.iter().find(|a| a.id == e.id) else {
                    rv.problems
                        .push(format!("attachment {} is not in the export", e.path));
                    continue;
                };
                let data = match replica.decrypt_attachment(&e.id, &blob.data) {
                    Ok(d) => d,
                    Err(err) => {
                        rv.problems.push(format!("attachment {}: {err}", e.path));
                        continue;
                    }
                };
                let Some(p) = place(&e.path, trashed, &mut taken) else {
                    rv.problems.push(format!(
                        "skipped a file with an unusable path: {:?}",
                        e.path
                    ));
                    continue;
                };
                rv.files.push(RecoveredFile { path: p, data });
            }
        }
    }
    rv.files.sort_by(|a, b| a.path.cmp(&b.path));
    rv
}

fn decrypt_shared_note(
    account: &AccountSecret,
    user_id: &str,
    n: &ExportedSharedNote,
) -> crate::engine::Result<(String, String)> {
    let ring = engine::object_keyring_for_recipient(account, user_id, &n.vault_id, &n.keys)?;
    let oid = &n.keys.object_id;
    let title = n
        .keys
        .encrypted_meta
        .as_ref()
        .and_then(|m| ObjectMeta::decrypt(&ring, oid, m).ok())
        .map(|m| m.title)
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| format!("Shared note {}", &oid[..oid.len().min(8)]));
    let mut replica = VaultReplica::for_shared_notes(&n.vault_id);
    replica.role = Role::Viewer;
    replica.doc_keys.insert(oid.clone(), ring);
    replica.apply_changes(&n.changes);
    Ok((title, replica.note_text(oid)))
}

/// A title usable as one file name: no separators or characters that are
/// invalid on common filesystems.
fn safe_name(title: &str) -> String {
    let s: String = title
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .collect();
    let s = s
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .to_string();
    if s.is_empty() {
        "Untitled".into()
    } else {
        s.chars().take(120).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::Purpose;
    use crate::doc::IndexDoc;
    use crate::engine::{new_vault, start_sharing};
    use crate::protocol::*;

    fn up(seq: i64, object_id: &str, payload: Vec<u8>) -> Update {
        Update {
            object_id: object_id.into(),
            seq,
            author_id: "u".into(),
            payload,
            created_at: 0,
        }
    }

    #[test]
    fn recovers_notes_attachments_trash_and_shared_notes() {
        let alice = AccountSecret::generate();
        let (create, ring) = new_vault(&alice, "alice", &VaultMeta::named("Family")).unwrap();
        let vid = ring.vault_id().to_string();
        let mut r = VaultReplica::new(&vid, Role::Owner, ring.clone(), VaultMeta::named("Family"));
        let index = IndexDoc::new();
        let mut updates = Vec::new();
        let mut seq = 0;
        let mut push = |r: &mut VaultReplica, o: &str, u: Vec<u8>, updates: &mut Vec<Update>| {
            seq += 1;
            let p = r.local_change(o, u).unwrap();
            updates.push(up(seq, o, p.payload));
        };
        let i1 = index
            .create("n1", "Plans/Trip.md", EntryKind::Note, 1)
            .unwrap();
        push(&mut r, "index", i1, &mut updates);
        let n1 = r
            .notes
            .entry("n1".into())
            .or_default()
            .set_text("Pack sunscreen")
            .unwrap();
        push(&mut r, "n1", n1, &mut updates);
        let i2 = index.create("n2", "Old.md", EntryKind::Note, 1).unwrap();
        push(&mut r, "index", i2, &mut updates);
        let n2 = r
            .notes
            .entry("n2".into())
            .or_default()
            .set_text("old stuff")
            .unwrap();
        push(&mut r, "n2", n2, &mut updates);
        let t = index.trash("n2", 2).unwrap();
        push(&mut r, "index", t, &mut updates);
        let i3 = index
            .create_with(
                "a1",
                "assets/photo.jpg",
                EntryKind::Attachment,
                1,
                Some(3),
                Some("image/jpeg"),
            )
            .unwrap();
        push(&mut r, "index", i3, &mut updates);
        // A hostile path from a collaborator must not escape the vault folder.
        let i4 = index.create("n3", "../../etc/passwd.md", EntryKind::Note, 1);
        if let Some(i4) = i4 {
            push(&mut r, "index", i4, &mut updates);
        }
        let attachment = ring.encrypt(Purpose::Attachment, "a1", b"JPG").unwrap();

        // n1 gets its own key and is shared with Bob.
        let bob = AccountSecret::generate();
        let (doc, req) =
            start_sharing(&ring, "n1", "Trip", &r.notes["n1"].encode_state(), seq).unwrap();
        let bob_user = PublicUser {
            id: "bob".into(),
            display_name: "Bob".into(),
            email: None,
            kind: PrincipalKind::Human,
            account_key: Some(PublicAccountKey::new(
                "bob",
                1,
                &bob.public(),
                &bob.sign_account_key("bob", 1),
            )),
        };
        let bob_env = engine::object_share_envelope(&doc, "n1", &bob_user).unwrap();
        let share_snapshot = req.snapshot.unwrap();

        let rk = RecoveryKey::generate();
        let export = EncryptedExport {
            format: EXPORT_FORMAT,
            user_id: "alice".into(),
            created_at: 0,
            recovery_envelope: Some(rk.wrap_account(&alice, "alice")),
            vaults: vec![ExportedVault {
                vault_id: vid.clone(),
                role: Role::Owner,
                encrypted_meta: create.encrypted_meta.clone(),
                keys: VaultKeysResponse {
                    current_epoch: 1,
                    envelopes: vec![create.key_envelope.clone()],
                    chain: vec![],
                    objects: vec![ObjectKeyEnvelope {
                        object_id: "n1".into(),
                        epoch: req.epoch,
                        envelope: req.vault_envelope.clone(),
                    }],
                    object_chain: vec![],
                },
                changes: ChangesResponse {
                    updates,
                    snapshots: vec![],
                    purged: vec![],
                    head_seq: seq,
                    has_more: false,
                    next_since: seq,
                },
                attachments: vec![ExportedAttachment {
                    id: "a1".into(),
                    data: attachment,
                }],
            }],
            shared_notes: vec![],
        };

        let account = unlock_with_recovery_key(&export, &rk.to_display()).unwrap();
        let got = decrypt_export(&account, &export);
        assert_eq!(got.len(), 1);
        let v = &got[0];
        assert_eq!(v.name, "Family");
        let file = |p: &str| v.files.iter().find(|f| f.path == p).map(|f| f.data.clone());
        assert_eq!(
            file("Plans/Trip.md").as_deref(),
            Some(&b"Pack sunscreen"[..])
        );
        assert_eq!(file("_trash/Old.md").as_deref(), Some(&b"old stuff"[..]));
        assert_eq!(file("assets/photo.jpg").as_deref(), Some(&b"JPG"[..]));
        assert!(
            v.files
                .iter()
                .all(|f| !f.path.contains("..") && !f.path.starts_with('/'))
        );
        assert!(unlock_with_recovery_key(&export, &RecoveryKey::generate().to_display()).is_err());

        // Bob's export of the note shared with him.
        let bob_export = EncryptedExport {
            format: EXPORT_FORMAT,
            user_id: "bob".into(),
            created_at: 0,
            recovery_envelope: None,
            vaults: vec![],
            shared_notes: vec![ExportedSharedNote {
                vault_id: vid.clone(),
                keys: ObjectKeysResponse {
                    object_id: "n1".into(),
                    role: Role::Viewer,
                    current_epoch: req.epoch,
                    envelopes: vec![bob_env],
                    chain: vec![],
                    encrypted_meta: Some(req.encrypted_meta.clone()),
                },
                changes: ChangesResponse {
                    updates: vec![],
                    snapshots: vec![Snapshot {
                        id: share_snapshot.snapshot_id.clone(),
                        object_id: "n1".into(),
                        covers_seq: share_snapshot.covers_seq,
                        author_id: "alice".into(),
                        payload: share_snapshot.payload.clone(),
                        created_at: 0,
                    }],
                    purged: vec![],
                    head_seq: seq,
                    has_more: false,
                    next_since: seq,
                },
            }],
        };
        let got = decrypt_export(&bob, &bob_export);
        assert_eq!(got[0].name, SHARED_DIR);
        assert_eq!(
            got[0].files,
            vec![RecoveredFile {
                path: "Trip.md".into(),
                data: b"Pack sunscreen".to_vec()
            }]
        );
    }
}
