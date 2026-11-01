//! Comments on notes from the command line (docs/comments.md).

mod common;

use common::*;
use notekpr_cli::vault::CommentAct;
use notekpr_cli::{manage, ops};
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
    use notekpr_cli::note_shares;
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
    let comments_for = |who: &std::sync::Arc<notekpr_cli::session::Session>, note: &str| {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn members_are_mentioned_and_read_everywhere() {
    use notekpr_cli::mentions;
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    let carol = login(&srv, "carol").await;
    let _dave = login(&srv, "dave").await;
    manage::create_vault(&alice, "Family").await.unwrap();
    let ah = hub(&alice).await;
    ops::put_document(&ah, "Family", "Plan.md", "Paint the fence\n", None)
        .await
        .unwrap();
    let v = ah.resolve("Family").await.unwrap();
    manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .unwrap();
    manage::share(&alice, &v, "carol", Role::Viewer, false)
        .await
        .unwrap();

    // Only the vault's members can be mentioned: Dave isn't one.
    let sent = ops::add_comment(
        &ah,
        "Family",
        "Plan",
        "fence",
        "@Bob and @dave, which color?",
    )
    .await
    .unwrap();
    assert_eq!(sent.mentioned.mentions, std::slice::from_ref(&bob.user_id));
    assert_eq!(sent.mentioned.not_mentioned, ["dave"]);
    let list = ops::comments(&ah, "Family", "Plan").await.unwrap();
    let c = &list.threads[0].comments[0];
    assert_eq!(c.mentions.len(), 1);
    assert_eq!(c.mentions[0].name.as_deref(), Some("Bob"));

    // Bob finds it among his mentions, new; Carol has none.
    let bh = hub(&bob).await;
    let (got, known) = mentions::list(&bh, None).await.unwrap();
    assert!(known);
    assert_eq!(got.len(), 1);
    let m = &got[0];
    assert_eq!((m.path.as_str(), m.quote.as_str()), ("Plan.md", "fence"));
    assert_eq!(m.author_name.as_deref(), Some("Alice"));
    assert!(m.unread && m.comment == sent.id);
    let ch = hub(&carol).await;
    assert!(mentions::list(&ch, None).await.unwrap().0.is_empty());

    // Read on one device: read everywhere (the synced settings).
    mentions::mark_read(&bob, &got).await.unwrap();
    let again = hub(&bob).await;
    assert!(!mentions::list(&again, None).await.unwrap().0[0].unread);

    // A reply that mentions Carol (a viewer: still a member) reaches her;
    // Bob's own reply mentioning himself is nobody's news.
    let short = &sent.id[..8];
    ops::comment_action(
        &bh,
        "Family",
        "Plan",
        short,
        CommentAct::Reply("Ask @Carol".into()),
    )
    .await
    .unwrap();
    let ch = hub(&carol).await;
    let got = mentions::list(&ch, Some("Family")).await.unwrap().0;
    assert_eq!(got.len(), 1);
    assert!(got[0].unread && got[0].body == "Ask @Carol");
    // Edited without her: not a mention anymore.
    ops::comment_action(
        &bh,
        "Family",
        "Plan",
        &got[0].comment[..8],
        CommentAct::Edit("Ask someone".into()),
    )
    .await
    .unwrap();
    let ch = hub(&carol).await;
    assert!(mentions::list(&ch, None).await.unwrap().0.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn people_a_note_is_shared_with_comment_from_the_cli() {
    use notekpr_cli::mentions;
    use notekpr_cli::note_shares::{self, SharedCommentAct};
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    let _carol = login(&srv, "carol").await;
    let rita = login(&srv, "rita").await;
    manage::create_vault(&alice, "Home").await.unwrap();
    let ah = hub(&alice).await;
    ops::put_document(&ah, "Home", "Trip.md", "A coast trip in May\n", None)
        .await
        .unwrap();
    let v = ah.resolve("Home").await.unwrap();
    manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .unwrap();
    manage::share(&alice, &v, "carol", Role::Editor, false)
        .await
        .unwrap();
    let bh = hub(&bob).await;
    ops::add_comment(&bh, "Home", "Trip", "coast", "Which one?")
        .await
        .unwrap();
    // Rita isn't a member: nobody can mention her.
    let ah = hub(&alice).await;
    let v = ah.resolve("Home").await.unwrap();
    let sent = ops::add_comment(&ah, "Home", "Trip", "May", "@rita, May?")
        .await
        .unwrap();
    assert!(sent.mentioned.mentions.is_empty());

    note_shares::share_note(&alice, &v, "Trip", "rita", Role::Viewer, false)
        .await
        .unwrap();
    note_shares::carry_comments(&v, "Trip").await.unwrap();
    let vid = v.lock().await.id().to_string();
    ah.local_changed(&vid, vec![], false).await;

    // Rita reads the threads, their authors verified.
    let list = note_shares::shared_comments(&rita, "Trip").await.unwrap();
    assert_eq!(list.threads.len(), 2);
    assert!(list.threads.iter().all(|t| t.comments[0].verified));
    assert_eq!(
        list.threads[0].comments[0].author_name.as_deref(),
        Some("Bob")
    );

    // She comments, mentioning the owner and Bob (who commented) — not
    // Carol, a member she can't know of.
    let sent = note_shares::shared_comment(
        &rita,
        "Trip",
        SharedCommentAct::Add {
            on: "trip".into(),
            text: "@Alice @bob @carol: by train?".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        sent.mentioned.mentions,
        [alice.user_id.clone(), bob.user_id.clone()]
    );
    assert_eq!(sent.mentioned.not_mentioned, ["carol"]);
    let bh = hub(&bob).await;
    let got = mentions::list(&bh, None).await.unwrap().0;
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].author_name.as_deref(), Some("Rita"));
    // The members see her comment, from someone the note is shared with.
    let list = ops::comments(&bh, "Home", "Trip").await.unwrap();
    assert!(
        list.threads
            .iter()
            .any(|t| t.comments[0].body == "@Alice @bob @carol: by train?")
    );

    // Reply, edit her own, resolve; never change someone else's.
    let id = sent.id[..8].to_string();
    let on = |id: &str, act| SharedCommentAct::On {
        id: id.to_string(),
        act,
    };
    note_shares::shared_comment(
        &rita,
        "Trip",
        on(&id, CommentAct::Reply("Or by bus".into())),
    )
    .await
    .unwrap();
    note_shares::shared_comment(&rita, "Trip", on(&id, CommentAct::Edit("By train?".into())))
        .await
        .unwrap();
    let bob_thread = list
        .threads
        .iter()
        .find(|t| t.comments[0].body == "Which one?")
        .unwrap()
        .id
        .clone();
    let err = note_shares::shared_comment(
        &rita,
        "Trip",
        on(&bob_thread[..8], CommentAct::Edit("Mine now".into())),
    )
    .await
    .unwrap_err();
    assert!(format!("{err}").contains("only its author"), "{err}");
    note_shares::shared_comment(&rita, "Trip", on(&id, CommentAct::Resolve))
        .await
        .unwrap();
    let list = note_shares::shared_comments(&rita, "Trip").await.unwrap();
    let t = list
        .threads
        .iter()
        .find(|t| t.comments[0].body == "By train?")
        .unwrap();
    assert_eq!(t.comments.len(), 2);
    assert_eq!(t.resolved_by.as_deref(), Some(rita.user_id.as_str()));

    // The owner stops viewers from commenting: Rita can't anymore.
    manage::set_viewers_comment(&alice, &v, false)
        .await
        .unwrap();
    let err = note_shares::shared_comment(
        &rita,
        "Trip",
        SharedCommentAct::Add {
            on: "May".into(),
            text: "?".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(format!("{err}").contains("can't comment"), "{err}");
}
