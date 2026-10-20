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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_note_key_is_never_renewed_for_a_changed_account_key() {
    use coolpsonotes_client::profile::ProfileStore;
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let _bob = login(&srv, "bob").await;
    let carol = login(&srv, "carol").await;
    manage::create_vault(&alice, "Private").await.unwrap();
    let ah = hub(&alice).await;
    ops::put_document(&ah, "Private", "Trip.md", "coast\n", None)
        .await
        .unwrap();
    let v = ah.resolve("Private").await.unwrap();
    let vid = v.lock().await.id().to_string();
    for who in ["bob", "carol"] {
        note_shares::share_note(&alice, &v, "Trip", who, Role::Editor, false)
            .await
            .unwrap();
    }
    // Carol's key now differs from the pinned one (as a server substituting
    // it would make it look): renewing the key leaves her out of it.
    let pins = ProfileStore::new(&srv.dirs("alice"), "alice").unwrap();
    pins.update(|p| p.pins.insert(carol.user_id.clone(), "0000".into()))
        .unwrap();
    note_shares::unshare_note(&alice, &v, "Trip", "bob")
        .await
        .unwrap();
    let note_id = v.lock().await.find_note("Trip").unwrap().1.id;
    let carol_has_key = || async {
        note_shares::note_shares(&alice, &vid, &note_id)
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.user.id == carol.user_id)
            .unwrap()
            .has_current_key
    };
    assert!(!carol_has_key().await, "left out of the new key");
    // Once verified, sharing again gives it back.
    note_shares::share_note(&alice, &v, "Trip", "carol", Role::Editor, true)
        .await
        .unwrap();
    assert!(carol_has_key().await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vault_key_rotation_leaves_out_a_member_whose_key_changed() {
    use coolpsonotes_client::profile::ProfileStore;
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let _bob = login(&srv, "bob").await;
    let carol = login(&srv, "carol").await;
    manage::create_vault(&alice, "Home").await.unwrap();
    let v = hub(&alice).await.resolve("Home").await.unwrap();
    let vid = v.lock().await.id().to_string();
    for who in ["bob", "carol"] {
        manage::share(&alice, &v, who, Role::Editor, false)
            .await
            .unwrap();
    }
    ProfileStore::new(&srv.dirs("alice"), "alice")
        .unwrap()
        .update(|p| p.pins.insert(carol.user_id.clone(), "0000".into()))
        .unwrap();
    // The key is renewed for everyone else, instead of not at all.
    alice.rotate_keys(&v).await.unwrap();
    let has_key = |members: &[coolpsonotes_core::protocol::MemberInfo], id: &str| {
        members
            .iter()
            .find(|m| m.user.id == id)
            .unwrap()
            .has_current_key
    };
    let members = alice.api.members(&vid).await.unwrap();
    assert!(has_key(&members, &alice.user_id) && !has_key(&members, &carol.user_id));
    // Once verified, sharing again gives it back.
    manage::share(&alice, &v, "carol", Role::Editor, true)
        .await
        .unwrap();
    let members = alice.api.members(&vid).await.unwrap();
    assert!(has_key(&members, &carol.user_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn icons_and_new_names_reach_the_people_a_note_is_shared_with() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    manage::create_vault(&alice, "Home").await.unwrap();
    let ah = hub(&alice).await;
    ops::put_document(&ah, "Home", "Garden/Tomatoes.md", "water daily\n", None)
        .await
        .unwrap();
    let v = ah.resolve("Home").await.unwrap();
    note_shares::share_note(&alice, &v, "Garden/Tomatoes", "bob", Role::Viewer, false)
        .await
        .unwrap();

    // A note's icon, and a folder that only existed as the note's parent.
    let (_, id) = ops::set_icon(&ah, "Home", "Garden/Tomatoes.md", Some("🍅"))
        .await
        .unwrap();
    note_shares::refresh_meta(&alice, &v, &id).await.unwrap();
    let (_, folder) = ops::set_icon(&ah, "Home", "Garden", Some("🌱"))
        .await
        .unwrap();
    {
        let g = v.lock().await;
        assert_eq!(
            g.replica.index.get(&id).unwrap().icon.as_deref(),
            Some("🍅")
        );
        let f = g.replica.index.get(&folder).unwrap();
        assert_eq!((f.path.as_str(), f.icon.as_deref()), ("Garden", Some("🌱")));
    }
    let shared = note_shares::shared_with_me(&bob).await.unwrap();
    assert_eq!(
        (shared[0].title.as_str(), shared[0].icon.as_deref()),
        ("Tomatoes", Some("🍅"))
    );

    // Renamed: Bob sees the new title with the icon.
    let out = ops::move_document(
        &ah,
        "Home",
        "Garden/Tomatoes.md",
        "Garden/Cherry tomatoes.md",
        true,
    )
    .await
    .unwrap();
    note_shares::refresh_meta(&alice, &v, out.id.as_deref().unwrap())
        .await
        .unwrap();
    let shared = note_shares::shared_with_me(&bob).await.unwrap();
    assert_eq!(
        (shared[0].title.as_str(), shared[0].icon.as_deref()),
        ("Cherry tomatoes", Some("🍅"))
    );

    // Icons are checked (one short emoji or text), and removable.
    assert!(
        ops::set_icon(&ah, "Home", "Garden", Some("a very long icon text"))
            .await
            .is_err()
    );
    assert!(
        ops::set_icon(&ah, "Home", "Nowhere", Some("🌱"))
            .await
            .is_err()
    );
    ops::set_icon(&ah, "Home", "Garden", None).await.unwrap();
    assert_eq!(
        v.lock().await.replica.index.get(&folder).unwrap().icon,
        None
    );

    // The vault's icon, which other members see.
    manage::set_vault_icon(&alice, &v, Some("🏡"))
        .await
        .unwrap();
    assert_eq!(v.lock().await.replica.meta.icon.as_deref(), Some("🏡"));
    let fresh = hub(&alice).await;
    let fv = fresh.resolve("Home").await.unwrap();
    assert_eq!(fv.lock().await.replica.meta.icon.as_deref(), Some("🏡"));
    manage::set_vault_icon(&alice, &v, None).await.unwrap();
    assert_eq!(v.lock().await.replica.meta.icon, None);
}
