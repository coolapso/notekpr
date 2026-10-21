//! Comments on notes from the command line (docs/comments.md).

mod common;

use common::*;
use notekpr_client::vault::CommentAct;
use notekpr_client::{manage, ops};
use notekpr_core::protocol::Role;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn viewers_comment_and_everyone_sees_the_threads() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let carol = login(&srv, "carol").await;
    manage::create_vault(&alice, "Family").await.unwrap();
    let ah = hub(&alice).await;
    ops::put_document(
        &ah,
        "Family",
        "Shopping.md",
        "Buy milk and bread\nmore bread\n",
        None,
    )
    .await
    .unwrap();
    let v = ah.resolve("Family").await.unwrap();
    manage::share(&alice, &v, "carol", Role::Viewer, false)
        .await
        .unwrap();

    // Carol only views the vault: she comments on some of its text…
    let ch = hub(&carol).await;
    let id = ops::add_comment(&ch, "Family", "Shopping", "milk", "Oat milk?")
        .await
        .unwrap()
        .id;
    // …on text that is there, once.
    for quote in ["eggs", "bread"] {
        assert!(
            ops::add_comment(&ch, "Family", "Shopping", quote, "?")
                .await
                .is_err(),
            "{quote}"
        );
    }
    // She still can't change the note.
    assert!(
        ops::put_document(&ch, "Family", "Shopping.md", "x\n", None)
            .await
            .is_err()
    );

    // Alice sees it, replies and resolves it; only Carol may change it.
    let fresh = hub(&alice).await;
    let list = ops::comments(&fresh, "Family", "Shopping").await.unwrap();
    assert_eq!(list.threads.len(), 1);
    let t = &list.threads[0];
    assert_eq!((t.quote.as_str(), t.on_text), ("milk", true));
    let c = &t.comments[0];
    assert_eq!(c.body, "Oat milk?");
    assert!(c.verified && !c.pending && c.author_name.is_some());
    let short = &id[..8];
    ops::comment_action(
        &fresh,
        "Family",
        "Shopping",
        short,
        CommentAct::Reply("Yes".into()),
    )
    .await
    .unwrap();
    assert!(
        ops::comment_action(
            &fresh,
            "Family",
            "Shopping",
            short,
            CommentAct::Edit("No".into())
        )
        .await
        .is_err()
    );
    ops::comment_action(&fresh, "Family", "Shopping", short, CommentAct::Resolve)
        .await
        .unwrap();

    // Carol again, from her copy on disk: the reply and the resolve.
    drop(ch);
    let again = hub(&carol).await;
    let list = ops::comments(&again, "Family", "Shopping").await.unwrap();
    let t = &list.threads[0];
    assert_eq!(
        t.comments
            .iter()
            .map(|c| c.body.as_str())
            .collect::<Vec<_>>(),
        ["Oat milk?", "Yes"]
    );
    assert_eq!(t.resolved_by.as_deref(), Some(alice.user_id.as_str()));
    // Her own comment she may change and delete.
    ops::comment_action(
        &again,
        "Family",
        "Shopping",
        short,
        CommentAct::Edit("Oat milk, please".into()),
    )
    .await
    .unwrap();
    let list = ops::comments(&again, "Family", "Shopping").await.unwrap();
    assert_eq!(list.threads[0].comments[0].body, "Oat milk, please");

    // The owner stops viewers from commenting.
    manage::set_viewers_comment(&alice, &v, false)
        .await
        .unwrap();
    let later = hub(&carol).await;
    let err = ops::add_comment(&later, "Family", "Shopping", "milk", "Hm")
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("doesn't let viewers comment"),
        "{err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_note_is_shared_with_its_comments_unless_told_otherwise() {
    use notekpr_client::note_shares;
    use notekpr_core::protocol::ChangesResponse;
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let rita = login(&srv, "rita").await;
    let sam = login(&srv, "sam").await;
    manage::create_vault(&alice, "Home").await.unwrap();
    let ah = hub(&alice).await;
    for (p, text) in [("Trip.md", "coast trip\n"), ("Diary.md", "dear diary\n")] {
        ops::put_document(&ah, "Home", p, text, None).await.unwrap();
    }
    ops::add_comment(&ah, "Home", "Trip", "coast", "Which one?")
        .await
        .unwrap();
    ops::add_comment(&ah, "Home", "Diary", "diary", "Private")
        .await
        .unwrap();
    let v = ah.resolve("Home").await.unwrap();
    let (vid, trip, diary) = {
        let g = v.lock().await;
        let id = |p: &str| g.find(p).unwrap().1.id;
        (g.id().to_string(), id("Trip"), id("Diary"))
    };
    let comments_for = |who: &std::sync::Arc<notekpr_client::session::Session>, note: &str| {
        let (who, path) = (
            who.clone(),
            format!("/vaults/{vid}/objects/{note}/changes?since=0"),
        );
        async move {
            let ch: ChangesResponse = who.api.get(&path).await.unwrap();
            ch.updates
                .iter()
                .filter(|u| u.object_id.starts_with("c_"))
                .count()
        }
    };

    // Shared with its comments (the default): Rita gets them, once.
    note_shares::share_note(&alice, &v, "Trip", "rita", Role::Viewer, false)
        .await
        .unwrap();
    assert_eq!(note_shares::carry_comments(&v, "Trip").await.unwrap(), 1);
    ah.local_changed(&vid, vec![], false).await;
    assert_eq!(comments_for(&rita, &trip).await, 1);
    assert_eq!(
        note_shares::carry_comments(&v, "Trip").await.unwrap(),
        0,
        "already there"
    );
    // Without: Sam sees only what is written from now on.
    note_shares::share_note(&alice, &v, "Diary", "sam", Role::Viewer, false)
        .await
        .unwrap();
    assert_eq!(comments_for(&sam, &diary).await, 0);
    ops::add_comment(&ah, "Home", "Diary", "dear", "Hi Sam")
        .await
        .unwrap();
    assert_eq!(comments_for(&sam, &diary).await, 1);
}
