//! Sharing single notes: owner side, recipient side, stale clients, revocation.

mod common;

use common::*;
use coolpsonotes_client::{manage, note_shares, ops};
use coolpsonotes_core::protocol::Role;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn single_note_sharing_end_to_end() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    manage::create_vault(&alice, "Private").await.unwrap();
    let ah = hub(&alice).await;
    ops::put_document(&ah, "Private", "Trip.md", "coast trip\n", None)
        .await
        .unwrap();
    ops::put_document(&ah, "Private", "Diary.md", "secret diary\n", None)
        .await
        .unwrap();
    // Another client of Alice's that loaded the vault before the note was shared.
    let stale = hub(&alice).await;
    let v = ah.resolve("Private").await.unwrap();
    let vid = v.lock().await.id().to_string();

    let out = note_shares::share_note(&alice, &v, "Trip", "bob", Role::Editor, false)
        .await
        .unwrap();
    assert_eq!(out.user.display_name.to_lowercase(), "bob");
    {
        let g = v.lock().await;
        let id = g.find_note("Trip").unwrap().1.id;
        assert!(g.replica.is_shared_object(&id));
    }

    // Bob sees only this note.
    let list = note_shares::shared_with_me(&bob).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "Trip");
    assert_eq!(list[0].info.role, Role::Editor);
    let (_, text) = note_shares::read_shared(&bob, "trip").await.unwrap();
    assert_eq!(text, "coast trip\n");
    assert!(bob.api.vaults().await.unwrap().is_empty());
    let shares = note_shares::note_shares(&alice, &vid, &list[0].info.object_id)
        .await
        .unwrap();
    assert_eq!(shares.len(), 1);

    // Bob edits; Alice receives it.
    note_shares::write_shared(&bob, "Trip", "bring sunscreen\n", true)
        .await
        .unwrap();
    ah.sync_vault(&vid).await.unwrap();
    assert_eq!(
        note(&ah, "Private", "Trip").await.as_deref(),
        Some("coast trip\nbring sunscreen\n")
    );

    // The stale client still encrypts with the vault key: the server answers
    // 412, the client re-keys and retries; nothing is lost.
    stale.sync_vault(&vid).await.unwrap();
    ops::put_document(
        &stale,
        "Private",
        "Trip.md",
        "coast trip\nbring sunscreen\nand hats\n",
        None,
    )
    .await
    .unwrap();
    stale.sync_vault(&vid).await.unwrap();
    let (_, text) = note_shares::read_shared(&bob, "Trip").await.unwrap();
    assert_eq!(text, "coast trip\nbring sunscreen\nand hats\n");

    // Unsharing cuts Bob off and rotates the note key; Alice keeps working.
    note_shares::unshare_note(&alice, &v, "Trip", "bob")
        .await
        .unwrap();
    assert!(note_shares::read_shared(&bob, "Trip").await.is_err());
    assert!(note_shares::shared_with_me(&bob).await.unwrap().is_empty());
    ah.sync_vault(&vid).await.unwrap();
    assert_eq!(
        note(&ah, "Private", "Trip").await.as_deref(),
        Some("coast trip\nbring sunscreen\nand hats\n")
    );
    ops::put_document(&ah, "Private", "Trip.md", "coast trip, just me\n", None)
        .await
        .unwrap();
    ah.sync_vault(&vid).await.unwrap();
    assert_eq!(
        note(&ah, "Private", "Trip").await.as_deref(),
        Some("coast trip, just me\n")
    );
    // A fresh client of Alice's reads the note with the rotated key.
    let fresh = hub(&alice).await;
    fresh.sync_vault(&vid).await.unwrap();
    assert_eq!(
        note(&fresh, "Private", "Trip").await.as_deref(),
        Some("coast trip, just me\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_needs_an_exact_name_not_a_prefix() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let _bobby = login(&srv, "bobby").await;
    manage::create_vault(&alice, "Plans").await.unwrap();
    let v = hub(&alice).await.resolve("Plans").await.unwrap();
    // Only "bobby" exists: "bob" must not quietly pick them.
    let err = manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .err()
        .expect("a prefix match is refused");
    assert!(format!("{err:#}").contains("did you mean"), "{err:#}");
    let _bob = login(&srv, "bob").await;
    let shared = manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .unwrap();
    assert!(shared.user.display_name.eq_ignore_ascii_case("bob"));
}
