//! Every note has its own key: new notes, sharing a note, removing a member.

mod common;

use common::*;
use notekpr_client::{manage, note_shares, ops};
use notekpr_core::crypto::sym_epoch;
use notekpr_core::keys::{FIRST_DOC_EPOCH, is_doc_epoch};
use notekpr_core::protocol::{ChangesResponse, Role, VaultKeysResponse};

async fn changes(s: &notekpr_client::session::Session, vid: &str, oid: &str) -> ChangesResponse {
    s.api
        .get(&format!(
            "/vaults/{vid}/objects/{oid}/changes?since=0&snapshots=true"
        ))
        .await
        .unwrap()
}

fn note_epoch(k: &VaultKeysResponse, oid: &str) -> Option<u32> {
    k.objects
        .iter()
        .filter(|o| o.object_id == oid)
        .map(|o| o.epoch)
        .max()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_note_is_never_under_the_vault_key_and_sharing_starts_a_fresh_key() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    manage::create_vault(&alice, "Private").await.unwrap();
    let ah = hub(&alice).await;
    ops::put_document(&ah, "Private", "Trip.md", "first draft\n", None)
        .await
        .unwrap();
    let v = ah.resolve("Private").await.unwrap();
    let (vid, oid) = {
        let g = v.lock().await;
        (g.id().to_string(), g.find_note("Trip").unwrap().1.id)
    };
    ah.sync_vault(&vid).await.unwrap();
    assert!(v.lock().await.outbox.new_keys.is_empty(), "key registered");

    // The server has the note's key, and nobody it is shared with.
    let k = alice.api.vault_keys(&vid).await.unwrap();
    assert_eq!(note_epoch(&k, &oid), Some(FIRST_DOC_EPOCH));
    assert_eq!(k.shared.as_deref(), Some(&[][..]));
    assert!(!v.lock().await.replica.is_shared_object(&oid));
    // None of its content was ever encrypted with the vault key.
    let ch = changes(&alice, &vid, &oid).await;
    assert!(!ch.updates.is_empty());
    assert!(
        ch.updates
            .iter()
            .all(|u| is_doc_epoch(sym_epoch(&u.payload).unwrap()))
    );

    // Sharing moves it to a fresh epoch; Bob reads the current text but
    // receives nothing written before.
    note_shares::share_note(&alice, &v, "Trip", "bob", Role::Viewer, false)
        .await
        .unwrap();
    let k = alice.api.vault_keys(&vid).await.unwrap();
    assert_eq!(note_epoch(&k, &oid), Some(FIRST_DOC_EPOCH + 1));
    assert_eq!(k.shared, Some(vec![oid.clone()]));
    assert!(v.lock().await.replica.is_shared_object(&oid));
    let (_, text) = note_shares::read_shared(&bob, "Trip").await.unwrap();
    assert_eq!(text, "first draft\n");
    let ch = changes(&bob, &vid, &oid).await;
    assert!(!ch.snapshots.is_empty());
    let epochs = ch
        .updates
        .iter()
        .map(|u| &u.payload)
        .chain(ch.snapshots.iter().map(|s| &s.payload))
        .map(|p| sym_epoch(p).unwrap());
    assert!(
        epochs.into_iter().all(|e| e > FIRST_DOC_EPOCH),
        "nothing from before sharing"
    );

    // Edits after sharing reach Bob.
    ops::put_document(&ah, "Private", "Trip.md", "first draft\nsecond\n", None)
        .await
        .unwrap();
    ah.sync_vault(&vid).await.unwrap();
    let (_, text) = note_shares::read_shared(&bob, "Trip").await.unwrap();
    assert_eq!(text, "first draft\nsecond\n");

    // A member who joins later gets the note's keys through the vault key.
    let carol = login(&srv, "carol").await;
    manage::share(&alice, &v, "carol", Role::Viewer, false)
        .await
        .unwrap();
    let ch = hub(&carol).await;
    assert_eq!(
        note(&ch, &vid, "Trip").await.as_deref(),
        Some("first draft\nsecond\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_a_member_renews_every_notes_key() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    let carol = login(&srv, "carol").await;
    manage::create_vault(&alice, "Team").await.unwrap();
    let ah = hub(&alice).await;
    let v = ah.resolve("Team").await.unwrap();
    let vid = v.lock().await.id().to_string();
    manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .unwrap();
    for (path, text) in [("A.md", "a\n"), ("B.md", "b\n"), ("Shared.md", "s\n")] {
        ops::put_document(&ah, "Team", path, text, None)
            .await
            .unwrap();
    }
    ah.sync_vault(&vid).await.unwrap();
    note_shares::share_note(&alice, &v, "Shared", "carol", Role::Editor, false)
        .await
        .unwrap();
    let before = alice.api.vault_keys(&vid).await.unwrap();
    let ids: Vec<String> = {
        let g = v.lock().await;
        ["A", "B", "Shared"]
            .iter()
            .map(|p| g.find_note(p).unwrap().1.id)
            .collect()
    };

    manage::unshare(&alice, &v, "bob").await.unwrap();
    let after = alice.api.vault_keys(&vid).await.unwrap();
    for oid in &ids {
        assert_eq!(
            note_epoch(&after, oid),
            note_epoch(&before, oid).map(|e| e + 1),
            "every note moved to a new key"
        );
    }

    // Everyone left keeps working: Alice writes, Carol reads her shared note.
    ops::put_document(&ah, "Team", "Shared.md", "s\nafter\n", None)
        .await
        .unwrap();
    ops::put_document(&ah, "Team", "A.md", "a\nafter\n", None)
        .await
        .unwrap();
    ah.sync_vault(&vid).await.unwrap();
    let (_, text) = note_shares::read_shared(&carol, "Shared").await.unwrap();
    assert_eq!(text, "s\nafter\n");
    assert!(bob.api.vault_keys(&vid).await.is_err(), "Bob is out");
    // A member who joins now reads the notes' whole history: older note keys
    // are wrapped under older vault keys, which the vault key chain unlocks.
    let dave = login(&srv, "dave").await;
    manage::share(&alice, &v, "dave", Role::Viewer, false)
        .await
        .unwrap();
    let dh = hub(&dave).await;
    assert_eq!(note(&dh, &vid, "A").await.as_deref(), Some("a\nafter\n"));
    assert_eq!(note(&dh, &vid, "B").await.as_deref(), Some("b\n"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_older_note_moves_to_its_own_key_at_its_next_snapshot() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    manage::create_vault(&alice, "Old").await.unwrap();
    let ah = hub(&alice).await;
    let v = ah.resolve("Old").await.unwrap();
    let vid = v.lock().await.id().to_string();
    // Written as by a client from before notes had their own keys.
    v.lock().await.outbox.note_keys = false;
    ops::put_document(&ah, "Old", "Log.md", "under the vault key\n", None)
        .await
        .unwrap();
    ah.sync_vault(&vid).await.unwrap();
    let oid = v.lock().await.find_note("Log").unwrap().1.id;
    assert_eq!(
        note_epoch(&alice.api.vault_keys(&vid).await.unwrap(), &oid),
        None
    );

    v.lock().await.outbox.note_keys = true;
    assert!(alice.snapshot_object(&vid, &v, &oid).await.unwrap());
    let k = alice.api.vault_keys(&vid).await.unwrap();
    assert_eq!(note_epoch(&k, &oid), Some(FIRST_DOC_EPOCH));
    assert_eq!(k.shared.as_deref(), Some(&[][..]), "still private");
    let ch = changes(&alice, &vid, &oid).await;
    assert!(
        ch.snapshots
            .iter()
            .any(|s| sym_epoch(&s.payload).unwrap() == FIRST_DOC_EPOCH)
    );

    // New edits go out under the note's key; members read everything.
    ops::put_document(
        &ah,
        "Old",
        "Log.md",
        "under the vault key\nthen its own\n",
        None,
    )
    .await
    .unwrap();
    ah.sync_vault(&vid).await.unwrap();
    let last = changes(&alice, &vid, &oid).await.updates.pop().unwrap();
    assert!(is_doc_epoch(sym_epoch(&last.payload).unwrap()));
    manage::share(&alice, &v, "bob", Role::Viewer, false)
        .await
        .unwrap();
    let bh = hub(&bob).await;
    assert_eq!(
        note(&bh, &vid, "Log").await.as_deref(),
        Some("under the vault key\nthen its own\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keys_someone_who_left_knows_are_renewed_by_the_owners_sync() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    let carol = login(&srv, "carol").await;
    manage::create_vault(&alice, "Team").await.unwrap();
    let ah = hub(&alice).await;
    let v = ah.resolve("Team").await.unwrap();
    let vid = v.lock().await.id().to_string();
    manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .unwrap();
    ops::put_document(&ah, "Team", "Shared.md", "s\n", None)
        .await
        .unwrap();
    ah.sync_vault(&vid).await.unwrap();
    note_shares::share_note(&alice, &v, "Shared", "carol", Role::Editor, false)
        .await
        .unwrap();
    let oid = v.lock().await.find_note("Shared").unwrap().1.id;
    let alice_live = live(ah.clone()).await;

    // Bob leaves on his own: the owner's running sync renews the vault's key.
    let bh = hub(&bob).await;
    let bv = bh.resolve("Team").await.unwrap();
    manage::leave_vault(&bob, &bv).await.unwrap();
    eventually(10, "the vault's key is renewed", || async {
        alice.api.vault_keys(&vid).await.unwrap().current_epoch == 2
    })
    .await;

    // Carol removes the note from her list: its key is renewed.
    let before = note_epoch(&alice.api.vault_keys(&vid).await.unwrap(), &oid).unwrap();
    carol
        .api
        .delete(&format!(
            "/vaults/{vid}/objects/{oid}/shares/{}",
            carol.user_id
        ))
        .await
        .unwrap();
    eventually(10, "the note's key is renewed", || async {
        note_epoch(&alice.api.vault_keys(&vid).await.unwrap(), &oid) == Some(before + 1)
    })
    .await;
    let info = alice.api.vault(&vid).await.unwrap();
    assert!(!info.rotation_pending && info.notes_rotation_pending.is_empty());
    alice_live.stop().await;
}
