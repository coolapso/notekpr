//! Live sync, offline reconciliation and crash-safe pending queues.

mod common;

use common::*;
use coolpsonotes_client::{manage, ops};
use coolpsonotes_core::engine::PinStatus;
use coolpsonotes_core::protocol::Role;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shared_vault_reaches_running_sync_live() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    manage::create_vault(&alice, "Family").await.unwrap();
    let alice_hub = hub(&alice).await;
    let fam = alice_hub.resolve("Family").await.unwrap();

    let shared = manage::share(&alice, &fam, "bob", Role::Editor, false)
        .await
        .unwrap();
    assert!(shared.key_shared);
    assert_eq!(shared.pin, Some(PinStatus::New));
    assert_eq!(
        shared.fingerprint.as_deref(),
        Some(bob.fingerprint().as_str())
    );

    // Bob's sync is running before Alice writes anything.
    let bob_live = live(hub(&bob).await).await;
    assert_eq!(bob_live.hub.vault_ids().len(), 1);

    ops::put_document(&alice_hub, "Family", "Welcome.md", "# Hi Bob\n", None)
        .await
        .unwrap();
    eventually(
        10,
        "bob receives alice's note over the websocket",
        || async {
            note(&bob_live.hub, "Family", "Welcome.md").await.as_deref() == Some("# Hi Bob\n")
        },
    )
    .await;

    // Bob's live edit flows back to Alice.
    ops::put_document(
        &bob_live.hub,
        "Family",
        "Welcome.md",
        "# Hi Bob\nHi Alice!\n",
        None,
    )
    .await
    .unwrap();
    let fam_id = fam.lock().await.id().to_string();
    alice_hub.sync_vault(&fam_id).await.unwrap();
    assert_eq!(
        note(&alice_hub, "Family", "Welcome").await.as_deref(),
        Some("# Hi Bob\nHi Alice!\n")
    );
    bob_live.stop().await;
}

/// A URL nothing listens on (connection refused).
async fn dead_url() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_edits_reconcile_and_pending_queue_survives_restart() {
    use coolpsonotes_client::profile::ProfileStore;
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    manage::create_vault(&alice, "Notes").await.unwrap();
    let alice_hub = hub(&alice).await;
    let v = alice_hub.resolve("Notes").await.unwrap();
    let vid = v.lock().await.id().to_string();
    manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .unwrap();
    ops::put_document(
        &alice_hub,
        &vid,
        "Plan.md",
        "line 1\nline 2\nline 3\n",
        None,
    )
    .await
    .unwrap();
    let bob_hub = hub(&bob).await;
    assert_eq!(
        note(&bob_hub, &vid, "Plan.md").await.as_deref(),
        Some("line 1\nline 2\nline 3\n")
    );
    drop(bob_hub);

    // Bob goes offline (server unreachable) and edits the first line.
    let bob_profile = ProfileStore::new(&srv.dirs("bob"), "bob").unwrap();
    let real = bob_profile.require().unwrap().server;
    let dead = dead_url().await;
    bob_profile.update(|p| p.server = dead.clone()).unwrap();
    let offline = hub(&reopen(&srv, "bob", "bob").await).await;
    assert!(!offline.is_online());
    let out = ops::put_document(
        &offline,
        &vid,
        "Plan.md",
        "line 1 (bob)\nline 2\nline 3\n",
        None,
    )
    .await
    .unwrap();
    assert!(
        out.pending_changes > 0,
        "edit must stay queued while offline"
    );
    drop(offline);

    // Meanwhile Alice edits the last line of the same note.
    ops::put_document(
        &alice_hub,
        &vid,
        "Plan.md",
        "line 1\nline 2\nline 3 (alice)\n",
        None,
    )
    .await
    .unwrap();

    // Bob's client restarts with the server back: the queue was persisted.
    bob_profile.update(|p| p.server = real.clone()).unwrap();
    let session = reopen(&srv, "bob", "bob").await;
    let pending = session
        .load_local(&vid)
        .unwrap()
        .unwrap()
        .replica
        .pending
        .clone();
    assert!(!pending.is_empty());
    // Simulate a crash right after the upload reached the server but before
    // the acknowledgement was recorded.
    let pushes = session
        .load_local(&vid)
        .unwrap()
        .unwrap()
        .replica
        .pending_pushes()
        .unwrap();
    // Signed, as every client sends them.
    let mut batch = vec![pushes];
    coolpsonotes_sync::members::sign_pushes(session.account(), &vid, &mut batch);
    let pushes = batch.pop().unwrap();
    let first = session.api.push(&vid, pushes).await.unwrap();
    assert!(first.rejected.is_empty());
    let head_after_first = first.head_seq;

    // Restart again and sync: the same client_update_ids are retried and the
    // server recognises them (same sequence numbers, head unchanged).
    let bob_hub = coolpsonotes_client::hub::Hub::start(
        session.clone(),
        coolpsonotes_client::hub::HubOptions {
            offline_ok: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        bob_hub
            .get(&vid)
            .unwrap()
            .lock()
            .await
            .replica
            .pending
            .len(),
        pending.len()
    );
    bob_hub.sync_vault(&vid).await.unwrap();
    assert!(
        bob_hub
            .get(&vid)
            .unwrap()
            .lock()
            .await
            .replica
            .pending
            .is_empty()
    );
    let changes = session.api.changes(&vid, 0, 1000, true).await.unwrap();
    assert_eq!(
        changes.head_seq, head_after_first,
        "retry must not create new log entries"
    );

    // Both edits survive, on both sides.
    let merged = "line 1 (bob)\nline 2\nline 3 (alice)\n";
    bob_hub.sync_vault(&vid).await.unwrap();
    assert_eq!(
        note(&bob_hub, &vid, "Plan.md").await.as_deref(),
        Some(merged)
    );
    alice_hub.sync_vault(&vid).await.unwrap();
    assert_eq!(
        note(&alice_hub, &vid, "Plan.md").await.as_deref(),
        Some(merged)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revocation_rotates_keys_and_keeps_local_copy_read_only() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    manage::create_vault(&alice, "Team").await.unwrap();
    let alice_hub = hub(&alice).await;
    let v = alice_hub.resolve("Team").await.unwrap();
    let vid = v.lock().await.id().to_string();
    manage::share(&alice, &v, "bob", Role::Viewer, false)
        .await
        .unwrap();
    ops::put_document(&alice_hub, &vid, "A.md", "before", None)
        .await
        .unwrap();
    let bob_live = live(hub(&bob).await).await;
    eventually(10, "bob sees A.md", || async {
        note(&bob_live.hub, &vid, "A.md").await.as_deref() == Some("before")
    })
    .await;
    // Viewers cannot write.
    assert!(matches!(
        ops::put_document(&bob_live.hub, &vid, "A.md", "x", None).await,
        Err(ops::OpError::Forbidden(_))
    ));

    let (_, epoch) = manage::unshare(&alice, &v, "bob").await.unwrap();
    assert_eq!(epoch, 2);
    eventually(10, "bob's copy becomes read-only", || async {
        bob_live
            .hub
            .get(&vid)
            .unwrap()
            .lock()
            .await
            .read_only
            .is_some()
    })
    .await;
    ops::put_document(&alice_hub, &vid, "A.md", "after", None)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    // The local copy stays readable but never receives post-revocation content.
    assert_eq!(
        note(&bob_live.hub, &vid, "A.md").await.as_deref(),
        Some("before")
    );
    bob_live.stop().await;
    // Alice's own replica uses the new epoch for writes.
    assert_eq!(v.lock().await.replica.keyring.current_epoch(), Some(2));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn editing_sessions_leave_revisions() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Journal").await.unwrap();
    let h = hub(&alice).await;
    let mut text = String::new();
    for i in 0..6 {
        text.push_str(&format!("entry {i}\n"));
        ops::put_document(&h, "Journal", "Today.md", &text, None)
            .await
            .unwrap();
    }
    assert_eq!(h.snapshot_idle(std::time::Duration::ZERO, 5).await, 1);
    ops::put_document(&h, "Journal", "Today.md", "rewritten\n", None)
        .await
        .unwrap();
    let revs = ops::history(&h, "Journal", "Today.md").await.unwrap();
    assert_eq!(revs.len(), 1);
    let old = ops::revision(&h, "Journal", "Today.md", &revs[0].id)
        .await
        .unwrap();
    assert_eq!(old, text);
    // Too few edits since the last revision: no new one.
    assert_eq!(h.snapshot_idle(std::time::Duration::ZERO, 5).await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_uploads_resume_after_a_reconnect_with_a_read_only_vault() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    manage::create_vault(&alice, "Was shared").await.unwrap();
    let alice_hub = hub(&alice).await;
    let v = alice_hub.resolve("Was shared").await.unwrap();
    manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .unwrap();
    manage::create_vault(&bob, "Own").await.unwrap();
    drop(hub(&bob).await); // bob gets a local copy of both
    manage::unshare(&alice, &v, "bob").await.unwrap();

    // Bob's client restarts: the unshared vault is a read-only local copy.
    let bob_hub = hub(&reopen(&srv, "bob", "bob").await).await;
    let ro = bob_hub.resolve("Was shared").await.unwrap();
    assert!(ro.lock().await.read_only.is_some());
    // As after a reconnect: uploads wait until every subscribed vault is
    // caught up. The read-only vault is never subscribed, so it must not be
    // waited for.
    bob_hub.set_push_now(false);
    let bob_live = live(bob_hub).await;
    let own = bob_live.hub.resolve("Own").await.unwrap();
    let own_id = own.lock().await.id().to_string();
    ops::put_document(&bob_live.hub, &own_id, "Today.md", "live edit\n", None)
        .await
        .unwrap();
    eventually(10, "bob's edit is uploaded live", || async {
        own.lock().await.replica.pending.is_empty()
    })
    .await;
    bob_live.stop().await;
    let fresh = hub(&reopen(&srv, "bob", "bob").await).await;
    assert_eq!(
        note(&fresh, &own_id, "Today.md").await.as_deref(),
        Some("live edit\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_error_from_a_proxy_counts_as_offline() {
    use coolpsonotes_client::profile::ProfileStore;
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Notes").await.unwrap();
    let h = hub(&alice).await;
    ops::put_document(&h, "Notes", "A.md", "a\n", None)
        .await
        .unwrap();
    drop(h);
    // The reverse proxy answers 502 while the server restarts.
    let down = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let d = down.clone();
    let url = proxy(
        &srv.url,
        std::sync::Arc::new(move |_, _| {
            d.load(std::sync::atomic::Ordering::SeqCst)
                .then_some(Fault::Reply(502))
        }),
    )
    .await;
    let store = ProfileStore::new(&srv.dirs("alice"), "alice").unwrap();
    store.update(|p| p.server = url.clone()).unwrap();
    let offline = hub(&reopen(&srv, "alice", "alice").await).await;
    assert!(!offline.is_online());
    assert_eq!(
        note(&offline, "Notes", "A.md").await.as_deref(),
        Some("a\n")
    );
    let out = ops::put_document(&offline, "Notes", "A.md", "a\nb\n", None)
        .await
        .unwrap();
    assert!(out.pending_changes > 0, "kept for later, not an error");
    let vid = offline.vault_ids()[0].clone();
    let o = offline.push(&vid).await.unwrap();
    assert!(o.offline && o.remaining > 0);
    drop(offline);
    // Back up: the queued edit is uploaded.
    down.store(false, std::sync::atomic::Ordering::SeqCst);
    let back = hub(&reopen(&srv, "alice", "alice").await).await;
    assert!(back.is_online());
    assert!(
        back.get(&vid)
            .unwrap()
            .lock()
            .await
            .replica
            .pending
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn oversized_changes_are_split_or_set_aside_and_never_block_the_rest() {
    use coolpsonotes_core::doc::NoteDoc;
    let srv = start_with(|c| {
        c.max_update_kb = 64;
        c.max_snapshot_kb = Some(128);
    })
    .await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    manage::create_vault(&alice, "Big").await.unwrap();
    let h = hub(&alice).await;
    let v = h.resolve("Big").await.unwrap();
    let vid = v.lock().await.id().to_string();
    manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .unwrap();

    // 1. One ~250 KB paste becomes several updates the server accepts.
    let big: String = (0..6000)
        .map(|i| format!("line {i}: some text to make the note large\n"))
        .collect();
    assert!(big.len() > 200 * 1024);
    let out = ops::put_document(&h, &vid, "Big.md", &big, None)
        .await
        .unwrap();
    assert_eq!(out.pending_changes, 0, "everything uploaded");
    let bob_hub = hub(&bob).await;
    assert_eq!(
        note(&bob_hub, &vid, "Big.md").await.as_deref(),
        Some(big.as_str())
    );

    // 2. A change that cannot be split (one huge CRDT update), in a note
    // larger than even a snapshot may be, is kept aside; the changes after
    // it are uploaded anyway. (Smaller ones go out in a snapshot instead:
    // `a_change_too_large_for_one_update_reaches_others_in_a_snapshot`.)
    {
        let mut g = v.lock().await;
        let w = g.create_note("Raw.md", "").unwrap();
        let u = NoteDoc::new().set_text(&"x".repeat(200_000)).unwrap();
        g.replica
            .notes
            .get(&w.id)
            .unwrap()
            .apply_update(&u)
            .unwrap();
        g.record(&w.id, u).unwrap();
        g.save().unwrap();
    }
    ops::put_document(&h, &vid, "Other.md", "fine\n", None)
        .await
        .unwrap();
    {
        let g = v.lock().await;
        assert!(g.replica.pending.is_empty(), "nothing is blocked");
        assert_eq!(g.outbox.held_count(), 1);
    }
    bob_hub.sync_vault(&vid).await.unwrap();
    assert_eq!(
        note(&bob_hub, &vid, "Other.md").await.as_deref(),
        Some("fine\n")
    );
    assert!(note(&bob_hub, &vid, "Raw.md").await.is_some());
    // It is kept across restarts (and reported).
    drop(h);
    let again = hub(&reopen(&srv, "alice", "alice").await).await;
    let s = again.summaries().await;
    assert_eq!(s[0].too_large_changes, 1);
    assert_eq!(
        note(&again, &vid, "Raw.md").await.map(|t| t.len()),
        Some(200_000)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_change_too_large_for_one_update_reaches_others_in_a_snapshot() {
    use coolpsonotes_core::doc::NoteDoc;
    let srv = start_with(|c| c.max_update_kb = 8).await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    manage::create_vault(&alice, "Big").await.unwrap();
    let ha = hub(&alice).await;
    let v = ha.resolve("Big").await.unwrap();
    let vid = v.lock().await.id().to_string();
    manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .unwrap();
    ops::put_document(&ha, &vid, "Notes.md", "start\n", None)
        .await
        .unwrap();
    ha.push(&vid).await.unwrap();
    let hb = hub(&bob).await;
    assert_eq!(
        note(&hb, &vid, "Notes.md").await.as_deref(),
        Some("start\n")
    );

    // A change made elsewhere (as from a copy in the file mirror) arrives
    // as one update far larger than the server takes.
    let big: String = (0..4000).map(|i| format!("line {i}\n")).collect();
    {
        let mut v = v.lock().await;
        let id = v.find("Notes.md").unwrap().1.id;
        let fork = NoteDoc::from_updates([v.replica.notes[&id].encode_state().as_slice()]).unwrap();
        let update = fork.set_text(&big).unwrap();
        assert!(update.len() > 8 * 1024);
        v.merge_note_update(&id, update).unwrap();
    }
    let out = ha.push(&vid).await.unwrap();
    assert_eq!((out.quarantined, out.carried), (1, 1));
    assert_eq!(v.lock().await.outbox.held_count(), 0, "it went out");
    // A small change that builds on it follows as usual.
    let mut edited = big.clone();
    edited.push_str("and one more\n");
    ops::put_document(&ha, &vid, "Notes.md", &edited, None)
        .await
        .unwrap();
    ha.push(&vid).await.unwrap();

    // Bob, already up to date before it, catches up and has both.
    let vb = hb.resolve("Big").await.unwrap();
    hb.session.catch_up(&vb).await.unwrap();
    assert_eq!(
        note(&hb, &vid, "Notes.md").await.as_deref(),
        Some(edited.as_str())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_shares_a_new_vault_with_several_people_in_a_row() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let others = ["bob", "carol", "dave", "erin"];
    for o in others {
        login(&srv, o).await;
    }
    // Alice's live connection works on the new vault (opening it, signing
    // her log) while she shares it: each share still gets in.
    let live = live(hub(&alice).await).await;
    for round in 0..3 {
        let name = format!("Team {round}");
        let v = manage::create_vault(&alice, &name).await.unwrap();
        let list = alice.api.vaults().await.unwrap();
        live.hub.open_listed(&list).await.unwrap();
        let shared = live.hub.get(v.id()).unwrap();
        for o in others {
            manage::share(&alice, &shared, o, Role::Editor, false)
                .await
                .unwrap_or_else(|e| panic!("sharing {name} with {o}: {e:#}"));
        }
        let members = manage::members(&alice, &shared).await.unwrap();
        assert_eq!(members.len(), others.len() + 1, "{name}");
    }
    live.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opening_a_new_vault_twice_at_once_keeps_one_copy() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    let bh = hub(&bob).await;
    for round in 0..5 {
        let name = format!("Team {round}");
        let v = manage::create_vault(&alice, &name).await.unwrap();
        let ah = hub(&alice).await;
        let shared = ah.get(v.id()).unwrap();
        manage::share(&alice, &shared, "bob", Role::Editor, false)
            .await
            .unwrap();
        // Two tasks of Bob's (say his live connection and a command) see
        // the new vault at the same moment.
        let list = bob.api.vaults().await.unwrap();
        let (a, b) = tokio::join!(bh.open_listed(&list), bh.open_listed(&list));
        let opened = a.unwrap().len() + b.unwrap().len();
        assert_eq!(opened, 1, "{name} was opened {opened} times");
        // What Bob writes into the open copy stays in it.
        let before = bh.get(v.id()).unwrap();
        ops::put_document(&bh, &name, "Bob.md", "mine\n", None)
            .await
            .unwrap();
        let (c, d) = tokio::join!(bh.open_listed(&list), bh.open_listed(&list));
        assert_eq!(c.unwrap().len() + d.unwrap().len(), 0);
        assert!(std::sync::Arc::ptr_eq(&before, &bh.get(v.id()).unwrap()));
    }
}
