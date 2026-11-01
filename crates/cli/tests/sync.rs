//! Live sync, offline reconciliation and crash-safe pending queues.

mod common;

use common::*;
use notekpr_cli::{manage, ops};
use notekpr_core::engine::PinStatus;
use notekpr_core::protocol::Role;

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
    // Family, and his own quick notes.
    assert_eq!(bob_live.hub.vault_ids().len(), 2);

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn owners_share_a_vault_and_it_outlives_its_creator() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    let carol = login(&srv, "carol").await;
    manage::create_vault(&alice, "Family").await.unwrap();
    let alice_hub = hub(&alice).await;
    let fam = alice_hub.resolve("Family").await.unwrap();
    ops::put_document(&alice_hub, "Family", "Plans.md", "# Plans\n", None)
        .await
        .unwrap();

    // Alice, the only owner, can't just go.
    let err = manage::leave_vault(&alice, &fam).await.unwrap_err();
    assert!(err.to_string().contains("only owner"), "{err}");

    // Bob joins as an owner (an editor first, then owner), and as one
    // shares the vault with Carol.
    manage::share(&alice, &fam, "bob", Role::Owner, false)
        .await
        .unwrap();
    let bob_hub = hub(&bob).await;
    let bfam = bob_hub.resolve("Family").await.unwrap();
    assert_eq!(bfam.lock().await.role(), Role::Owner);
    manage::share(&bob, &bfam, "carol", Role::Viewer, false)
        .await
        .unwrap();

    // Alice leaves; the vault stays, Bob's, and still checks out for
    // everyone — its log starts with Alice, who is gone.
    manage::leave_vault(&alice, &fam).await.unwrap();
    bob_hub.session.reload_member_log(&bfam).await.unwrap();
    let owners: Vec<String> = bfam
        .lock()
        .await
        .log
        .state
        .as_ref()
        .unwrap()
        .owners()
        .map(|m| m.user_id.clone())
        .collect();
    assert_eq!(owners, vec![bob.user_id.clone()]);
    let carol_hub = hub(&carol).await;
    assert_eq!(
        note(&carol_hub, "Family", "Plans.md").await.as_deref(),
        Some("# Plans\n")
    );
    // And Bob, its last owner, can't just go either.
    assert!(manage::leave_vault(&bob, &bfam).await.is_err());
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
    use notekpr_cli::profile::ProfileStore;
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
    notekpr_sync::members::sign_pushes(session.account(), &vid, &mut batch);
    let pushes = batch.pop().unwrap();
    let first = session.api.push(&vid, pushes).await.unwrap();
    assert!(first.rejected.is_empty());
    let head_after_first = first.head_seq;

    // Restart again and sync: the same client_update_ids are retried and the
    // server recognises them (same sequence numbers, head unchanged).
    let bob_hub = notekpr_cli::hub::Hub::start(
        session.clone(),
        notekpr_cli::hub::HubOptions {
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
    use notekpr_cli::profile::ProfileStore;
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
    let vid = offline
        .resolve("Notes")
        .await
        .unwrap()
        .lock()
        .await
        .id()
        .to_string();
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
    use notekpr_core::doc::NoteDoc;
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
    use notekpr_core::doc::NoteDoc;
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
async fn writing_into_a_note_before_it_arrived_still_merges() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    manage::create_vault(&alice, "Team").await.unwrap();
    let ah = hub(&alice).await;
    let v = ah.resolve("Team").await.unwrap();
    manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .unwrap();
    let bh = hub(&bob).await;
    let vid = v.lock().await.id().to_string();
    // Alice starts a page; Bob writes into it before his copy has it.
    let page = ops::put_document(&ah, "Team", "Page 2.md", "# Page 2\n\n## Bob\n", None)
        .await
        .unwrap();
    let bv = bh.get(&vid).unwrap();
    {
        let mut g = bv.lock().await;
        assert!(g.set_note_text(&page.id, "hello from bob").unwrap());
        g.save().unwrap();
    }
    bh.local_changed(&vid, vec![page.id.clone()], false).await;
    for _ in 0..2 {
        bh.sync_vault(&vid).await.unwrap();
        ah.sync_vault(&vid).await.unwrap();
    }
    let a = v.lock().await.note_text(&page.id);
    let b = bv.lock().await.note_text(&page.id);
    assert_eq!(a, b);
    assert!(
        a.contains("## Bob") && a.contains("hello from bob"),
        "{a:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rename_from_an_older_copy_keeps_what_others_typed_meanwhile() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    let carol = login(&srv, "carol").await;
    manage::create_vault(&alice, "Team").await.unwrap();
    let ah = hub(&alice).await;
    let v = ah.resolve("Team").await.unwrap();
    for p in ["bob", "carol"] {
        manage::share(&alice, &v, p, Role::Editor, false)
            .await
            .unwrap();
    }
    let n1 = "# Goals 1\n\nSee also [[Goals 2]].\n\n## Bob\n\n## Carol\n";
    let n3 = "# Goals 3\n\nSee also [[Goals 1]], [[Plan]].\n\n## Bob\n\n## Carol\n";
    ops::put_document(&ah, "Team", "Goals 1.md", n1, None)
        .await
        .unwrap();
    ops::put_document(&ah, "Team", "Goals 3.md", n3, None)
        .await
        .unwrap();
    let vid = v.lock().await.id().to_string();
    let bh = hub(&bob).await;
    let ch = hub(&carol).await;
    // Bob types a sentence into Goals 3, a few characters at a time.
    let id3 = bh
        .get(&vid)
        .unwrap()
        .lock()
        .await
        .find("Goals 3")
        .unwrap()
        .1
        .id;
    let sentence = "The bikes need new chains before the tour.";
    let bv = bh.get(&vid).unwrap();
    for (i, chunk) in sentence.as_bytes().chunks(3).enumerate() {
        let chunk = std::str::from_utf8(chunk).unwrap();
        {
            let mut g = bv.lock().await;
            let t = g.note_text(&id3);
            let at = t.find("## Bob\n").unwrap() + "## Bob\n".len();
            let mut end = t[at..].find("\n## ").map(|e| at + e).unwrap();
            while end > at && t.as_bytes()[end - 1] == b'\n' {
                end -= 1;
            }
            let mut new = t.clone();
            new.insert_str(end, chunk);
            if i == 0 {
                new.insert(end + chunk.len(), '\n');
            }
            g.set_note_text(&id3, &new).unwrap();
        }
        bh.local_changed(&vid, vec![id3.clone()], false).await;
        // Halfway, Carol (who hasn't seen Bob's typing) renames Goals 1.
        if i == 5 {
            ops::move_document(&ch, "Team", "Goals 1.md", "Map 957.md", true)
                .await
                .unwrap();
        }
    }
    for _ in 0..3 {
        for h in [&ah, &bh, &ch] {
            h.sync_vault(&vid).await.unwrap();
        }
    }
    for (who, h) in [("alice", &ah), ("bob", &bh), ("carol", &ch)] {
        let t = h.get(&vid).unwrap().lock().await.note_text(&id3);
        assert!(t.contains(sentence), "{who} lost it: {t:?}");
        assert!(t.contains("[[Map 957]]"), "{who}: {t:?}");
    }
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn typing_into_a_note_whose_key_has_not_arrived_keeps_the_text() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    manage::create_vault(&alice, "Team").await.unwrap();
    let ah = hub(&alice).await;
    let v = ah.resolve("Team").await.unwrap();
    manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .unwrap();
    let bh = hub(&bob).await;
    let vid = v.lock().await.id().to_string();
    let page = ops::put_document(
        &ah,
        "Team",
        "Goals 3.md",
        "# Goals 3\n\n## Alice\n\n## Bob\n",
        None,
    )
    .await
    .unwrap();
    // Bob's copy gets the changes, but not the new note's key yet.
    let bv = bh.get(&vid).unwrap();
    let since = bv.lock().await.replica.seq;
    let ch = bob.api.changes(&vid, since, 100, false).await.unwrap();
    {
        let mut g = bv.lock().await;
        g.apply_changes(&ch);
        // Listed, but its text waits for its key.
        assert!(g.entry(&page.id).is_some());
        assert_eq!(g.note_text(&page.id), "");
        assert!(!g.replica.doc_keys.contains_key(&page.id));
        assert!(g.set_note_text(&page.id, "Moved the deadline.").unwrap());
        g.save().unwrap();
    }
    bh.local_changed(&vid, vec![page.id.clone()], false).await;
    for _ in 0..3 {
        let _ = bh.sync_vault(&vid).await;
        let _ = ah.sync_vault(&vid).await;
    }
    // Both keep both texts (Bob's at the start: he typed into what looked
    // empty to him).
    let a = v.lock().await.note_text(&page.id);
    let b = bv.lock().await.note_text(&page.id);
    assert_eq!(a, b);
    assert!(
        a.contains("Moved the deadline.") && a.contains("# Goals 3"),
        "{a:?}"
    );
    assert_eq!(bv.lock().await.replica.pending.len(), 0);
}

/// Bob writes in Home's Soup without uploading (offline, say); Alice
/// deletes it for good meanwhile. Returns their hubs and the vault id.
async fn deleted_while_unsent(
    srv: &TestServer,
) -> (
    std::sync::Arc<notekpr_cli::hub::Hub>,
    std::sync::Arc<notekpr_cli::hub::Hub>,
    String,
) {
    let alice = login(srv, "alice").await;
    let bob = login(srv, "bob").await;
    manage::create_vault(&alice, "Home").await.unwrap();
    let ah = hub(&alice).await;
    ops::put_document(&ah, "Home", "Soup.md", "leeks\n", None)
        .await
        .unwrap();
    let av = ah.resolve("Home").await.unwrap();
    let vid = av.lock().await.id().to_string();
    manage::share(&alice, &av, "bob", Role::Editor, false)
        .await
        .unwrap();
    let bh = hub(&bob).await;
    let bv = bh.resolve(&vid).await.unwrap();
    {
        let mut g = bv.lock().await;
        let id = g.find("Soup.md").unwrap().1.id;
        g.set_note_text(&id, "leeks\nand a pinch of salt\n")
            .unwrap();
        g.save().unwrap();
    }
    ops::delete_document(&ah, "Home", "Soup.md").await.unwrap();
    ops::empty_trash(&ah, "Home").await.unwrap();
    (ah, bh, vid)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unsent_edits_of_a_note_deleted_elsewhere_are_kept() {
    // Back online: the catch-up brings the deletion first.
    let srv = start().await;
    let (ah, bh, vid) = deleted_while_unsent(&srv).await;
    bh.sync_vault(&vid).await.unwrap();
    ah.sync_vault(&vid).await.unwrap();
    assert_eq!(
        note(&ah, &vid, "Soup (recovered).md").await.as_deref(),
        Some("leeks\nand a pinch of salt\n")
    );
    assert!(note(&ah, &vid, "Soup.md").await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unsent_edits_refused_because_their_note_is_gone_are_kept() {
    // Back online: the upload comes first and is refused.
    let srv = start().await;
    let (ah, bh, vid) = deleted_while_unsent(&srv).await;
    bh.push(&vid).await.unwrap();
    bh.sync_vault(&vid).await.unwrap();
    ah.sync_vault(&vid).await.unwrap();
    assert_eq!(
        note(&ah, &vid, "Soup (recovered).md").await.as_deref(),
        Some("leeks\nand a pinch of salt\n")
    );
    // Once: the deletion arriving afterwards finds nothing left to keep.
    assert!(note(&bh, &vid, "Soup (recovered 2).md").await.is_none());
}

/// Emptying the trash can be undone for a while: what was deleted for good
/// goes back to the trash, and its text comes back to everyone's copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deleting_for_good_can_be_undone() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    manage::create_vault(&alice, "Home").await.unwrap();
    let ah = hub(&alice).await;
    ops::put_document(&ah, "Home", "Soup.md", "leeks\n", None)
        .await
        .unwrap();
    let av = ah.resolve("Home").await.unwrap();
    let vid = av.lock().await.id().to_string();
    manage::share(&alice, &av, "bob", Role::Editor, false)
        .await
        .unwrap();
    let bh = hub(&bob).await;
    let bv = bh.resolve(&vid).await.unwrap();
    bh.sync_vault(&vid).await.unwrap();
    assert_eq!(note(&bh, &vid, "Soup.md").await.as_deref(), Some("leeks\n"));

    ops::delete_document(&ah, &vid, "Soup.md").await.unwrap();
    assert_eq!(ops::empty_trash(&ah, &vid).await.unwrap(), 1);
    assert!(av.lock().await.trashed().is_empty());
    let gone = ops::deleted_for_good(&ah, &vid).await.unwrap();
    assert_eq!(gone.len(), 1, "{gone:?}");
    assert_eq!(gone[0].0.path, "Soup.md");
    // Editors don't see it, nor undo it.
    bh.sync_vault(&vid).await.unwrap();
    assert!(bv.lock().await.trashed().is_empty());
    assert!(bv.lock().await.note_text(&gone[0].0.id).is_empty());
    assert!(ops::deleted_for_good(&bh, &vid).await.unwrap().is_empty());
    let err = ops::undo_purge(&bh, &vid, "Soup.md").await.unwrap_err();
    assert!(matches!(err, ops::OpError::Forbidden(_)), "{err:?}");

    // Undone: back in the trash, everywhere, with its text.
    let e = ops::undo_purge(&ah, &vid, "Soup.md").await.unwrap();
    assert_eq!(e.path, "Soup.md");
    let err = ops::undo_purge(&ah, &vid, "Soup.md").await.unwrap_err();
    assert!(matches!(err, ops::OpError::NotFound(_)), "{err:?}");
    assert!(ops::deleted_for_good(&ah, &vid).await.unwrap().is_empty());
    bh.sync_vault(&vid).await.unwrap();
    assert_eq!(bv.lock().await.trashed().len(), 1);
    {
        let mut g = bv.lock().await;
        g.restore("Soup.md").unwrap();
        g.save().unwrap();
    }
    assert_eq!(note(&bh, &vid, "Soup.md").await.as_deref(), Some("leeks\n"));
    bh.sync_vault(&vid).await.unwrap();
    ah.sync_vault(&vid).await.unwrap();
    assert_eq!(note(&ah, &vid, "Soup.md").await.as_deref(), Some("leeks\n"));
}

/// Editors move notes to the trash; emptying it is the owner's and the
/// vault's admins'. The owner makes someone an admin in the signed log.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_owner_and_admins_empty_the_trash() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    manage::create_vault(&alice, "Home").await.unwrap();
    let ah = hub(&alice).await;
    for name in ["Soup.md", "Stew.md"] {
        ops::put_document(&ah, "Home", name, "leeks\n", None)
            .await
            .unwrap();
    }
    let av = ah.resolve("Home").await.unwrap();
    let vid = av.lock().await.id().to_string();
    manage::share(&alice, &av, "bob", Role::Editor, false)
        .await
        .unwrap();
    let bh = hub(&bob).await;
    let bv = bh.resolve(&vid).await.unwrap();

    // As an editor, bob trashes and restores, but can't delete for good.
    ops::delete_document(&bh, &vid, "Soup.md").await.unwrap();
    let err = ops::empty_trash(&bh, &vid).await.unwrap_err();
    assert!(matches!(err, ops::OpError::Forbidden(_)), "{err:?}");
    assert_eq!(bv.lock().await.trashed().len(), 1);

    // Made an admin, he can.
    let out = manage::share(&alice, &av, "bob", Role::Admin, false)
        .await
        .unwrap();
    assert!(out.updated_role);
    bob.refresh_vault(&bv).await.unwrap();
    assert_eq!(bv.lock().await.role(), Role::Admin);
    assert_eq!(ops::empty_trash(&bh, &vid).await.unwrap(), 1);
    ah.sync_vault(&vid).await.unwrap();
    assert!(av.lock().await.trashed().is_empty());
    assert!(note(&ah, &vid, "Soup.md").await.is_none());

    // So does the owner, whatever an admin trashed.
    ops::delete_document(&bh, &vid, "Stew.md").await.unwrap();
    ah.sync_vault(&vid).await.unwrap();
    assert_eq!(ops::empty_trash(&ah, &vid).await.unwrap(), 1);

    // Agents are never admins.
    let agent = manage::create_agent(&alice, "Helper").await.unwrap();
    let Err(err) = manage::share(&alice, &av, &agent.agent.id, Role::Admin, false).await else {
        panic!("an agent was made an admin");
    };
    assert!(
        err.to_string().contains("agents can't be vault admins"),
        "{err:#}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_only_server_keeps_edits_until_it_accepts_them_again() {
    use notekpr_core::protocol::{InstanceSettingsUpdate, ReadOnly};
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Notes").await.unwrap();
    let live = live(hub(&alice).await).await;
    let v = live.hub.resolve("Notes").await.unwrap();
    let set_read_only = |on: bool| {
        let state = srv.state.clone();
        async move {
            let change = InstanceSettingsUpdate {
                read_only: Some(on),
                ..Default::default()
            };
            state.instance.update(&state.db, &change).await.unwrap();
        }
    };

    set_read_only(true).await;
    eventually(
        10,
        "the live connection hears the server is read-only",
        || async { live.hub.server_read_only() == Some(ReadOnly::Manual) },
    )
    .await;
    let out = ops::put_document(&live.hub, "Notes", "Kept.md", "written meanwhile\n", None)
        .await
        .unwrap();
    assert!(out.pending_changes > 0, "kept for later, not an error");
    // An upload that tries anyway (a one-off command) is refused and keeps
    // everything queued.
    let o = live.hub.session.push(&v).await.unwrap();
    assert!(o.read_only && !o.offline && o.remaining > 0, "{o:?}");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!v.lock().await.replica.pending.is_empty());

    // Writable again: uploaded at once, without waiting for a retry.
    set_read_only(false).await;
    eventually(5, "the kept edit is uploaded", || async {
        v.lock().await.replica.pending.is_empty()
    })
    .await;
    assert_eq!(live.hub.server_read_only(), None);
    live.stop().await;
    let fresh = hub(&reopen(&srv, "alice", "alice").await).await;
    assert_eq!(
        note(&fresh, "Notes", "Kept.md").await.as_deref(),
        Some("written meanwhile\n")
    );
}

/// A vault made here knows its storage limit at once (`vault list`), and
/// quick notes theirs.
#[tokio::test]
async fn new_vaults_know_their_storage_limit_at_once() {
    use notekpr_core::protocol::{InstanceSettingsUpdate, Limit};
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let change = InstanceSettingsUpdate {
        storage_per_vault_mb: Some(Limit::Max(100)),
        quick_notes_storage_mb: Some(Limit::Max(20)),
        ..Default::default()
    };
    srv.state
        .instance
        .update(&srv.state.db, &change)
        .await
        .unwrap();
    let v = manage::create_vault(&alice, "Photos").await.unwrap();
    assert_eq!(v.storage_limit_mb, Limit::Max(100));
    let q = manage::create_quick_notes(&alice).await.unwrap().unwrap();
    assert_eq!(q.storage_limit_mb, Limit::Max(20));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_server_keeps_edits_in_every_vault_until_there_is_room() {
    use notekpr_core::protocol::{InstanceSettingsUpdate, Limit};
    let srv = start().await;
    // The first person in administers the server too.
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Photos").await.unwrap();
    manage::create_vault(&alice, "Notes").await.unwrap();
    let live = live(hub(&alice).await).await;
    let photos = live.hub.resolve("Photos").await.unwrap();
    let notes = live.hub.resolve("Notes").await.unwrap();
    let limit = |mb: Limit| {
        let api = &alice.api;
        async move {
            let change = InstanceSettingsUpdate {
                max_storage_mb: Some(mb),
                ..Default::default()
            };
            let _: serde_json::Value = api.patch("/admin/settings", &change).await.unwrap();
        }
    };

    limit(Limit::Max(0)).await;
    for v in [&photos, &notes] {
        eventually(10, "every vault is full, by the server", || async {
            let g = v.lock().await;
            g.full && g.full_by_server
        })
        .await;
    }
    ops::put_document(&live.hub, "Notes", "Kept.md", "written meanwhile\n", None)
        .await
        .unwrap();
    assert!(!notes.lock().await.replica.pending.is_empty());

    limit(Limit::Unlimited).await;
    eventually(5, "the kept edit is uploaded", || async {
        notes.lock().await.replica.pending.is_empty()
    })
    .await;
    assert!(!photos.lock().await.full && !photos.lock().await.full_by_server);
    live.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_vault_keeps_edits_until_it_has_room() {
    use notekpr_core::protocol::{AdminUpdateVault, Allowance};
    let srv = start().await;
    // The first person in administers the server too.
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Photos").await.unwrap();
    manage::create_vault(&alice, "Notes").await.unwrap();
    let live = live(hub(&alice).await).await;
    let v = live.hub.resolve("Photos").await.unwrap();
    let id = v.lock().await.id().to_string();
    let path = format!("/admin/vaults/{id}");
    let limit = |a: Allowance| {
        let (api, path) = (&alice.api, &path);
        async move {
            let change = AdminUpdateVault {
                storage_allowance: Some(a),
                ..Default::default()
            };
            let _: serde_json::Value = api.patch(path, &change).await.unwrap();
        }
    };

    limit(Allowance::Max(0)).await;
    eventually(
        10,
        "the live connection hears the vault is full",
        || async { v.lock().await.full },
    )
    .await;
    let out = ops::put_document(&live.hub, "Photos", "Kept.md", "written meanwhile\n", None)
        .await
        .unwrap();
    assert!(out.pending_changes > 0, "kept for later, not an error");
    // An upload that tries anyway is refused and keeps everything queued.
    v.lock().await.full = false;
    let o = live.hub.session.push(&v).await.unwrap();
    assert!(o.vault_full && !o.read_only && o.remaining > 0, "{o:?}");
    assert!(v.lock().await.full);
    let s = live.hub.summaries().await;
    assert!(s.iter().find(|s| s.id == id).unwrap().full);
    // Other vaults go on.
    ops::put_document(&live.hub, "Notes", "Other.md", "fine\n", None)
        .await
        .unwrap();
    let notes = live.hub.resolve("Notes").await.unwrap();
    eventually(5, "the other vault's edit is uploaded", || async {
        notes.lock().await.replica.pending.is_empty()
    })
    .await;
    assert!(!v.lock().await.replica.pending.is_empty());

    // Room again: uploaded at once, without waiting for a retry.
    limit(Allowance::Default).await;
    eventually(5, "the kept edit is uploaded", || async {
        v.lock().await.replica.pending.is_empty()
    })
    .await;
    assert!(!v.lock().await.full);
    live.stop().await;
    let fresh = hub(&reopen(&srv, "alice", "alice").await).await;
    assert_eq!(
        note(&fresh, "Photos", "Kept.md").await.as_deref(),
        Some("written meanwhile\n")
    );
}
