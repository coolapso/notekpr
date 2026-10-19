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

    let shared = manage::share(&alice, &fam, "bob", Role::Editor, false).await.unwrap();
    assert!(shared.key_shared);
    assert_eq!(shared.pin, Some(PinStatus::New));
    assert_eq!(shared.fingerprint.as_deref(), Some(bob.fingerprint().as_str()));

    // Bob's sync is running before Alice writes anything.
    let bob_live = live(hub(&bob).await).await;
    assert_eq!(bob_live.hub.vault_ids().len(), 1);

    ops::put_document(&alice_hub, "Family", "Welcome.md", "# Hi Bob\n", None).await.unwrap();
    eventually(10, "bob receives alice's note over the websocket", || async {
        note(&bob_live.hub, "Family", "Welcome.md").await.as_deref() == Some("# Hi Bob\n")
    })
    .await;

    // Bob's live edit flows back to Alice.
    ops::put_document(&bob_live.hub, "Family", "Welcome.md", "# Hi Bob\nHi Alice!\n", None).await.unwrap();
    let fam_id = fam.lock().await.id().to_string();
    alice_hub.sync_vault(&fam_id).await.unwrap();
    assert_eq!(note(&alice_hub, "Family", "Welcome").await.as_deref(), Some("# Hi Bob\nHi Alice!\n"));
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
    manage::share(&alice, &v, "bob", Role::Editor, false).await.unwrap();
    ops::put_document(&alice_hub, &vid, "Plan.md", "line 1\nline 2\nline 3\n", None).await.unwrap();
    let bob_hub = hub(&bob).await;
    assert_eq!(note(&bob_hub, &vid, "Plan.md").await.as_deref(), Some("line 1\nline 2\nline 3\n"));
    drop(bob_hub);

    // Bob goes offline (server unreachable) and edits the first line.
    let bob_profile = ProfileStore::new(&srv.dirs("bob"), "bob").unwrap();
    let real = bob_profile.require().unwrap().server;
    let dead = dead_url().await;
    bob_profile.update(|p| p.server = dead.clone()).unwrap();
    let offline = hub(&reopen(&srv, "bob", "bob").await).await;
    assert!(!offline.is_online());
    let out = ops::put_document(&offline, &vid, "Plan.md", "line 1 (bob)\nline 2\nline 3\n", None).await.unwrap();
    assert!(out.pending_changes > 0, "edit must stay queued while offline");
    drop(offline);

    // Meanwhile Alice edits the last line of the same note.
    ops::put_document(&alice_hub, &vid, "Plan.md", "line 1\nline 2\nline 3 (alice)\n", None).await.unwrap();

    // Bob's client restarts with the server back: the queue was persisted.
    bob_profile.update(|p| p.server = real.clone()).unwrap();
    let session = reopen(&srv, "bob", "bob").await;
    let pending = session.load_local(&vid).unwrap().unwrap().replica.pending.clone();
    assert!(!pending.is_empty());
    // Simulate a crash right after the upload reached the server but before
    // the acknowledgement was recorded.
    let pushes = session.load_local(&vid).unwrap().unwrap().replica.pending_pushes().unwrap();
    let first = session.api.push(&vid, pushes).await.unwrap();
    let head_after_first = first.head_seq;

    // Restart again and sync: the same client_update_ids are retried and the
    // server recognises them (same sequence numbers, head unchanged).
    let bob_hub = coolpsonotes_client::hub::Hub::start(
        session.clone(),
        coolpsonotes_client::hub::HubOptions { offline_ok: true, ..Default::default() },
    )
    .await
    .unwrap();
    assert_eq!(bob_hub.get(&vid).unwrap().lock().await.replica.pending.len(), pending.len());
    bob_hub.sync_vault(&vid).await.unwrap();
    assert!(bob_hub.get(&vid).unwrap().lock().await.replica.pending.is_empty());
    let changes = session.api.changes(&vid, 0, 1000, true).await.unwrap();
    assert_eq!(changes.head_seq, head_after_first, "retry must not create new log entries");

    // Both edits survive, on both sides.
    let merged = "line 1 (bob)\nline 2\nline 3 (alice)\n";
    bob_hub.sync_vault(&vid).await.unwrap();
    assert_eq!(note(&bob_hub, &vid, "Plan.md").await.as_deref(), Some(merged));
    alice_hub.sync_vault(&vid).await.unwrap();
    assert_eq!(note(&alice_hub, &vid, "Plan.md").await.as_deref(), Some(merged));
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
    manage::share(&alice, &v, "bob", Role::Viewer, false).await.unwrap();
    ops::put_document(&alice_hub, &vid, "A.md", "before", None).await.unwrap();
    let bob_live = live(hub(&bob).await).await;
    eventually(10, "bob sees A.md", || async { note(&bob_live.hub, &vid, "A.md").await.as_deref() == Some("before") })
        .await;
    // Viewers cannot write.
    assert!(matches!(ops::put_document(&bob_live.hub, &vid, "A.md", "x", None).await, Err(ops::OpError::Forbidden(_))));

    let (_, epoch) = manage::unshare(&alice, &v, "bob").await.unwrap();
    assert_eq!(epoch, 2);
    eventually(10, "bob's copy becomes read-only", || async {
        bob_live.hub.get(&vid).unwrap().lock().await.read_only.is_some()
    })
    .await;
    ops::put_document(&alice_hub, &vid, "A.md", "after", None).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    // The local copy stays readable but never receives post-revocation content.
    assert_eq!(note(&bob_live.hub, &vid, "A.md").await.as_deref(), Some("before"));
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
        ops::put_document(&h, "Journal", "Today.md", &text, None).await.unwrap();
    }
    assert_eq!(h.snapshot_idle(std::time::Duration::ZERO, 5).await, 1);
    ops::put_document(&h, "Journal", "Today.md", "rewritten\n", None).await.unwrap();
    let revs = ops::history(&h, "Journal", "Today.md").await.unwrap();
    assert_eq!(revs.len(), 1);
    let old = ops::revision(&h, "Journal", "Today.md", &revs[0].id).await.unwrap();
    assert_eq!(old, text);
    // Too few edits since the last revision: no new one.
    assert_eq!(h.snapshot_idle(std::time::Duration::ZERO, 5).await, 0);
}
