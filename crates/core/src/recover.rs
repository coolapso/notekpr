//! Turning an [`EncryptedExport`] back into plain files, without a server:
//! the escape hatch behind `notekpr recover`. Given the person's account
//! secret (from their recovery key or a device), every vault they could read
//! becomes a folder of Markdown files and attachments again.
//!
//! Paths come from decrypted vault indexes, which collaborators control:
//! they are normalized here, and the code that writes them to disk must still
//! check that every component stays inside the output directory.
//!
//! A note's comments become a Markdown file of their own under
//! [`comments::EXPORT_DIR`] (`_comments/<the note's path>`).
//!
//! What an export holds is the server's word, so it is checked as the apps
//! check it ([`Known`]): each vault's keys against its owner's commitments
//! in its membership log, its content against who could write it, a shared
//! note's key against an owner proof. What fails is left out and said; a
//! vault or shared note that can't be checked at all (no log in the export,
//! one that doesn't check out, an older server's export) is still
//! recovered, in a folder marked [`UNCHECKED`], with the reason.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::comments::{self, Trust};
use crate::doc::EntryKind;
use crate::engine::{self, ObjectMeta, VaultMeta, VaultReplica};
use crate::keys::{AccountPublic, AccountSecret, RecoveryKey, VaultKeyring};
use crate::memberlog::{self, LogHeads, LogState, OwnerProofError};
use crate::paths;
use crate::protocol::{EncryptedExport, ExportedSharedNote, ExportedVault, Role};

/// Added to the folder of a vault (or of shared notes) whose content
/// couldn't be checked: it may not be what its people wrote.
pub const UNCHECKED: &str = " (unchecked)";

/// Whom the person's apps trust, to check an export against: from the
/// preferences in it ([`Known::of_export`]), and a device's own when
/// recovering with one. Without any, every vault is a first contact, as
/// for a new app (but our own vaults: those start with our key).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Known {
    /// User id → account key fingerprint.
    #[serde(default)]
    pub pins: BTreeMap<String, String>,
    /// Vault id → who started its log (vaults of notes shared with us).
    #[serde(default)]
    pub share_roots: BTreeMap<String, String>,
    /// Where each vault's log stood when our apps last verified it.
    #[serde(default)]
    pub log_heads: LogHeads,
}

impl Known {
    /// What the preferences in `export` say (nothing if it has none, or
    /// they can't be read).
    pub fn of_export(account: &AccountSecret, export: &EncryptedExport) -> Known {
        export
            .prefs
            .as_deref()
            .and_then(|ct| account.open_private(&export.user_id, ct).ok())
            .and_then(|pt| serde_json::from_slice(&pt).ok())
            .unwrap_or_default()
    }
}

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

/// Who is who in a recovered vault: people's names by user id (shown with
/// their comments; ids where unknown) and the vault's owner, whose
/// deletions of other people's comments count.
#[derive(Clone, Copy, Debug, Default)]
pub struct People<'a> {
    pub names: Option<&'a BTreeMap<String, String>>,
    pub owner: Option<&'a str>,
}

impl People<'_> {
    fn name(&self, id: &str) -> String {
        self.names
            .and_then(|n| n.get(id))
            .cloned()
            .unwrap_or_else(|| id.to_string())
    }
}

/// A note's comment threads in a recovery: checked against the vault's
/// membership log when the copy has one. An export has none, so its
/// records are taken as they are, like its notes.
fn note_threads(replica: &VaultReplica, note: &str, owner: Option<&str>) -> Vec<comments::Thread> {
    let is_owner = |u: &str| owner == Some(u);
    if replica.authors.is_some() {
        return replica.comment_threads(note, |_| None, is_owner);
    }
    replica
        .comments
        .get(note)
        .map(|log| comments::threads(log, |_| Some(Trust::Verified), is_owner))
        .unwrap_or_default()
}

/// The Markdown file of a note's comments (`None`: it has none).
fn comments_file(
    replica: &VaultReplica,
    note: &str,
    title: &str,
    people: People,
) -> Option<String> {
    let threads = note_threads(replica, note, people.owner);
    comments::to_markdown(title, replica.note(note), &threads, |u| people.name(u))
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

/// Decrypt everything in the export, checked against what its own
/// preferences say our apps trust ([`decrypt_export_with`]).
pub fn decrypt_export(account: &AccountSecret, export: &EncryptedExport) -> Vec<RecoveredVault> {
    decrypt_export_with(account, export, &Known::of_export(account, export))
}

/// Decrypt everything in the export, checked against `known`. Vaults that
/// can't be opened at all are returned with a problem and no files, so one
/// bad vault never hides the rest.
pub fn decrypt_export_with(
    account: &AccountSecret,
    export: &EncryptedExport,
    known: &Known,
) -> Vec<RecoveredVault> {
    let names: BTreeMap<String, String> = export
        .users
        .iter()
        .map(|u| (u.id.clone(), u.display_name.clone()))
        .collect();
    let mut out: Vec<RecoveredVault> = export
        .vaults
        .iter()
        .map(|v| {
            let people = People {
                names: Some(&names),
                owner: v.owner_id.as_deref(),
            };
            decrypt_vault(account, &export.user_id, v, people, known)
        })
        .collect();
    if !export.shared_notes.is_empty() {
        let folder = |name: String| RecoveredVault {
            vault_id: String::new(),
            name,
            ..Default::default()
        };
        // Those whose key was checked, and those whose key couldn't be.
        let mut checked = folder(SHARED_DIR.to_string());
        let mut unchecked = folder(format!("{SHARED_DIR}{UNCHECKED}"));
        let mut taken_checked = BTreeSet::new();
        let mut taken_unchecked = BTreeSet::new();
        for n in &export.shared_notes {
            let people = People {
                names: Some(&names),
                owner: n.owner_id.as_deref(),
            };
            match decrypt_shared_note(account, &export.user_id, n, people, known) {
                Ok((title, text, comments, why)) => {
                    let (shared, taken) = match why {
                        None => (&mut checked, &mut taken_checked),
                        Some(why) => {
                            unchecked.problems.push(format!("“{title}”: {why}"));
                            (&mut unchecked, &mut taken_unchecked)
                        }
                    };
                    let safe = safe_name(&title);
                    let path = paths::unique(&format!("{safe}.md"), |p| taken.contains(p));
                    taken.insert(path.clone());
                    if let Some(md) = comments {
                        let at =
                            paths::unique(&comments::export_path(&path), |p| taken.contains(p));
                        taken.insert(at.clone());
                        shared.files.push(RecoveredFile {
                            path: at,
                            data: md.into_bytes(),
                        });
                    }
                    shared.files.push(RecoveredFile {
                        path,
                        data: text.into_bytes(),
                    });
                }
                Err(e) => checked
                    .problems
                    .push(format!("shared note {}: {e}", n.keys.object_id)),
            }
        }
        for shared in [checked, unchecked] {
            if !shared.files.is_empty() || !shared.problems.is_empty() {
                out.push(shared);
            }
        }
    }
    out
}

/// A vault's membership log as the apps would take it: starting with our
/// key (our vault) or the one our apps pinned for whoever started it (or
/// anyone's, the first time, as the apps do), verified, and still holding
/// where our apps saw it last. `Err`: why the vault's content can't be
/// checked.
fn checked_log(
    account: &AccountSecret,
    user_id: &str,
    v: &ExportedVault,
    known: &Known,
) -> Result<LogState, String> {
    if v.member_log.is_empty() {
        return Err(if known.log_heads.contains_key(&v.vault_id) {
            "the export leaves out its membership history, which your apps have seen, so what it \
             holds may have been changed"
                .into()
        } else {
            "the export has no membership history for it, so who wrote what can't be checked".into()
        });
    }
    let (creator, key) = memberlog::creator(&v.member_log)
        .ok_or("its membership history doesn't start as a vault's does")?;
    let expected = if creator == user_id {
        Some(account.public().fingerprint())
    } else {
        known.pins.get(&creator).cloned()
    };
    if expected.is_some_and(|fp| fp != key.fingerprint()) {
        return Err(
            "its membership history starts with a key your apps never had for whoever made the vault"
                .into(),
        );
    }
    let state = LogState::verify(&v.vault_id, &key, &v.member_log)
        .map_err(|e| format!("its membership history doesn't check out ({e})"))?;
    if known
        .log_heads
        .get(&v.vault_id)
        .is_some_and(|h| !h.is_in(&v.member_log))
    {
        return Err(
            "its membership history is older than, or not the one, your apps saw last".into(),
        );
    }
    Ok(state)
}

/// `ring` without the keys `state` doesn't have its owner make, and how
/// many were left out.
fn owners_keys(ring: &VaultKeyring, state: &LogState) -> (VaultKeyring, usize) {
    let mut kept = VaultKeyring::new(ring.vault_id());
    let mut left_out = 0;
    for epoch in ring.epochs() {
        let Some(key) = ring.get(epoch) else { continue };
        if state.check_key(epoch, key).is_ok() {
            kept.insert(epoch, key.clone());
        } else {
            left_out += 1;
        }
    }
    (kept, left_out)
}

fn decrypt_vault(
    account: &AccountSecret,
    user_id: &str,
    v: &ExportedVault,
    people: People,
    known: &Known,
) -> RecoveredVault {
    let mut rv = RecoveredVault {
        vault_id: v.vault_id.clone(),
        name: v.vault_id.clone(),
        ..Default::default()
    };
    let mut ring = match engine::keyring_from_response(account, user_id, &v.vault_id, &v.keys) {
        Ok(r) => r,
        Err(e) => {
            rv.problems.push(format!("no usable vault key: {e}"));
            return rv;
        }
    };
    // Checked as the apps do: only keys its owner made, only content by
    // someone who could write it.
    let log = checked_log(account, user_id, v, known);
    let authors = match &log {
        Ok(state) => {
            let (kept, left_out) = owners_keys(&ring, state);
            if left_out > 0 {
                rv.problems.push(format!(
                    "{left_out} vault key(s) its owner never made were left out"
                ));
            }
            ring = kept;
            Some(state.authors())
        }
        Err(why) => {
            rv.problems.push(format!("can't be checked: {why}"));
            None
        }
    };
    if ring.current().is_none() {
        rv.problems.push("no usable vault key".into());
        return rv;
    }
    match VaultMeta::decrypt(&ring, &v.encrypted_meta) {
        Ok(m) => rv.name = m.name,
        Err(e) => rv.problems.push(format!("vault name unreadable: {e}")),
    }
    let mut replica = VaultReplica::new(
        &v.vault_id,
        v.role.clone(),
        ring.clone(),
        VaultMeta::default(),
    );
    replica.doc_keys = engine::object_keyrings(&ring, &v.keys);
    replica.authors = authors.clone();
    let (_, mut errors) = replica.apply_changes(&v.changes);
    if authors.is_some() {
        // What waited for a newer log than the export's is refused now.
        errors.extend(replica.set_authors(authors).1);
        if !errors.is_empty() {
            rv.problems.push(format!(
                "{} changes were left out: they couldn't be read, or weren't signed by someone \
                 who could write them",
                errors.len()
            ));
        }
    } else if !errors.is_empty() {
        rv.problems.push(format!(
            "{} encrypted changes could not be read (missing or damaged keys)",
            errors.len()
        ));
    }
    let files = vault_files(&replica, &rv.name, people, |id| {
        v.attachments
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.data.clone())
            .ok_or_else(|| "not in the export".to_string())
    });
    rv.problems.extend(files.problems);
    if log.is_err() {
        rv.name.push_str(UNCHECKED);
    }
    RecoveredVault {
        files: files.files,
        folders: files.folders,
        ..rv
    }
}

/// One vault's files, as recovered: its notes and attachments at their
/// paths (trashed ones under [`TRASH_DIR`]), its notes' comments (under
/// [`comments::EXPORT_DIR`]), its explicit folders, and the saved shapes it
/// keeps (the Personal storage) as an `.excalidrawlib` file. `attachment` gives
/// an attachment's ciphertext, or why it isn't there. Used for exports and
/// for a device's local copy alike.
pub fn vault_files(
    replica: &VaultReplica,
    name: &str,
    people: People,
    attachment: impl Fn(&str) -> Result<Vec<u8>, String>,
) -> RecoveredVault {
    let mut rv = RecoveredVault {
        vault_id: replica.id.clone(),
        name: name.to_string(),
        ..Default::default()
    };
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
    // Notes with comments (path, title, id): their files come last, so a
    // note's own path is never taken by another's comments.
    let mut commented = Vec::new();
    for e in replica.index.entries() {
        // Deleted for good: an export doesn't hold it, even while it can
        // still be undone.
        if e.purged.is_some() {
            continue;
        }
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
                if replica.comments.contains_key(&e.id) {
                    commented.push((p.clone(), e.title().to_string(), e.id.clone()));
                }
                rv.files.push(RecoveredFile {
                    path: p,
                    data: replica.note_text(&e.id).into_bytes(),
                });
            }
            EntryKind::Drawing => {
                let Some(p) = place(&e.path, trashed, &mut taken) else {
                    rv.problems.push(format!(
                        "skipped a drawing with an unusable path: {:?}",
                        e.path
                    ));
                    continue;
                };
                // As a .drawing file (Excalidraw's JSON, under our name),
                // the pictures it shows inside.
                let scene = replica
                    .notes
                    .get(&e.id)
                    .map(|d| d.drawing().scene())
                    .unwrap_or_default();
                let scene = crate::pictures::embed(&scene, |att| {
                    attachment(att)
                        .ok()
                        .and_then(|ct| replica.decrypt_attachment(att, &ct).ok())
                });
                rv.files.push(RecoveredFile {
                    path: p,
                    data: notekpr_draw::file::write_drawing(&scene).into_bytes(),
                });
            }
            EntryKind::Attachment => {
                let data = match attachment(&e.id).and_then(|ct| {
                    replica
                        .decrypt_attachment(&e.id, &ct)
                        .map_err(|err| err.to_string())
                }) {
                    Ok(d) => d,
                    Err(why) => {
                        rv.problems.push(format!("attachment {}: {why}", e.path));
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
    // The saved shapes (only the Personal storage holds any), as
    // Excalidraw's library file.
    let shapes = crate::library::items(&replica.index);
    if !shapes.is_empty()
        && let Some(p) = place(crate::account_export::SAVED_SHAPES_FILE, false, &mut taken)
    {
        rv.files.push(RecoveredFile {
            path: p,
            data: notekpr_draw::file::write_library(&shapes).into_bytes(),
        });
    }
    for (path, title, id) in commented {
        let Some(md) = comments_file(replica, &id, &title, people) else {
            continue;
        };
        if let Some(p) = place(&comments::export_path(&path), false, &mut taken) {
            rv.files.push(RecoveredFile {
                path: p,
                data: md.into_bytes(),
            });
        }
    }
    rv.files.sort_by(|a, b| a.path.cmp(&b.path));
    rv
}

/// A note shared with the person: its title, its text and its comments
/// (as a Markdown file, if it has any).
/// The owner whose grant counts for a note shared with us, as the apps take
/// it: one the export's owner proof shows, from the vault's first entry
/// (signed by the root our apps pinned, if any), their keys as our apps
/// pinned them. `Err`: why the note's key can't be checked.
fn proven_signer(
    user_id: &str,
    n: &ExportedSharedNote,
    known: &Known,
) -> Result<AccountPublic, String> {
    let signer = engine::grant_signer(&n.keys, user_id)
        .map(str::to_string)
        .or_else(|| n.owner_id.clone())
        .unwrap_or_default();
    let root = known.share_roots.get(&n.vault_id).map(String::as_str);
    let proven = memberlog::check_owner_proof(&n.vault_id, root, &n.keys.signer_proof, &signer)
        .map_err(|e| match e {
            OwnerProofError::Missing => {
                "the export doesn't show that whoever gave you its key owns its vault".to_string()
            }
            e => format!(
                "its key was given by someone its vault's history doesn't show as an owner ({e})"
            ),
        })?;
    let pinned_else = [(&proven.root, &proven.root_key), (&signer, &proven.key)]
        .into_iter()
        .any(|(who, key)| {
            known
                .pins
                .get(who)
                .is_some_and(|fp| *fp != key.fingerprint())
        });
    if pinned_else {
        return Err("its vault's owners have keys other than the ones your apps know".into());
    }
    Ok(proven.key)
}

/// A note shared with us: title, text, comments, and why its key couldn't
/// be checked (`None`: it was, as the apps check it).
fn decrypt_shared_note(
    account: &AccountSecret,
    user_id: &str,
    n: &ExportedSharedNote,
    people: People,
    known: &Known,
) -> crate::engine::Result<(String, String, Option<String>, Option<String>)> {
    let keyring = |owner: Option<&AccountPublic>| {
        engine::object_keyring_for_recipient(account, user_id, &n.vault_id, &n.keys, owner)
    };
    let checked = proven_signer(user_id, n, known).and_then(|key| {
        keyring(Some(&key))
            .map_err(|_| "its key isn't one the owner signed giving it to you".to_string())
    });
    let (ring, why) = match checked {
        Ok(ring) => (ring, None),
        Err(why) => (keyring(None)?, Some(why)),
    };
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
    let comments = comments_file(&replica, oid, &title, people);
    Ok((title, replica.note_text(oid), comments, why))
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
    use crate::comments::{Action, Record, Signed};
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
            signature: None,
        }
    }

    #[test]
    fn saved_shapes_are_recovered_as_a_library_file() {
        let alice = AccountSecret::generate();
        let meta = VaultMeta::named(crate::quick::NAME);
        let (_, ring) = new_vault(&alice, "alice", &meta, 1).unwrap();
        let vid = ring.vault_id().to_string();
        let mut r = VaultReplica::new(&vid, Role::Owner, ring, meta);
        let shapes = notekpr_draw::file::read_library(
            r#"{"type":"excalidrawlib","version":2,"libraryItems":[{"id":"box","status":"unpublished",
                "created":1,"name":"Box","elements":[{"id":"r","type":"rectangle","width":5,"height":5}]}]}"#,
            1.0,
            &mut notekpr_draw::restore::Ids::new(1),
        )
        .unwrap();
        let u = crate::library::add(&r.index, &shapes[0], usize::MAX)
            .unwrap()
            .unwrap();
        r.local_change(INDEX_OBJECT_ID, u).unwrap();
        let people = People {
            names: None,
            owner: None,
        };
        let rv = vault_files(&r, "Quick notes", people, |_| Err("none".into()));
        let file = rv
            .files
            .iter()
            .find(|f| f.path == crate::account_export::SAVED_SHAPES_FILE)
            .expect("the saved shapes");
        let back = notekpr_draw::file::read_library(
            std::str::from_utf8(&file.data).unwrap(),
            0.0,
            &mut notekpr_draw::restore::Ids::new(2),
        )
        .unwrap();
        assert_eq!(back[0].name.as_deref(), Some("Box"));
    }

    #[test]
    fn recovers_notes_attachments_trash_and_shared_notes() {
        let alice = AccountSecret::generate();
        let (create, ring) = new_vault(&alice, "alice", &VaultMeta::named("Family"), 1).unwrap();
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
        // Comments on n1: Alice's, Bob's reply, and Carol's, which Alice
        // (the owner) deletes.
        let carol = AccountSecret::generate();
        let bob = AccountSecret::generate();
        let on = crate::comments::anchor(&r.notes["n1"], 5, 14).unwrap();
        let signed = |who: &AccountSecret, author: &str, action: Action| {
            Signed::sign(who, &vid, "n1", &Record::new(author, 0, action)).unwrap()
        };
        let first = signed(
            &alice,
            "alice",
            Action::Comment {
                body: "Which one?".into(),
                anchor: on,
            },
        );
        let thread = first.decode().unwrap().id;
        let answer = signed(
            &bob,
            "bob",
            Action::Reply {
                thread: thread.clone(),
                body: "SPF 50".into(),
            },
        );
        let spam = signed(
            &carol,
            "carol",
            Action::Reply {
                thread,
                body: "Buy my sunscreen".into(),
            },
        );
        let delete = signed(
            &alice,
            "alice",
            Action::Delete {
                target: spam.decode().unwrap().id,
            },
        );
        for s in [&first, &answer, &spam, &delete] {
            seq += 1;
            let p = r.local_comment("n1", s).unwrap();
            updates.push(up(seq, "c_n1", p.payload));
        }

        // n1 gets its own key and is shared with Bob.
        let (doc, req) = start_sharing(
            &ring,
            "n1",
            &ObjectMeta::titled("Trip"),
            &r.notes["n1"].encode_state(),
            seq,
        )
        .unwrap();
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
                    shared: None,
                    share_from: Default::default(),
                },
                changes: ChangesResponse {
                    updates,
                    snapshots: vec![],
                    purged: vec![],
                    restored: vec![],
                    head_seq: seq,
                    has_more: false,
                    next_since: seq,
                },
                attachments: vec![ExportedAttachment {
                    id: "a1".into(),
                    data: attachment,
                }],
                owner_id: Some("alice".into()),
                member_log: vec![],
            }],
            shared_notes: vec![],
            users: vec![ExportedUser {
                id: "alice".into(),
                display_name: "Alice".into(),
            }],
            prefs: None,
        };

        let account = unlock_with_recovery_key(&export, &rk.to_display()).unwrap();
        let got = decrypt_export(&account, &export);
        assert_eq!(got.len(), 1);
        let v = &got[0];
        // (Made by hand without a membership log: nothing to check it by.)
        assert_eq!(v.name, format!("Family{UNCHECKED}"));
        let file = |p: &str| v.files.iter().find(|f| f.path == p).map(|f| f.data.clone());
        assert_eq!(
            file("Plans/Trip.md").as_deref(),
            Some(&b"Pack sunscreen"[..])
        );
        assert_eq!(file("_trash/Old.md").as_deref(), Some(&b"old stuff"[..]));
        assert_eq!(file("assets/photo.jpg").as_deref(), Some(&b"JPG"[..]));
        // The note's comments, with names where known; what the owner
        // deleted stays deleted.
        let md = String::from_utf8(file("_comments/Plans/Trip.md").unwrap()).unwrap();
        assert!(
            md.starts_with("# Comments on Trip\n\n## On “sunscreen”"),
            "{md}"
        );
        assert!(
            md.contains("**Alice** · ") && md.contains("> Which one?"),
            "{md}"
        );
        assert!(
            md.contains("**bob** · reply") && md.contains("> SPF 50"),
            "{md}"
        );
        assert!(!md.contains("Buy my"), "{md}");
        assert!(
            v.files
                .iter()
                .all(|f| !f.path.contains("..") && !f.path.starts_with('/'))
        );
        assert!(unlock_with_recovery_key(&export, &RecoveryKey::generate().to_display()).is_err());

        // Bob's export of the note shared with him, with a comment from
        // after it was shared.
        r.doc_keys.insert("n1".into(), doc.clone());
        let later = signed(
            &alice,
            "alice",
            Action::Comment {
                body: "Bring two".into(),
                anchor: crate::comments::anchor(&r.notes["n1"], 0, 4).unwrap(),
            },
        );
        let carried = up(
            seq + 1,
            "c_n1",
            r.local_comment("n1", &later).unwrap().payload,
        );
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
                    member_envelopes: vec![],
                    shared: None,
                    signer_proof: vec![],
                },
                changes: ChangesResponse {
                    updates: vec![carried],
                    snapshots: vec![Snapshot {
                        id: share_snapshot.snapshot_id.clone(),
                        object_id: "n1".into(),
                        covers_seq: share_snapshot.covers_seq,
                        author_id: "alice".into(),
                        payload: share_snapshot.payload.clone(),
                        created_at: 0,
                        signature: None,
                    }],
                    purged: vec![],
                    restored: vec![],
                    head_seq: seq + 1,
                    has_more: false,
                    next_since: seq + 1,
                },
                owner_id: Some("alice".into()),
            }],
            users: vec![],
            prefs: None,
        };
        let got = decrypt_export(&bob, &bob_export);
        assert_eq!(got[0].name, format!("{SHARED_DIR}{UNCHECKED}"));
        let paths: Vec<&str> = got[0].files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["_comments/Trip.md", "Trip.md"]);
        assert_eq!(got[0].files[1].data, b"Pack sunscreen");
        let md = String::from_utf8(got[0].files[0].data.clone()).unwrap();
        assert!(
            md.contains("## On “Pack”") && md.contains("**alice**") && !md.contains("Which one"),
            "only what was written after sharing: {md}"
        );
    }
}
