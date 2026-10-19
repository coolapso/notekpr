//! User-facing operations. Each one edits CRDT state locally (encrypting the
//! resulting update for sync) or calls the REST API for membership and keys.

use std::cell::RefCell;
use std::rc::Rc;

use coolpsonotes_core::doc::{EntryKind, NoteDoc};
use coolpsonotes_core::engine::{self, ObjectMeta, PinStatus, VaultMeta, VaultReplica};
use coolpsonotes_core::protocol::*;
use coolpsonotes_core::textdiff::Edit;
use coolpsonotes_core::{markdown, paths};
use leptos::prelude::Update;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::editor::syntax::Target;
use crate::editor::{self, Host};
use crate::state::{self, Route, error, info, local_update, navigate, success, with};
use crate::{api, knowledge, render, util, zipfile};

fn now() -> i64 {
    util::now_ms()
}

// ------------------------------------------------------------------ vaults

pub async fn create_vault(name: String) -> Option<String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let made = with(|c| {
        c.account
            .as_ref()
            .map(|a| engine::new_vault(a, &c.user_id, &VaultMeta::named(&name)))
    });
    let (req, ring) = match made {
        Some(Ok(x)) => x,
        Some(Err(e)) => {
            error(e.to_string());
            return None;
        }
        None => return None,
    };
    let info: VaultInfo = match api::post("/vaults", &req).await {
        Ok(i) => i,
        Err(e) => {
            error(format!("Could not create the vault: {e}"));
            return None;
        }
    };
    let id = info.id.clone();
    with(|c| {
        let r = VaultReplica::new(&id, Role::Owner, ring, VaultMeta::named(&name));
        c.replicas.insert(id.clone(), r);
        c.vault_infos.insert(id.clone(), info);
        // A brand-new vault has nothing to catch up on.
        c.loaded.insert(id.clone());
    });
    if let Some(s) = with(|c| c.socket.clone()) {
        s.send(&ClientMessage::Subscribe {
            vault_id: id.clone(),
            since: Some(0),
            object_id: None,
        });
    }
    state::publish_vaults();
    Some(id)
}

pub async fn rename_vault(vault: &str, name: &str) {
    let ct = with(|c| {
        let r = c.replicas.get_mut(vault)?;
        r.meta.name = name.trim().to_string();
        r.meta.encrypt(&r.keyring).ok()
    });
    let Some(ct) = ct else { return };
    match api::patch::<_, VaultInfo>(
        &format!("/vaults/{vault}"),
        &UpdateVaultRequest {
            encrypted_meta: Some(ct),
            archived: None,
        },
    )
    .await
    {
        Ok(i) => {
            with(|c| c.vault_infos.insert(vault.to_string(), i));
            state::publish_vaults();
        }
        Err(e) => error(format!("Rename failed: {e}")),
    }
}

pub async fn set_archived(vault: &str, archived: bool) {
    match api::patch::<_, VaultInfo>(
        &format!("/vaults/{vault}"),
        &UpdateVaultRequest {
            encrypted_meta: None,
            archived: Some(archived),
        },
    )
    .await
    {
        Ok(i) => {
            with(|c| c.vault_infos.insert(vault.to_string(), i));
            state::publish_vaults();
        }
        Err(e) => error(e.message),
    }
}

pub async fn delete_vault(vault: &str) {
    match api::delete(&format!("/vaults/{vault}")).await {
        Ok(()) => {
            with(|c| {
                c.replicas.remove(vault);
                c.vault_infos.remove(vault);
            });
            state::publish_vaults();
            navigate(Route::Home);
            success("Vault deleted.");
        }
        Err(e) => error(e.message),
    }
}

pub async fn leave_vault(vault: &str) {
    let uid = with(|c| c.user_id.clone());
    match api::delete(&format!("/vaults/{vault}/members/{uid}")).await {
        Ok(()) => {
            with(|c| {
                c.replicas.remove(vault);
                c.vault_infos.remove(vault);
            });
            state::publish_vaults();
            navigate(Route::Home);
        }
        Err(e) => error(e.message),
    }
}

// ------------------------------------------------------------------ sharing

pub fn fingerprint(user: &PublicUser) -> Option<String> {
    user.account_key
        .as_ref()?
        .verify()
        .ok()
        .map(|p| p.fingerprint())
}

pub fn pin_status(user: &PublicUser) -> Option<PinStatus> {
    let fp = fingerprint(user)?;
    Some(with(|c| match c.prefs.pins.get(&user.id) {
        None => PinStatus::New,
        Some(p) if *p == fp => PinStatus::Match,
        Some(_) => PinStatus::Changed,
    }))
}

/// Explicitly accept a (changed) key after verifying it out of band.
pub fn trust_key(user: &PublicUser) {
    if let Some(fp) = fingerprint(user) {
        with(|c| c.prefs.pins.insert(user.id.clone(), fp));
        state::save_prefs();
    }
}

fn check_trust(user: &PublicUser) -> Result<(), String> {
    let Some(fp) = fingerprint(user) else {
        return Ok(());
    };
    let st = with(|c| engine::check_pin(&mut c.prefs.pins, &user.id, &fp));
    match st {
        PinStatus::Changed => Err(format!(
            "The encryption key of {} changed since you last shared with them. This can happen if they reset their account — or if someone is intercepting. Compare fingerprints with them before continuing.",
            user.display_name
        )),
        PinStatus::New => {
            state::save_prefs();
            Ok(())
        }
        PinStatus::Match => Ok(()),
    }
}

pub async fn share(vault: &str, user: &PublicUser, role: Role) -> Result<(), String> {
    let envelope = if user.account_key.is_some() {
        check_trust(user)?;
        let env = with(|c| {
            c.replicas
                .get(vault)
                .map(|r| engine::share_envelope(&r.keyring, user))
        });
        match env {
            Some(Ok(e)) => Some(e),
            Some(Err(e)) => return Err(e.to_string()),
            None => return Err("vault not loaded".into()),
        }
    } else {
        None
    };
    let pending = envelope.is_none();
    api::post::<_, MemberInfo>(
        &format!("/vaults/{vault}/members"),
        &AddMemberRequest {
            user_id: user.id.clone(),
            role,
            key_envelope: envelope,
        },
    )
    .await
    .map_err(|e| e.message)?;
    with(|c| c.users.insert(user.id.clone(), user.clone()));
    if pending {
        info(format!(
            "{} will get access after their first sign-in.",
            user.display_name
        ));
    } else {
        success(format!("Shared with {}.", user.display_name));
    }
    Ok(())
}

pub async fn set_role(vault: &str, user_id: &str, role: Role) {
    if let Err(e) = api::patch::<_, ()>(
        &format!("/vaults/{vault}/members/{user_id}"),
        &UpdateMemberRequest { role },
    )
    .await
    {
        error(e.message);
    }
}

/// Remove a member and rotate the vault key so they cannot read anything
/// written afterwards.
pub async fn remove_member(vault: &str, user_id: &str) {
    if let Err(e) = api::delete(&format!("/vaults/{vault}/members/{user_id}")).await {
        error(e.message);
        return;
    }
    if rotate_keys(vault).await {
        success("Member removed and vault key rotated.");
    }
}

pub async fn rotate_keys(vault: &str) -> bool {
    let members: Vec<MemberInfo> = match api::get(&format!("/vaults/{vault}/members")).await {
        Ok(m) => m,
        Err(e) => {
            error(e.message);
            return false;
        }
    };
    for m in &members {
        if m.user.account_key.is_some()
            && let Err(e) = check_trust(&m.user)
        {
            error(e);
            return false;
        }
    }
    let prepared = with(|c| {
        let r = c.replicas.get(vault)?;
        let mut ring = r.keyring.clone();
        Some(engine::rotate(&mut ring, &members, &r.meta).map(|req| (ring, req)))
    });
    let (ring, req) = match prepared {
        Some(Ok(x)) => x,
        Some(Err(e)) => {
            error(e.to_string());
            return false;
        }
        None => return false,
    };
    match api::post::<_, ()>(&format!("/vaults/{vault}/keys/rotate"), &req).await {
        Ok(()) => {
            let notes = with(|c| {
                let r = c.replicas.get_mut(vault)?;
                r.keyring = ring;
                Some(r.doc_keys.keys().cloned().collect::<Vec<_>>())
            });
            // Removed members knew the keys of individually shared notes too.
            for n in notes.unwrap_or_default() {
                if let Err(e) = rotate_note_key(vault, &n).await {
                    error(format!("Could not renew the key of a shared note: {e}"));
                }
            }
            true
        }
        Err(e) => {
            error(format!("Key rotation failed: {e}"));
            false
        }
    }
}

/// Owner: give keys to members who were added before they had any.
pub async fn grant_missing_keys(vault: &str) {
    let Ok(members) = api::get::<Vec<MemberInfo>>(&format!("/vaults/{vault}/members")).await else {
        return;
    };
    let trusted: Vec<MemberInfo> = members
        .into_iter()
        .filter(|m| m.user.account_key.is_none() || check_trust(&m.user).is_ok())
        .collect();
    let env = with(|c| {
        c.replicas
            .get(vault)
            .map(|r| engine::missing_grants(&r.keyring, &trusted))
    })
    .unwrap_or_default();
    if env.is_empty() {
        return;
    }
    if api::post::<_, ()>(
        &format!("/vaults/{vault}/keys/grant"),
        &GrantKeysRequest { envelopes: env },
    )
    .await
    .is_ok()
    {
        info("Shared vault keys with new members.");
    }
}

// ------------------------------------------------------------------ sharing single notes

/// Share one note (not the vault) with someone. The note first gets its own
/// key; the person receives only that key.
pub async fn share_note(
    vault: &str,
    note: &str,
    user: &PublicUser,
    role: Role,
) -> Result<(), String> {
    if user.account_key.is_none() {
        return Err(format!(
            "{} hasn't set up encryption yet. Ask them to sign in once, then share again.",
            user.display_name
        ));
    }
    check_trust(user)?;
    ensure_note_key(vault, note).await?;
    let env = with(|c| {
        c.replicas
            .get(vault)?
            .doc_keys
            .get(note)
            .map(|ring| engine::object_share_envelope(ring, note, user))
    });
    let envelope = match env {
        Some(Ok(e)) => e,
        Some(Err(e)) => return Err(e.to_string()),
        None => return Err("The note's key is not available.".into()),
    };
    let req = AddObjectShareRequest {
        user_id: user.id.clone(),
        role,
        envelope,
    };
    match api::post::<_, ObjectShareInfo>(&format!("/vaults/{vault}/objects/{note}/shares"), &req)
        .await
    {
        Ok(_) => {}
        Err(e) if e.status == 409 && e.message.contains("already shared") => {
            api::patch::<_, ()>(
                &format!("/vaults/{vault}/objects/{note}/shares/{}", user.id),
                &UpdateMemberRequest { role },
            )
            .await
            .map_err(|e| e.message)?;
        }
        Err(e) if e.status == 409 && e.message.contains("through the vault") => {
            return Err(format!(
                "{} is a member of this vault and can already open the note.",
                user.display_name
            ));
        }
        // The key changed meanwhile (someone removed a person): reload and let the user retry.
        Err(e) if e.status == 412 => {
            state::reload_vault_keys(vault.to_string()).await;
            return Err("The note's key just changed. Please try again.".into());
        }
        Err(e) => return Err(e.message),
    }
    with(|c| c.users.insert(user.id.clone(), user.clone()));
    state::ui().shares_rev.update(|v| *v += 1);
    success(format!(
        "Shared “{}” with {}.",
        state::note_title(vault, note).unwrap_or_default(),
        user.display_name
    ));
    Ok(())
}

/// Give a note its own document key (once), re-encrypting its current
/// content under it so people it is shared with can read it.
async fn ensure_note_key(vault: &str, note: &str) -> Result<(), String> {
    for attempt in 0..3 {
        if state::is_shared_note(vault, note) {
            return Ok(());
        }
        // The new key's first snapshot must cover every stored change of the
        // note, including our own: send them and wait for the server.
        state::flush_now();
        for _ in 0..50 {
            if !with(|c| {
                c.replicas
                    .get(vault)
                    .is_some_and(|r| r.pending.iter().any(|p| p.object_id == note))
            }) {
                break;
            }
            util::sleep(100).await;
        }
        let made = with(|c| {
            let r = c.replicas.get(vault)?;
            let title = r
                .index
                .get(note)
                .map(|e| e.title().to_string())
                .unwrap_or_default();
            let st = r.note(note).map(|n| n.encode_state()).unwrap_or_default();
            Some(engine::start_sharing(&r.keyring, note, &title, &st, r.seq))
        });
        let (ring, req) = match made {
            Some(Ok(x)) => x,
            Some(Err(e)) => return Err(e.to_string()),
            None => return Err("vault not loaded".into()),
        };
        match api::put::<_, ()>(&format!("/vaults/{vault}/objects/{note}/key"), &req).await {
            Ok(()) => {
                with(|c| {
                    if let Some(r) = c.replicas.get_mut(vault) {
                        r.doc_keys.insert(note.to_string(), ring);
                    }
                });
                state::ui().tree_rev.update(|v| *v += 1);
                state::send_presence();
                return Ok(());
            }
            // Someone else started sharing it, or new changes arrived: reload and retry.
            Err(e) if (e.status == 409 || e.status == 412) && attempt < 2 => {
                state::reload_vault_keys(vault.to_string()).await;
                util::sleep(300).await;
            }
            Err(e) => return Err(e.message),
        }
    }
    Err("Could not prepare the note for sharing; please try again.".into())
}

/// Stop sharing a note with someone, and give the note a new key so they
/// can't read what is written from now on.
pub async fn unshare_note(vault: &str, note: &str, user_id: &str) -> Result<(), String> {
    api::delete(&format!("/vaults/{vault}/objects/{note}/shares/{user_id}"))
        .await
        .map_err(|e| e.message)?;
    rotate_note_key(vault, note).await?;
    state::ui().shares_rev.update(|v| *v += 1);
    Ok(())
}

pub async fn set_note_share_role(
    vault: &str,
    note: &str,
    user_id: &str,
    role: Role,
) -> Result<(), String> {
    api::patch::<_, ()>(
        &format!("/vaults/{vault}/objects/{note}/shares/{user_id}"),
        &UpdateMemberRequest { role },
    )
    .await
    .map_err(|e| e.message)
}

/// New document key for a shared note, for its current recipients.
pub async fn rotate_note_key(vault: &str, note: &str) -> Result<(), String> {
    for attempt in 0..2 {
        let shares: Vec<ObjectShareInfo> =
            api::get(&format!("/vaults/{vault}/objects/{note}/shares"))
                .await
                .map_err(|e| e.message)?;
        let users: Vec<PublicUser> = shares.into_iter().map(|s| s.user).collect();
        // Never wrap the new key to a key that changed since it was pinned.
        for u in users.iter().filter(|u| u.account_key.is_some()) {
            check_trust(u)?;
        }
        let made = with(|c| {
            let r = c.replicas.get(vault)?;
            let ring = r.doc_keys.get(note)?;
            let title = r
                .index
                .get(note)
                .map(|e| e.title().to_string())
                .unwrap_or_default();
            Some(engine::rotate_object_key(
                &r.keyring, ring, note, &title, &users,
            ))
        });
        let (next, req) = match made {
            Some(Ok(x)) => x,
            Some(Err(e)) => return Err(e.to_string()),
            None => return Ok(()),
        };
        match api::put::<_, ()>(&format!("/vaults/{vault}/objects/{note}/key"), &req).await {
            Ok(()) => {
                with(|c| {
                    if let Some(r) = c.replicas.get_mut(vault) {
                        r.doc_keys.insert(note.to_string(), next);
                    }
                });
                return Ok(());
            }
            // Recipients or keys changed meanwhile: reload and retry once.
            Err(e) if (e.status == 409 || e.status == 412) && attempt == 0 => {
                state::reload_vault_keys(vault.to_string()).await
            }
            Err(e) => return Err(e.message),
        }
    }
    Err("The note's recipients keep changing; please try again.".into())
}

/// Shared notes carry their title encrypted for their recipients.
async fn update_note_meta(vault: String, note: String) {
    for _ in 0..2 {
        let ct = with(|c| {
            let r = c.replicas.get(&vault)?;
            let ring = r.doc_keys.get(&note)?;
            let title = r.index.get(&note)?.title().to_string();
            ObjectMeta { title }.encrypt(ring, &note).ok()
        });
        let Some(encrypted_meta) = ct else { return };
        match api::put::<_, ()>(
            &format!("/vaults/{vault}/objects/{note}/meta"),
            &ObjectMetaUpdate { encrypted_meta },
        )
        .await
        {
            Err(e) if e.status == 412 => state::reload_vault_keys(vault.clone()).await,
            _ => return,
        }
    }
}

/// Remove a note someone shared with us from our list.
pub async fn leave_shared_note(vault: &str, note: &str) {
    let uid = with(|c| c.user_id.clone());
    match api::delete(&format!("/vaults/{vault}/objects/{note}/shares/{uid}")).await {
        Ok(()) | Err(api::Error { status: 404, .. }) => {
            state::navigate(Route::Home);
            state::refresh_shares().await;
        }
        Err(e) => error(e.message),
    }
}

// ------------------------------------------------------------------ notes & folders

fn unique_path(vault: &str, path: &str) -> String {
    let taken: Vec<String> = state::live_entries(vault)
        .into_iter()
        .map(|e| e.path.to_lowercase())
        .collect();
    paths::unique(path, |p| taken.contains(&p.to_lowercase()))
}

pub async fn create_note_with(
    vault: &str,
    folder: &str,
    title: &str,
    text: &str,
) -> Option<String> {
    if !state::can_write(vault) {
        error("You can only read this vault.");
        return None;
    }
    let title = if title.trim().is_empty() {
        "Untitled"
    } else {
        title.trim()
    };
    let safe: String = title
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect();
    let path = match paths::normalize_note(&paths::join(folder, &safe)) {
        Ok(p) => unique_path(vault, &p),
        Err(e) => {
            error(e.to_string());
            return None;
        }
    };
    let id = state::new_id();
    let up = with(|c| {
        c.replicas
            .get(vault)
            .and_then(|r| r.index.create(&id, &path, EntryKind::Note, now()))
    });
    if let Some(u) = up {
        local_update(vault, INDEX_OBJECT_ID, u);
    }
    state::ensure_note_loaded(vault, &id);
    if !text.is_empty() {
        // Large texts become several updates, each within the server's limit.
        state::set_note_text(vault, &id, text);
    }
    state::ui().tree_rev.update(|v| *v += 1);
    Some(id)
}

pub fn new_note(vault: &str, folder: &str) {
    let (v, f) = (vault.to_string(), folder.to_string());
    leptos::task::spawn_local(async move {
        if let Some(id) = create_note_with(&v, &f, "Untitled", "").await {
            navigate(Route::Note(v, id));
            crate::views::note::focus_title_soon();
        }
    });
}

pub fn create_folder(vault: &str, path: &str) {
    let Ok(p) = paths::normalize(path) else {
        error("Invalid folder name");
        return;
    };
    let p = unique_path(vault, &p);
    let id = state::new_id();
    let up = with(|c| {
        c.replicas
            .get(vault)
            .and_then(|r| r.index.create(&id, &p, EntryKind::Folder, now()))
    });
    if let Some(u) = up {
        local_update(vault, INDEX_OBJECT_ID, u);
        state::ui().tree_rev.update(|v| *v += 1);
    }
}

/// Rename a note, attachment or explicit folder. Wiki links pointing to a
/// renamed note are updated in every note of the vault.
pub fn rename_entry(vault: &str, id: &str, new_name: &str) -> Result<(), String> {
    let e = state::entry(vault, id).ok_or("not found")?;
    let new_name = new_name.trim();
    if new_name.is_empty() || new_name.contains('/') {
        return Err("Names can't be empty or contain '/'.".into());
    }
    let parent = paths::parent(&e.path).to_string();
    let mut new_path = paths::join(&parent, new_name);
    if e.kind == EntryKind::Note {
        new_path = paths::normalize_note(&new_path).map_err(|e| e.to_string())?;
    } else {
        new_path = paths::normalize(&new_path).map_err(|e| e.to_string())?;
    }
    if new_path == e.path {
        return Ok(());
    }
    if state::live_entries(vault)
        .iter()
        .any(|x| x.id != id && x.path.eq_ignore_ascii_case(&new_path))
    {
        return Err("Something with that name already exists here.".into());
    }
    if e.kind == EntryKind::Folder {
        let up = with(|c| {
            c.replicas
                .get(vault)
                .and_then(|r| r.index.rename_folder(&e.path, &new_path, now()))
        });
        if let Some(u) = up {
            local_update(vault, INDEX_OBJECT_ID, u);
        }
    } else {
        let old_title = e.title().to_string();
        let old_path = e.path.clone();
        let up = with(|c| {
            c.replicas
                .get(vault)
                .and_then(|r| r.index.rename(id, &new_path, now()))
        });
        if let Some(u) = up {
            local_update(vault, INDEX_OBJECT_ID, u);
        }
        if state::is_shared_note(vault, id) && paths::title(&new_path) != old_title {
            leptos::task::spawn_local(update_note_meta(vault.to_string(), id.to_string()));
        }
        if e.kind == EntryKind::Note && paths::title(&new_path) != old_title {
            update_links_after_rename(vault, id, &old_path, paths::title(&new_path));
        }
    }
    state::ui().tree_rev.update(|v| *v += 1);
    Ok(())
}

fn update_links_after_rename(vault: &str, id: &str, old_path: &str, new_title: &str) {
    // Resolve against the old tree so links that used the old name still match.
    let mut cands: Vec<(String, String)> = state::live_entries(vault)
        .into_iter()
        .map(|e| (e.id, e.path))
        .collect();
    for c in &mut cands {
        if c.0 == id {
            c.1 = old_path.to_string();
        }
    }
    let notes = knowledge::notes_with_text(vault);
    let mut changed = 0;
    for (nid, npath, text) in notes {
        let path = if nid == id {
            old_path.to_string()
        } else {
            npath
        };
        let rewritten = markdown::rewrite_links(&text, new_title, |l| {
            markdown::resolve(l, &path, &cands) == Some(id)
        });
        if let Some(new_text) = rewritten
            && state::set_note_text(vault, &nid, &new_text) > 0
        {
            changed += 1;
        }
    }
    if changed > 0 {
        info(format!(
            "Updated links in {changed} note{}.",
            if changed == 1 { "" } else { "s" }
        ));
        refresh_open_editor(vault);
    }
}

fn refresh_open_editor(vault: &str) {
    let open = with(|c| {
        c.editor
            .as_ref()
            .map(|(v, n, e)| (v.clone(), n.clone(), e.clone()))
    });
    if let Some((v, n, ed)) = open
        && v == vault
    {
        let t = with(|c| c.replicas.get(&v).map(|r| r.note_text(&n)));
        if let Some(t) = t {
            ed.set_text(t, false);
        }
    }
}

pub fn move_entry(vault: &str, id: &str, folder: &str) -> Result<(), String> {
    let e = state::entry(vault, id).ok_or("not found")?;
    let name = paths::file_name(&e.path).to_string();
    let target = paths::join(folder, &name);
    if e.kind == EntryKind::Folder && (folder == e.path || paths::is_within(folder, &e.path)) {
        return Err("A folder can't be moved into itself.".into());
    }
    let target = unique_path(vault, &target);
    let up = with(|c| {
        let r = c.replicas.get(vault)?;
        if e.kind == EntryKind::Folder {
            r.index.rename_folder(&e.path, &target, now())
        } else {
            r.index.rename(id, &target, now())
        }
    });
    if let Some(u) = up {
        local_update(vault, INDEX_OBJECT_ID, u);
        state::ui().tree_rev.update(|v| *v += 1);
    }
    Ok(())
}

/// Move a folder that only exists implicitly (no folder entry).
pub fn rename_implicit_folder(vault: &str, old: &str, new_name: &str) -> Result<(), String> {
    let new_path =
        paths::normalize(&paths::join(paths::parent(old), new_name)).map_err(|e| e.to_string())?;
    let up = with(|c| {
        c.replicas
            .get(vault)
            .and_then(|r| r.index.rename_folder(old, &new_path, now()))
    });
    if let Some(u) = up {
        local_update(vault, INDEX_OBJECT_ID, u);
        state::ui().tree_rev.update(|v| *v += 1);
    }
    Ok(())
}

pub fn trash(vault: &str, id: &str) {
    let up = with(|c| c.replicas.get(vault).and_then(|r| r.index.trash(id, now())));
    if let Some(u) = up {
        local_update(vault, INDEX_OBJECT_ID, u);
        state::ui().tree_rev.update(|v| *v += 1);
        info("Moved to trash.");
    }
}

pub fn trash_folder(vault: &str, folder: &str) {
    let up = with(|c| {
        c.replicas
            .get(vault)
            .and_then(|r| r.index.trash_folder(folder, now()))
    });
    if let Some(u) = up {
        local_update(vault, INDEX_OBJECT_ID, u);
        state::ui().tree_rev.update(|v| *v += 1);
        info("Folder moved to trash.");
    }
}

pub fn restore(vault: &str, id: &str) {
    let up = with(|c| c.replicas.get(vault).and_then(|r| r.index.restore(id)));
    if let Some(u) = up {
        local_update(vault, INDEX_OBJECT_ID, u);
        state::ui().tree_rev.update(|v| *v += 1);
        success("Restored.");
    }
}

/// Permanently delete a trashed entry (server data and index entry).
pub async fn purge(vault: &str, id: &str) {
    let kind = state::entry(vault, id).map(|e| e.kind);
    let r = match kind {
        Some(EntryKind::Attachment) => {
            api::delete(&format!("/vaults/{vault}/attachments/{id}")).await
        }
        Some(EntryKind::Note) => api::delete(&format!("/vaults/{vault}/objects/{id}")).await,
        _ => Ok(()),
    };
    match r {
        Ok(()) | Err(api::Error { status: 404, .. }) => {
            let up = with(|c| c.replicas.get(vault).and_then(|r| r.index.remove(id)));
            if let Some(u) = up {
                local_update(vault, INDEX_OBJECT_ID, u);
            }
            state::ui().tree_rev.update(|v| *v += 1);
        }
        Err(e) => error(e.message),
    }
}

pub async fn empty_trash(vault: &str) {
    let ids: Vec<String> = with(|c| {
        c.replicas.get(vault).map(|r| {
            r.index
                .entries()
                .into_iter()
                .filter(|e| !e.is_live())
                .map(|e| e.id)
                .collect()
        })
    })
    .unwrap_or_default();
    for id in ids {
        purge(vault, &id).await;
    }
    success("Trash emptied.");
}

pub fn toggle_favorite(vault: &str, note: &str) {
    with(|c| {
        let key = (vault.to_string(), note.to_string());
        if let Some(i) = c.prefs.favorites.iter().position(|f| f == &key) {
            c.prefs.favorites.remove(i);
        } else {
            c.prefs.favorites.push(key);
        }
    });
    state::save_prefs();
}

pub fn is_favorite(vault: &str, note: &str) -> bool {
    with(|c| {
        c.prefs
            .favorites
            .iter()
            .any(|(v, n)| v == vault && n == note)
    })
}

/// Follow a wiki link; create the note if it doesn't exist yet.
/// Go to a heading of a note being opened (`[[Note#Heading]]`): the caret
/// in the editor, or the heading in reading view. Retries briefly while the
/// note's view is still being set up.
pub fn jump_to_heading(vault: String, note: String, heading: String) {
    fn attempt(vault: String, note: String, heading: String, left: u32) {
        if try_jump(&vault, &note, &heading) || left == 0 {
            return;
        }
        util::set_timeout(100, move || attempt(vault, note, heading, left - 1));
    }
    util::set_timeout(50, move || attempt(vault, note, heading, 20));
}

fn try_jump(vault: &str, note: &str, heading: &str) -> bool {
    if state::current_route().note() != Some((vault, note)) {
        return false;
    }
    let text = with(|c| c.replicas.get(vault).map(|r| r.note_text(note))).unwrap_or_default();
    let want = heading.to_lowercase();
    let outline = knowledge::outline(vault, note, &text);
    let Some(i) = outline
        .iter()
        .position(|(_, t, _)| t.trim().to_lowercase() == want)
    else {
        return true; // no such heading: stay at the top
    };
    if state::ui().mode.get_untracked() == state::ViewMode::Reading {
        return match util::document().query_selector(&format!("article.reading [data-h=\"{i}\"]")) {
            Ok(Some(el)) => {
                el.scroll_into_view();
                true
            }
            _ => false,
        };
    }
    let ed = with(|c| {
        c.editor
            .as_ref()
            .filter(|(v, n, _)| v == vault && n == note)
            .map(|(_, _, e)| e.clone())
    });
    match ed {
        Some(ed) => {
            let off = outline[i].2;
            ed.focus();
            ed.set_selection(off, off);
            true
        }
        None => false,
    }
}

pub fn open_wiki(vault: &str, target: &str, from_path: &str) {
    let (name, heading) = match target.split_once('#') {
        Some((n, h)) => (n.trim(), Some(h.trim()).filter(|h| !h.is_empty())),
        None => (target.trim(), None),
    };
    if name.is_empty() {
        if let (Some(h), Some((v, n))) = (heading, state::current_route().note()) {
            jump_to_heading(v.to_string(), n.to_string(), h.to_string());
        }
        return;
    }
    if let Some(id) = knowledge::resolve_target(vault, name, from_path) {
        match state::entry(vault, &id).map(|e| e.kind) {
            Some(EntryKind::Attachment) => {
                let (v, i) = (vault.to_string(), id.clone());
                leptos::task::spawn_local(async move { open_attachment(&v, &i).await });
            }
            _ => {
                if state::current_route().note() != Some((vault, id.as_str())) {
                    navigate(Route::Note(vault.to_string(), id.clone()));
                }
                if let Some(h) = heading {
                    jump_to_heading(vault.to_string(), id, h.to_string());
                }
            }
        }
        return;
    }
    let (folder, title) = if name.contains('/') {
        (
            paths::parent(name).to_string(),
            paths::title(name).to_string(),
        )
    } else {
        (
            paths::parent(from_path).to_string(),
            paths::title(name).to_string(),
        )
    };
    let v = vault.to_string();
    leptos::task::spawn_local(async move {
        if let Some(id) = create_note_with(&v, &folder, &title, "").await {
            navigate(Route::Note(v, id));
        }
    });
}

// ------------------------------------------------------------------ attachments

thread_local! {
    static WAITING_EDITORS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

pub fn cached_attachment_url(vault: &str, id: &str) -> Option<String> {
    with(|c| c.attachment_urls.get(&format!("{vault}:{id}")).cloned())
}

pub async fn attachment_bytes(vault: &str, id: &str) -> Option<Vec<u8>> {
    let ct = api::get_bytes(&format!("/vaults/{vault}/attachments/{id}"))
        .await
        .ok()?;
    with(|c| c.replicas.get(vault)?.decrypt_attachment(id, &ct).ok())
}

/// Load, decrypt and cache an attachment as a blob URL.
pub async fn load_attachment(vault: &str, id: &str) -> Option<String> {
    let key = format!("{vault}:{id}");
    if let Some(u) = with(|c| c.attachment_urls.get(&key).cloned()) {
        return Some(u);
    }
    if !with(|c| c.attachment_loading.insert(key.clone())) {
        return None;
    }
    let name = state::entry(vault, id).map(|e| e.path).unwrap_or_default();
    let bytes = attachment_bytes(vault, id).await;
    let url = bytes.and_then(|b| util::blob_url(&b, util::mime_for(&name)));
    with(|c| {
        c.attachment_loading.remove(&key);
        if let Some(u) = &url {
            c.attachment_urls.insert(key.clone(), u.clone());
        }
    });
    url
}

/// Show an attachment in a new tab if the browser can display it safely
/// (raster images, PDFs); download anything else.
pub async fn open_attachment(vault: &str, id: &str) {
    let name = state::entry(vault, id).map(|e| e.path).unwrap_or_default();
    if !util::opens_inline(&name) {
        download_attachment(vault, id).await;
        return;
    }
    if let Some(url) = load_attachment(vault, id).await {
        let _ = util::window().open_with_url_and_target_and_features(
            &url,
            "_blank",
            "noopener,noreferrer",
        );
    }
}

pub async fn download_attachment(vault: &str, id: &str) {
    let name = state::entry(vault, id)
        .map(|e| paths::file_name(&e.path).to_string())
        .unwrap_or_else(|| "file".into());
    // Never hand the browser a type it might render as a document of this origin.
    let mime = if util::opens_inline(&name) {
        util::mime_for(&name)
    } else {
        "application/octet-stream"
    };
    if let Some(b) = attachment_bytes(vault, id).await {
        util::download(&name, mime, &b);
    } else {
        error("Could not download the file.");
    }
}

/// Encrypt and upload files, add them to the index and return embed snippets.
pub async fn upload_files(vault: &str, files: Vec<web_sys::File>, folder: &str) -> Vec<String> {
    let max = state::ui()
        .server
        .get_untracked()
        .map(|s| s.max_attachment_bytes)
        .unwrap_or(25 << 20);
    let mut embeds = Vec::new();
    for f in files {
        let name = f.name();
        let size = f.size() as u64;
        if size > max {
            error(format!(
                "{name} is larger than {} and can't be uploaded.",
                util::human_bytes(max)
            ));
            continue;
        }
        let Some(bytes) = util::read_file(&f).await else {
            continue;
        };
        if let Some(path) = upload_bytes(vault, &paths::join(folder, &name), &bytes).await {
            let fname = paths::file_name(&path).to_string();
            embeds.push(if util::is_image(&fname) {
                format!("![[{fname}]]")
            } else {
                format!("[[{fname}]]")
            });
        }
    }
    embeds
}

pub async fn upload_bytes(vault: &str, path: &str, bytes: &[u8]) -> Option<String> {
    let id = state::new_id();
    let ct = with(|c| c.replicas.get(vault)?.encrypt_attachment(&id, bytes).ok())?;
    if let Err(e) = api::put_bytes(&format!("/vaults/{vault}/attachments/{id}"), &ct).await {
        error(format!("Upload failed: {e}"));
        return None;
    }
    let path = unique_path(vault, &paths::normalize(path).ok()?);
    let mime = util::mime_for(&path);
    let up = with(|c| {
        c.replicas.get(vault).and_then(|r| {
            r.index.create_with(
                &id,
                &path,
                EntryKind::Attachment,
                now(),
                Some(bytes.len() as u64),
                Some(mime),
            )
        })
    });
    if let Some(u) = up {
        local_update(vault, INDEX_OBJECT_ID, u);
    }
    state::ui().tree_rev.update(|v| *v += 1);
    Some(path)
}

// ------------------------------------------------------------------ export / import

pub async fn export_vault(vault: &str) {
    let name = state::vault_name(vault);
    info("Preparing export…");
    let mut entries = Vec::new();
    for e in state::live_entries(vault) {
        match e.kind {
            EntryKind::Note => {
                let text =
                    with(|c| c.replicas.get(vault).map(|r| r.note_text(&e.id))).unwrap_or_default();
                entries.push(zipfile::Entry {
                    name: e.path.clone(),
                    data: text.into_bytes(),
                });
            }
            EntryKind::Attachment => match attachment_bytes(vault, &e.id).await {
                Some(b) => entries.push(zipfile::Entry {
                    name: e.path.clone(),
                    data: b,
                }),
                None => error(format!("Skipped {} (download failed)", e.path)),
            },
            EntryKind::Folder => entries.push(zipfile::Entry {
                name: format!("{}/.keep", e.path),
                data: vec![],
            }),
        }
    }
    let zip = match zipfile::write(&entries) {
        Ok(z) => z,
        Err(e) => {
            error(format!("Export failed: {e}"));
            return;
        }
    };
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    util::download(&format!("{}.zip", safe.trim()), "application/zip", &zip);
    success(format!("Exported {} items.", entries.len()));
}

/// Import Markdown files and attachments, preserving folders.
pub async fn import(vault: &str, mut files: Vec<(String, Vec<u8>)>) {
    files.retain(|(p, _)| !p.split('/').any(|s| s.starts_with('.') || s == "__MACOSX"));
    // Drop a single common top-level folder (from folder uploads or zips).
    let top: Option<String> = files
        .first()
        .and_then(|(p, _)| p.split('/').next().map(str::to_string));
    if let Some(t) = top
        && files.len() > 1
        && files.iter().all(|(p, _)| p.starts_with(&format!("{t}/")))
    {
        for (p, _) in &mut files {
            *p = p[t.len() + 1..].to_string();
        }
    }
    let total = files.len();
    let mut notes = 0;
    let mut atts = 0;
    let max = state::max_update_bytes();
    for (i, (path, data)) in files.into_iter().enumerate() {
        // Send in pushes of bounded size as we go, and let the page breathe.
        if state::outbox_bytes(vault) >= max {
            state::flush_now();
        }
        if i % 20 == 19 {
            util::sleep(0).await;
        }
        if paths::is_note(&path) {
            let text = String::from_utf8_lossy(&data).replace("\r\n", "\n");
            let folder = paths::parent(&path).to_string();
            let title = paths::title(&path).to_string();
            if create_note_with(vault, &folder, &title, &text)
                .await
                .is_some()
            {
                notes += 1;
            }
        } else if upload_bytes(vault, &path, &data).await.is_some() {
            atts += 1;
        }
    }
    state::flush_now();
    success(format!(
        "Imported {notes} notes and {atts} files (of {total})."
    ));
}

pub async fn files_to_import(files: Vec<web_sys::File>) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for f in files {
        let rel = js_sys::Reflect::get(&f, &"webkitRelativePath".into())
            .ok()
            .and_then(|v| v.as_string())
            .filter(|s| !s.is_empty());
        let name = rel.unwrap_or_else(|| f.name());
        let Some(bytes) = util::read_file(&f).await else {
            continue;
        };
        if name.to_lowercase().ends_with(".zip") {
            match zipfile::read(&bytes) {
                Ok(entries) => out.extend(entries.into_iter().map(|e| (e.name, e.data))),
                Err(e) => error(format!("{name}: {e}")),
            }
        } else {
            out.push((name, bytes));
        }
    }
    out
}

// ------------------------------------------------------------------ history

pub fn restore_revision(vault: &str, note: &str, text: &str) {
    if state::set_note_text(vault, note, text) > 0 {
        refresh_open_editor(vault);
        success("Restored this version. The previous text remains in history.");
    }
}

// ------------------------------------------------------------------ editor host

pub struct NoteHost {
    pub vault: String,
    pub note: String,
    /// The note is shared with us individually: the rest of its vault
    /// (linked notes, attachments) is out of reach.
    pub shared: bool,
}

impl NoteHost {
    pub fn new(vault: &str, note: &str) -> Rc<NoteHost> {
        Self::with_mode(vault, note, false)
    }

    pub fn for_shared(vault: &str, note: &str) -> Rc<NoteHost> {
        Self::with_mode(vault, note, true)
    }

    fn with_mode(vault: &str, note: &str, shared: bool) -> Rc<NoteHost> {
        state::ensure_note_loaded(vault, note);
        with(|c| {
            if let Some(n) = c
                .replicas
                .get_mut(vault)
                .and_then(|r| r.notes.get_mut(note))
                && !n.undo_enabled()
            {
                n.enable_undo_with_clock(|| js_sys::Date::now() as u64);
            }
        });
        Rc::new(NoteHost {
            vault: vault.to_string(),
            note: note.to_string(),
            shared,
        })
    }

    fn path(&self) -> String {
        state::entry(&self.vault, &self.note)
            .map(|e| e.path)
            .unwrap_or_default()
    }

    fn take(&self, f: impl FnOnce(&mut NoteDoc) -> Option<Vec<u8>>) -> Option<String> {
        let (u, t) = with(|c| {
            let n = c.replicas.get_mut(&self.vault)?.notes.get_mut(&self.note)?;
            let u = f(n)?;
            Some((u, n.text()))
        })?;
        local_update(&self.vault, &self.note, u);
        Some(t)
    }
}

impl Host for NoteHost {
    fn edit(&self, base: &str, edits: &[Edit]) -> bool {
        // One edit is one update; refuse what the server could never store
        // rather than block the vault's sync with it.
        let inserted = crate::sizes::inserted_bytes(edits);
        let max = state::max_update_bytes();
        if inserted > crate::sizes::insert_budget(max) {
            error(format!(
                "This is too large to add at once ({}; the server accepts {} per change). Add it in smaller parts, or attach it as a file.",
                util::human_bytes(inserted as u64),
                util::human_bytes(max as u64)
            ));
            return false;
        }
        // The editor's text and the CRDT can differ when remote text arrived
        // during an IME composition: the editor's offsets are then relative to
        // an outdated text. Move simple edits onto the current text, or drop
        // them; either way the editor reloads the note's text afterwards.
        let res = with(|c| {
            let n = c.replicas.get_mut(&self.vault)?.notes.get_mut(&self.note)?;
            let current = n.text();
            let in_sync = current == base;
            let rebased;
            let edits = if in_sync {
                edits
            } else {
                match editor::rebase(base, &current, edits) {
                    Some(e) => {
                        rebased = e;
                        &rebased[..]
                    }
                    None => return Some((None, 0, false)),
                }
            };
            // Out-of-range offsets would make yrs panic.
            if !editor::edits_fit(&current, edits) {
                return Some((None, 0, false));
            }
            Some((n.apply_edits(edits), n.len(), in_sync))
        });
        let Some((update, len, in_sync)) = res else {
            return true;
        };
        if !in_sync {
            util::warn(if update.is_some() {
                "the note changed during text input; the input was moved onto the current text"
            } else {
                "the note changed during text input; the input could not be applied"
            });
        }
        if let Some(u) = update {
            local_update(&self.vault, &self.note, u);
            state::touch(&self.vault, &self.note, len);
            let ui = state::ui();
            let d = with(|c| c.content_debounce.clone());
            d.run(400, move || ui.content_rev.update(|v| *v += 1));
        }
        in_sync
    }

    fn text(&self) -> Option<String> {
        with(|c| {
            c.replicas
                .get(&self.vault)?
                .notes
                .get(&self.note)
                .map(|n| n.text())
        })
    }

    fn undo(&self) -> Option<String> {
        self.take(|n| n.undo())
    }

    fn redo(&self) -> Option<String> {
        self.take(|n| n.redo())
    }

    fn boundary(&self) {
        with(|c| {
            if let Some(n) = c
                .replicas
                .get_mut(&self.vault)
                .and_then(|r| r.notes.get_mut(&self.note))
            {
                n.undo_boundary();
            }
        });
    }

    fn link_exists(&self, target: &str) -> bool {
        !self.shared && knowledge::link_exists(&self.vault, target)
    }

    fn open_link(&self, target: &Target, _new_tab: bool) {
        match target {
            Target::Url(u) if render::safe_external(u) => {
                let _ = util::window().open_with_url_and_target_and_features(
                    u.trim(),
                    "_blank",
                    "noopener,noreferrer",
                );
            }
            Target::Url(_) => info("Only web and e-mail links can be opened from a note."),
            Target::Wiki(_) if self.shared => {
                info("Only this note is shared with you, not the notes it links to.")
            }
            Target::Wiki(t) => open_wiki(&self.vault, t, &self.path()),
        }
    }

    fn image_url(&self, target: &str) -> Option<String> {
        if self.shared {
            return None;
        }
        let id = knowledge::resolve_attachment(&self.vault, target)?;
        if let Some(u) = cached_attachment_url(&self.vault, &id) {
            return Some(u);
        }
        let v = self.vault.clone();
        leptos::task::spawn_local(async move {
            if load_attachment(&v, &id).await.is_some() {
                let ed = with(|c| {
                    c.editor
                        .as_ref()
                        .filter(|(ev, _, _)| *ev == v)
                        .map(|(_, _, e)| e.clone())
                });
                if let Some(ed) = ed {
                    ed.refresh();
                }
            }
        });
        None
    }

    fn selection_changed(&self, anchor: usize, head: usize) {
        with(|c| c.cursor = Some((anchor, head)));
        state::send_presence();
    }

    fn insert_files(&self, files: Vec<web_sys::File>, _at: usize) {
        if self.shared {
            error("Files can't be added to a note shared with you.");
            return;
        }
        let (v, n) = (self.vault.clone(), self.note.clone());
        leptos::task::spawn_local(async move {
            let embeds = upload_files(&v, files, "attachments").await;
            insert_embeds(&v, &n, &embeds);
        });
    }
}

/// Insert links to uploaded files into the note they were added to — only
/// if that note is still the one open (uploads take a while).
pub fn insert_embeds(vault: &str, note: &str, embeds: &[String]) {
    if embeds.is_empty() {
        return;
    }
    let ed = with(|c| {
        c.editor
            .as_ref()
            .filter(|(v, n, _)| v == vault && n == note)
            .map(|(_, _, e)| e.clone())
    });
    match ed {
        Some(ed) => ed.insert_text(&embeds.join("\n")),
        None => info(format!(
            "Uploaded {} to “attachments”. The note was closed meanwhile; add {} to link it.",
            if embeds.len() == 1 {
                "the file".to_string()
            } else {
                format!("{} files", embeds.len())
            },
            embeds.join(" ")
        )),
    }
}

/// Pick files with a hidden `<input type=file>`.
pub fn pick_files(
    directory: bool,
    accept: &str,
    on_files: impl FnOnce(Vec<web_sys::File>) + 'static,
) {
    let doc = util::document();
    let input: web_sys::HtmlInputElement = doc.create_element("input").unwrap().unchecked_into();
    input.set_type("file");
    input.set_multiple(true);
    if !accept.is_empty() {
        input.set_accept(accept);
    }
    if directory {
        let _ = input.set_attribute("webkitdirectory", "");
    }
    let cell = Rc::new(RefCell::new(Some(on_files)));
    let inp = input.clone();
    let cb = wasm_bindgen::closure::Closure::wrap(Box::new(move |_e: web_sys::Event| {
        let files: Vec<web_sys::File> = inp
            .files()
            .map(|l| (0..l.length()).filter_map(|i| l.get(i)).collect())
            .unwrap_or_default();
        if let Some(f) = cell.borrow_mut().take() {
            f(files);
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    let _ = input.add_event_listener_with_callback("change", cb.as_ref().unchecked_ref());
    cb.forget();
    input.click();
}
