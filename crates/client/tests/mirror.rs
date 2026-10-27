//! The plaintext file mirror: two-way sync with a directory, external edits
//! racing remote edits, deletes and renames.

mod common;

use std::path::Path;
use std::time::Duration;

use common::*;
use notekpr_client::mirror::Mirror;
use notekpr_client::{manage, note_shares, ops};
use notekpr_core::protocol::Role;
use tokio::sync::watch;

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

fn write(p: &Path, s: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mirror_syncs_both_ways_and_merges_external_edits() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    manage::create_vault(&alice, "Shared").await.unwrap();
    let alice_hub = hub(&alice).await;
    let v = alice_hub.resolve("Shared").await.unwrap();
    let vid = v.lock().await.id().to_string();
    manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .unwrap();
    ops::put_document(&alice_hub, &vid, "Notes/Plan.md", "alpha\nbeta\n", None)
        .await
        .unwrap();

    // ---- live daemon: watcher + websocket
    let mirror_root = srv.path("bob-mirror");
    let bob_live = live(hub(&bob).await).await;
    let mut mirror = Mirror::new(bob_live.hub.clone(), &mirror_root)
        .await
        .unwrap();
    mirror.reconcile_all(false).await.unwrap();
    let dir = mirror.vault_dir(&vid).unwrap().to_path_buf();
    assert_eq!(dir.file_name().unwrap(), "Shared");
    let plan = dir.join("Notes/Plan.md");
    assert_eq!(read(&plan), "alpha\nbeta\n");
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(mirror.run(rx));

    // A file written by an editor shows up for Alice, as it was written
    // (`@today` stays: only the app, the CLI and the gateway turn it into a
    // date, never a file under someone's editor).
    write(&dir.join("Ideas/new.md"), "from bob's editor @today\n");
    eventually(15, "alice sees the file bob created", || async {
        alice_hub.sync_vault(&vid).await.unwrap();
        note(&alice_hub, &vid, "Ideas/new.md").await.as_deref()
            == Some("from bob's editor @today\n")
    })
    .await;

    // A remote edit updates Bob's file.
    ops::put_document(
        &alice_hub,
        &vid,
        "Notes/Plan.md",
        "alpha\nbeta\ngamma\n",
        None,
    )
    .await
    .unwrap();
    eventually(15, "bob's file receives alice's edit", || async {
        read(&plan) == "alpha\nbeta\ngamma\n"
    })
    .await;
    let _ = stop.send(true);
    task.await.unwrap().unwrap();
    bob_live.stop().await;

    // ---- deterministic passes (state reloaded from the encrypted store)
    let bob_hub = hub(&reopen(&srv, "bob", "bob").await).await;
    let mut mirror = Mirror::new(bob_hub.clone(), &mirror_root).await.unwrap();
    let r = mirror.reconcile_all(true).await.unwrap();
    assert_eq!(r.local_changes(), 0, "nothing changed on disk: {r:?}");

    // Race 1: the editor still holds V1 when a remote edit (V2) is written
    // to disk; it then saves V1 + its own change over V2.
    let v1 = read(&plan);
    ops::put_document(
        &alice_hub,
        &vid,
        "Notes/Plan.md",
        "ALPHA\nbeta\ngamma\n",
        None,
    )
    .await
    .unwrap();
    bob_hub.sync_vault(&vid).await.unwrap();
    mirror.reconcile_all(false).await.unwrap();
    assert_eq!(read(&plan), "ALPHA\nbeta\ngamma\n");
    tokio::time::sleep(Duration::from_millis(20)).await;
    write(&plan, &format!("{v1}delta (bob)\n"));
    mirror.reconcile_all(true).await.unwrap();
    let both = "ALPHA\nbeta\ngamma\ndelta (bob)\n";
    assert_eq!(
        read(&plan),
        both,
        "the stale save must not revert the remote edit"
    );
    alice_hub.sync_vault(&vid).await.unwrap();
    assert_eq!(
        note(&alice_hub, &vid, "Notes/Plan.md").await.as_deref(),
        Some(both)
    );

    // Race 2: a remote edit arrives while a local edit is not ingested yet.
    write(&plan, &format!("{both}epsilon (bob)\n"));
    ops::put_document(
        &alice_hub,
        &vid,
        "Notes/Plan.md",
        "ALPHA\nBETA (alice)\ngamma\ndelta (bob)\n",
        None,
    )
    .await
    .unwrap();
    bob_hub.sync_vault(&vid).await.unwrap();
    mirror.reconcile_all(true).await.unwrap();
    let all = "ALPHA\nBETA (alice)\ngamma\ndelta (bob)\nepsilon (bob)\n";
    assert_eq!(read(&plan), all);
    alice_hub.sync_vault(&vid).await.unwrap();
    assert_eq!(
        note(&alice_hub, &vid, "Notes/Plan.md").await.as_deref(),
        Some(all)
    );

    // Deleting a file moves the entry to the trash (restorable).
    let new_id = bob_hub
        .get(&vid)
        .unwrap()
        .lock()
        .await
        .find("Ideas/new.md")
        .unwrap()
        .1
        .id;
    std::fs::remove_file(dir.join("Ideas/new.md")).unwrap();
    let r = mirror.reconcile_all(true).await.unwrap();
    assert_eq!(r.trashed, 1);
    alice_hub.sync_vault(&vid).await.unwrap();
    {
        let mut g = v.lock().await;
        assert!(g.find("Ideas/new.md").is_none());
        assert!(g.trashed().iter().any(|e| e.id == new_id));
        g.restore(&new_id).unwrap();
        g.save().unwrap();
    }
    alice_hub.push(&vid).await.unwrap();
    bob_hub.sync_vault(&vid).await.unwrap();
    mirror.reconcile_all(true).await.unwrap();
    assert_eq!(
        read(&dir.join("Ideas/new.md")),
        "from bob's editor @today\n",
        "restored note is written back"
    );

    // Renaming a file moves the entry (same id, same content).
    let plan_id = bob_hub
        .get(&vid)
        .unwrap()
        .lock()
        .await
        .find("Notes/Plan.md")
        .unwrap()
        .1
        .id;
    std::fs::create_dir_all(dir.join("Archive")).unwrap();
    std::fs::rename(&plan, dir.join("Archive/Old plan.md")).unwrap();
    let r = mirror.reconcile_all(true).await.unwrap();
    // The rename is detected (no trash + create); the now-empty "Notes"
    // directory stays on disk and is recorded as an explicit folder.
    assert_eq!((r.renamed, r.trashed, r.created), (1, 0, 1), "{r:?}");
    alice_hub.sync_vault(&vid).await.unwrap();
    {
        let g = v.lock().await;
        let (_, e) = g.find("Archive/Old plan.md").expect("moved entry");
        assert_eq!(e.id, plan_id);
        assert_eq!(g.note_text(&e.id), all);
    }

    // Ignored files never sync.
    write(&dir.join(".obsidian/workspace.json"), "{}");
    write(&dir.join("Archive/.Old plan.md.swp"), "swap");
    write(&dir.join("Archive/Old plan.md~"), "backup");
    let r = mirror.reconcile_all(true).await.unwrap();
    assert_eq!(r.local_changes(), 0, "{r:?}");

    // A remote rename moves Bob's file.
    ops::move_document(&alice_hub, &vid, "Archive/Old plan.md", "Plan.md", false)
        .await
        .unwrap();
    bob_hub.sync_vault(&vid).await.unwrap();
    mirror.reconcile_all(true).await.unwrap();
    assert_eq!(read(&dir.join("Plan.md")), all);
    assert!(!dir.join("Archive/Old plan.md").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_offline_creation_gives_two_files_not_a_lost_note() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    manage::create_vault(&alice, "Ideas").await.unwrap();
    let alice_hub = hub(&alice).await;
    let v = alice_hub.resolve("Ideas").await.unwrap();
    let vid = v.lock().await.id().to_string();
    manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .unwrap();

    // Bob is offline (uploads deferred) while both create the same path.
    let bob_hub = hub(&bob).await;
    bob_hub.set_push_now(false);
    let root = srv.path("bob-mirror");
    let mut mirror = Mirror::new(bob_hub.clone(), &root).await.unwrap();
    mirror.reconcile_all(true).await.unwrap();
    let dir = mirror.vault_dir(&vid).unwrap().to_path_buf();
    write(&dir.join("Idea.md"), "bob's idea\n");
    mirror.reconcile_all(true).await.unwrap();
    ops::put_document(&alice_hub, &vid, "Idea.md", "alice's idea\n", None)
        .await
        .unwrap();

    // Reconnect: both entries exist; the older keeps the name.
    bob_hub.set_push_now(true);
    bob_hub.sync_vault(&vid).await.unwrap();
    mirror.reconcile_all(true).await.unwrap();
    alice_hub.sync_vault(&vid).await.unwrap();
    let mut texts = vec![read(&dir.join("Idea.md")), read(&dir.join("Idea 2.md"))];
    texts.sort();
    assert_eq!(
        texts,
        vec!["alice's idea\n".to_string(), "bob's idea\n".to_string()]
    );
    let a_paths: Vec<String> = v.lock().await.paths().into_keys().collect();
    assert_eq!(
        a_paths,
        vec!["Idea 2.md".to_string(), "Idea.md".to_string()]
    );

    // Editing the suffixed file edits that entry; nothing is renamed in the shared state.
    let second = read(&dir.join("Idea 2.md"));
    write(&dir.join("Idea 2.md"), &format!("{second}more\n"));
    mirror.reconcile_all(true).await.unwrap();
    alice_hub.sync_vault(&vid).await.unwrap();
    assert_eq!(
        note(&alice_hub, &vid, "Idea 2.md").await.unwrap(),
        format!("{second}more\n")
    );
    let g = v.lock().await;
    assert!(
        g.replica
            .index
            .entries()
            .iter()
            .all(|e| e.path == "Idea.md")
    );
}

/// A vault owned by alice and shared with bob (editor), plus bob's mirror.
async fn shared_mirror(
    srv: &TestServer,
    name: &str,
) -> (
    std::sync::Arc<notekpr_client::hub::Hub>,
    String,
    std::sync::Arc<notekpr_client::hub::Hub>,
    Mirror,
    std::path::PathBuf,
) {
    let alice = login(srv, "alice").await;
    let bob = login(srv, "bob").await;
    manage::create_vault(&alice, name).await.unwrap();
    let alice_hub = hub(&alice).await;
    let v = alice_hub.resolve(name).await.unwrap();
    let vid = v.lock().await.id().to_string();
    manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .unwrap();
    let bob_hub = hub(&bob).await;
    let mirror = Mirror::new(bob_hub.clone(), &srv.path("bob-mirror"))
        .await
        .unwrap();
    let dir = mirror.vault_dir(&vid).unwrap().to_path_buf();
    (alice_hub, vid, bob_hub, mirror, dir)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_bad_file_does_not_stop_the_mirror() {
    use notekpr_core::doc::EntryKind;
    use notekpr_core::protocol::INDEX_OBJECT_ID;
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Long").await.unwrap();
    let h = hub(&alice).await;
    let v = h.resolve("Long").await.unwrap();
    let vid = v.lock().await.id().to_string();
    ops::put_document(&h, &vid, "Good.md", "good\n", None)
        .await
        .unwrap();
    // Two notes with the same 255-byte name: the second is shown as
    // "… 2.md", which no file system accepts.
    let long = format!("{}.md", "x".repeat(252));
    {
        let mut g = v.lock().await;
        g.create_note(&long, "first\n").unwrap();
        let id = notekpr_core::random_id();
        let u = g
            .replica
            .index
            .create_with(
                &id,
                &long,
                EntryKind::Note,
                notekpr_core::now_ms() + 1000,
                Some(7),
                None,
            )
            .unwrap();
        g.record(INDEX_OBJECT_ID, u).unwrap();
        let u = g
            .replica
            .notes
            .entry(id.clone())
            .or_default()
            .set_text("second\n")
            .unwrap();
        g.record(&id, u).unwrap();
        g.save().unwrap();
    }
    let mut m = Mirror::new(h.clone(), &srv.path("mirror")).await.unwrap();
    m.reconcile_all(true).await.unwrap();
    let dir = m.vault_dir(&vid).unwrap().to_path_buf();
    assert_eq!(read(&dir.join("Good.md")), "good\n");
    // A name at the limit is written (its temporary file name used to be too long).
    assert_eq!(read(&dir.join(&long)), "first\n");
    // The other files keep syncing.
    write(&dir.join("Good.md"), "good, edited\n");
    let r = m.reconcile_all(true).await.unwrap();
    assert_eq!(r.ingested, 1, "{r:?}");
    assert_eq!(
        note(&h, &vid, "Good.md").await.as_deref(),
        Some("good, edited\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_edited_here_while_trashed_elsewhere_is_kept() {
    let srv = start().await;
    let (alice_hub, vid, bob_hub, mut m, dir) = shared_mirror(&srv, "Plans").await;
    ops::put_document(&alice_hub, &vid, "Trip.md", "day 1\n", None)
        .await
        .unwrap();
    bob_hub.sync_vault(&vid).await.unwrap();
    m.reconcile_all(true).await.unwrap();
    let trip = dir.join("Trip.md");
    assert_eq!(read(&trip), "day 1\n");

    // Bob edits the file (not picked up yet) while Alice trashes the note.
    tokio::time::sleep(Duration::from_millis(20)).await;
    write(&trip, "day 1\nday 2 (bob)\n");
    ops::delete_document(&alice_hub, &vid, "Trip.md")
        .await
        .unwrap();
    bob_hub.sync_vault(&vid).await.unwrap();
    let r = m.reconcile_all(true).await.unwrap();
    assert_eq!(
        read(&trip),
        "day 1\nday 2 (bob)\n",
        "the local edit is kept"
    );
    assert!(r.conflicts >= 1, "{r:?}");
    // ... and synced again as a note.
    alice_hub.sync_vault(&vid).await.unwrap();
    assert_eq!(
        note(&alice_hub, &vid, "Trip.md").await.as_deref(),
        Some("day 1\nday 2 (bob)\n")
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_replaced_by_a_symlink_is_not_trashed() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Links").await.unwrap();
    let h = hub(&alice).await;
    let vid = h
        .resolve("Links")
        .await
        .unwrap()
        .lock()
        .await
        .id()
        .to_string();
    ops::put_document(&h, &vid, "A.md", "a\n", None)
        .await
        .unwrap();
    let mut m = Mirror::new(h.clone(), &srv.path("mirror")).await.unwrap();
    m.reconcile_all(true).await.unwrap();
    let dir = m.vault_dir(&vid).unwrap().to_path_buf();
    let outside = srv.path("elsewhere.md");
    write(&outside, "outside\n");
    std::fs::remove_file(dir.join("A.md")).unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("A.md")).unwrap();

    let r = m.reconcile_all(true).await.unwrap();
    assert_eq!(r.trashed, 0, "{r:?}");
    assert_eq!(note(&h, &vid, "A.md").await.as_deref(), Some("a\n"));
    // A remote edit neither replaces the link nor writes through it.
    ops::put_document(&h, &vid, "A.md", "a2\n", None)
        .await
        .unwrap();
    m.reconcile_all(true).await.unwrap();
    let meta = std::fs::symlink_metadata(dir.join("A.md")).unwrap();
    assert!(meta.file_type().is_symlink());
    assert_eq!(read(&outside), "outside\n");
    assert_eq!(note(&h, &vid, "A.md").await.as_deref(), Some("a2\n"));
}

/// The quick notes' names, sorted.
async fn names(h: &notekpr_client::hub::Hub) -> Vec<String> {
    let q = ops::list_quick_notes(h, 100).await.unwrap();
    let mut n: Vec<String> = q.notes.into_iter().map(|n| n.title).collect();
    n.sort();
    n
}

async fn note_count(h: &notekpr_client::hub::Hub, vid: &str) -> usize {
    h.get(vid).unwrap().lock().await.notes().len()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vanished_folder_or_mass_deletion_is_held_not_trashed() {
    use notekpr_client::mirror::MirrorOptions;
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Many").await.unwrap();
    let h = hub(&alice).await;
    let vid = h
        .resolve("Many")
        .await
        .unwrap()
        .lock()
        .await
        .id()
        .to_string();
    for i in 0..12 {
        ops::put_document(&h, &vid, &format!("N{i}.md"), &format!("{i}\n"), None)
            .await
            .unwrap();
    }
    let root = srv.path("mirror");
    let mut m = Mirror::new(h.clone(), &root).await.unwrap();
    m.reconcile_all(true).await.unwrap();
    let dir = m.vault_dir(&vid).unwrap().to_path_buf();
    assert_eq!(read(&dir.join("N3.md")), "3\n");

    // 1. Most files vanish at once (even in a final pass): nothing is trashed.
    let aside = srv.path("aside");
    std::fs::create_dir_all(&aside).unwrap();
    for i in 0..11 {
        let n = format!("N{i}.md");
        std::fs::rename(dir.join(&n), aside.join(&n)).unwrap();
    }
    let r = m.reconcile_all(true).await.unwrap();
    assert_eq!((r.trashed, r.held), (0, 11), "{r:?}");
    assert_eq!(note_count(&h, &vid).await, 12);
    assert!(!dir.join("N0.md").exists(), "held files are not re-created");
    // They come back: everything is as before.
    for i in 0..11 {
        let n = format!("N{i}.md");
        std::fs::rename(aside.join(&n), dir.join(&n)).unwrap();
    }
    let r = m.reconcile_all(true).await.unwrap();
    assert_eq!((r.trashed, r.held, r.local_changes()), (0, 0, 0), "{r:?}");

    // 2. The whole vault folder vanishes (moved away): held, not re-created.
    std::fs::rename(&dir, srv.path("moved-away")).unwrap();
    let r = m.reconcile_all(true).await.unwrap();
    assert_eq!((r.trashed, r.held), (0, 12), "{r:?}");
    assert!(!dir.exists(), "a vanished vault folder is not re-created");
    std::fs::rename(srv.path("moved-away"), &dir).unwrap();
    let r = m.reconcile_all(true).await.unwrap();
    assert_eq!((r.trashed, r.held), (0, 0), "{r:?}");

    // 3. The mirror root vanishes while running: nothing happens.
    std::fs::rename(&root, srv.path("unmounted")).unwrap();
    let r = m.reconcile_all(true).await.unwrap();
    assert_eq!(r.trashed, 0, "{r:?}");
    assert!(!root.exists(), "a vanished mirror root is not re-created");
    drop(m);
    // ... and a restart refuses to re-create it.
    let err = Mirror::new(h.clone(), &root).await.err().expect("refused");
    assert!(format!("{err:#}").contains("missing"), "{err:#}");
    assert!(!root.exists());
    assert_eq!(note_count(&h, &vid).await, 12);
    // --restore-missing writes a fresh copy instead.
    let mut m = Mirror::with_options(
        h.clone(),
        &root,
        MirrorOptions {
            restore_missing: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let r = m.reconcile_all(true).await.unwrap();
    assert_eq!((r.trashed, r.restored), (0, 12), "{r:?}");
    assert_eq!(read(&dir.join("N7.md")), "7\n");
    assert_eq!(note_count(&h, &vid).await, 12);

    // 4. Confirmed mass deletion: --allow-mass-delete trashes them.
    for i in 0..11 {
        std::fs::remove_file(dir.join(format!("N{i}.md"))).unwrap();
    }
    m.set_options(MirrorOptions {
        allow_mass_delete: true,
        ..Default::default()
    });
    let r = m.reconcile_all(true).await.unwrap();
    assert_eq!((r.trashed, r.held), (11, 0), "{r:?}");
    assert_eq!(note_count(&h, &vid).await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reverting_a_file_to_an_older_version_is_synced() {
    let srv = start().await;
    let (alice_hub, vid, bob_hub, mut m, dir) = shared_mirror(&srv, "Undo").await;
    ops::put_document(&alice_hub, &vid, "Plan.md", "v1\n", None)
        .await
        .unwrap();
    bob_hub.sync_vault(&vid).await.unwrap();
    m.reconcile_all(true).await.unwrap();
    let plan = dir.join("Plan.md");
    assert_eq!(read(&plan), "v1\n");

    // Bob's editor saves an edit ...
    tokio::time::sleep(Duration::from_millis(20)).await;
    write(&plan, "v1\nv2 (bob)\n");
    m.reconcile_all(true).await.unwrap();
    assert_eq!(
        note(&bob_hub, &vid, "Plan.md").await.as_deref(),
        Some("v1\nv2 (bob)\n")
    );
    // ... then undoes it and saves again (or `git checkout`s the old file).
    tokio::time::sleep(Duration::from_millis(20)).await;
    write(&plan, "v1\n");
    let r = m.reconcile_all(true).await.unwrap();
    assert_eq!(r.ingested, 1, "{r:?}");
    assert_eq!(read(&plan), "v1\n", "the revert must not be overwritten");
    assert_eq!(
        note(&bob_hub, &vid, "Plan.md").await.as_deref(),
        Some("v1\n")
    );
    alice_hub.sync_vault(&vid).await.unwrap();
    assert_eq!(
        note(&alice_hub, &vid, "Plan.md").await.as_deref(),
        Some("v1\n")
    );

    // A stale editor buffer saved after a remote edit still keeps the remote
    // edit (it started from what we wrote, not from its own last save).
    ops::put_document(&alice_hub, &vid, "Plan.md", "v1\nremote\n", None)
        .await
        .unwrap();
    bob_hub.sync_vault(&vid).await.unwrap();
    m.reconcile_all(true).await.unwrap();
    assert_eq!(read(&plan), "v1\nremote\n");
    tokio::time::sleep(Duration::from_millis(20)).await;
    write(&plan, "v1\nlocal\n");
    m.reconcile_all(true).await.unwrap();
    let merged = note(&bob_hub, &vid, "Plan.md").await.unwrap();
    assert!(
        merged.contains("remote") && merged.contains("local"),
        "{merged:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leftover_files_are_not_imported_into_a_new_vault_with_the_same_name() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Diary").await.unwrap();
    let h = hub(&alice).await;
    let old = h.resolve("Diary").await.unwrap();
    let old_id = old.lock().await.id().to_string();
    ops::put_document(&h, &old_id, "Secret.md", "old vault\n", None)
        .await
        .unwrap();
    let root = srv.path("mirror");
    let mut m = Mirror::new(h.clone(), &root).await.unwrap();
    m.reconcile_all(true).await.unwrap();
    let old_dir = m.vault_dir(&old_id).unwrap().to_path_buf();
    assert_eq!(old_dir.file_name().unwrap(), "Diary");
    drop(m);

    // The same vault without its mirror bookkeeping (e.g. a re-login) adopts
    // its own folder again: nothing is duplicated.
    {
        let g = old.lock().await;
        std::fs::remove_file(g.store().dir().join("mirror.sealed")).unwrap();
    }
    let mut m = Mirror::new(h.clone(), &root).await.unwrap();
    let r = m.reconcile_all(true).await.unwrap();
    assert_eq!(m.vault_dir(&old_id).unwrap(), old_dir);
    assert_eq!((r.created, r.conflicts), (0, 0), "{r:?}");
    drop(m);

    // The vault is deleted; its files stay on disk. A new vault with the same
    // name must not import them.
    manage::delete_vault(&alice, &old).await.unwrap();
    manage::create_vault(&alice, "Diary").await.unwrap();
    let h = hub(&reopen(&srv, "alice", "alice").await).await;
    let new_id = h
        .resolve("Diary")
        .await
        .unwrap()
        .lock()
        .await
        .id()
        .to_string();
    assert_ne!(new_id, old_id);
    let mut m = Mirror::new(h.clone(), &root).await.unwrap();
    let r = m.reconcile_all(true).await.unwrap();
    assert_eq!(r.created, 0, "{r:?}");
    let new_dir = m.vault_dir(&new_id).unwrap().to_path_buf();
    assert_ne!(new_dir, old_dir);
    assert!(
        new_dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("Diary (")
    );
    assert_eq!(read(&old_dir.join("Secret.md")), "old vault\n");
    assert!(h.get(&new_id).unwrap().lock().await.notes().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn case_insensitive_file_systems_keep_case_variants_apart() {
    use notekpr_client::mirror::MirrorOptions;
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Notes").await.unwrap();
    manage::create_vault(&alice, "notes").await.unwrap();
    let h = hub(&alice).await;
    let mut ids = Vec::new();
    for v in h.vaults() {
        let g = v.lock().await;
        ids.push((g.name().to_string(), g.id().to_string()));
    }
    ids.sort();
    let (upper, lower) = (ids[0].1.clone(), ids[1].1.clone());
    ops::put_document(&h, &upper, "Idea.md", "upper case\n", None)
        .await
        .unwrap();
    {
        // (`put_document` would match "Idea.md" case-insensitively.)
        let v = h.get(&upper).unwrap();
        let mut g = v.lock().await;
        g.create_note("idea.md", "lower case\n").unwrap();
        g.save().unwrap();
    }
    ops::put_document(&h, &lower, "x.md", "other vault\n", None)
        .await
        .unwrap();

    // macOS and Windows defaults: forced here, detected in real use.
    let opts = MirrorOptions {
        case_insensitive: Some(true),
        ..Default::default()
    };
    let mut m = Mirror::with_options(h.clone(), &srv.path("mirror"), opts)
        .await
        .unwrap();
    m.reconcile_all(true).await.unwrap();
    // "Notes" and "notes" do not share a folder.
    let d_upper = m.vault_dir(&upper).unwrap().to_path_buf();
    let d_lower = m.vault_dir(&lower).unwrap().to_path_buf();
    assert_ne!(
        d_upper.to_string_lossy().to_lowercase(),
        d_lower.to_string_lossy().to_lowercase()
    );
    // "Idea.md" and "idea.md" get files whose names differ ignoring case.
    let mut names: Vec<String> = std::fs::read_dir(&d_upper)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    assert_eq!(names, ["Idea.md", "idea 2.md"]);
    assert_eq!(read(&d_upper.join("Idea.md")), "upper case\n");
    assert_eq!(read(&d_upper.join("idea 2.md")), "lower case\n");
    // Stable: no ping-pong, no duplicates.
    let r = m.reconcile_all(true).await.unwrap();
    assert!(r.is_empty(), "{r:?}");
    // Editing each file edits its own note.
    tokio::time::sleep(Duration::from_millis(20)).await;
    write(&d_upper.join("idea 2.md"), "lower case, edited\n");
    let r = m.reconcile_all(true).await.unwrap();
    assert_eq!((r.ingested, r.created), (1, 0), "{r:?}");
    assert_eq!(
        note(&h, &upper, "idea.md").await.as_deref(),
        Some("lower case, edited\n")
    );
    assert_eq!(
        note(&h, &upper, "Idea.md").await.as_deref(),
        Some("upper case\n")
    );
    assert_eq!(h.get(&upper).unwrap().lock().await.notes().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn server_limits_are_remembered_and_enforced_offline() {
    use notekpr_client::profile::{ProfileStore, ServerLimits};
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Files").await.unwrap();
    let h = hub(&alice).await;
    let vid = h
        .resolve("Files")
        .await
        .unwrap()
        .lock()
        .await
        .id()
        .to_string();
    drop(h);
    // Learned while online.
    let store = ProfileStore::new(&srv.dirs("alice"), "alice").unwrap();
    assert_eq!(
        store.require().unwrap().server_limits,
        Some(ServerLimits {
            max_snapshot_bytes: 32 << 20,
            ..ServerLimits::default()
        })
    );

    // Offline, with a (remembered) small attachment limit.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = format!("http://{}", l.local_addr().unwrap());
    drop(l);
    store
        .update(|p| {
            p.server = dead.clone();
            p.server_limits = Some(ServerLimits {
                max_attachment_bytes: 10,
                max_update_bytes: 2048 * 1024,
                max_snapshot_bytes: 0,
            });
        })
        .unwrap();
    let offline = hub(&reopen(&srv, "alice", "alice").await).await;
    assert!(!offline.is_online());
    let mut m = Mirror::new(offline.clone(), &srv.path("mirror"))
        .await
        .unwrap();
    m.reconcile_all(true).await.unwrap();
    let dir = m.vault_dir(&vid).unwrap().to_path_buf();
    std::fs::write(dir.join("big.bin"), [7u8; 100]).unwrap();
    std::fs::write(dir.join("small.bin"), [7u8; 5]).unwrap();
    let r = m.reconcile_all(true).await.unwrap();
    assert_eq!(r.created, 1, "only the small file is synced: {r:?}");
    assert!(matches!(
        ops::put_attachment(&offline, &vid, "x.bin", &[0u8; 100], None).await,
        Err(ops::OpError::BadRequest(_))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_named_with_separate_accent_marks_is_the_same_note() {
    use notekpr_client::mirror::MirrorOptions;
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Notes").await.unwrap();
    let h = hub(&alice).await;
    let vid = h
        .resolve("Notes")
        .await
        .unwrap()
        .lock()
        .await
        .id()
        .to_string();
    let mut m = Mirror::with_options(h.clone(), &srv.path("mirror"), MirrorOptions::default())
        .await
        .unwrap();
    m.reconcile_all(true).await.unwrap();
    let dir = m.vault_dir(&vid).unwrap().to_path_buf();
    // "Café.md" as macOS lists it (and zips made there name it): "e" and a
    // separate accent mark.
    let decomposed = "Cafe\u{301}.md";
    write(&dir.join(decomposed), "croissants\n");
    let r = m.reconcile_all(true).await.unwrap();
    assert_eq!(r.created, 1, "{r:?}");
    // The note's name is written the one usual way; the file keeps its own.
    assert_eq!(
        note(&h, &vid, "Caf\u{e9}.md").await.as_deref(),
        Some("croissants\n")
    );
    // Changes made elsewhere go into that same file: never a second "Café".
    ops::put_document(&h, &vid, "Caf\u{e9}.md", "croissants\nand coffee\n", None)
        .await
        .unwrap();
    m.reconcile_all(true).await.unwrap();
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    assert_eq!(names, [decomposed]);
    assert_eq!(read(&dir.join(decomposed)), "croissants\nand coffee\n");
    let r = m.reconcile_all(true).await.unwrap();
    assert!(r.is_empty(), "{r:?}");
    assert_eq!(h.get(&vid).unwrap().lock().await.notes().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_note_renamed_in_the_folder_is_renamed_for_whom_it_is_shared_with() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let carol = login(&srv, "carol").await;
    manage::create_vault(&alice, "Home").await.unwrap();
    let ah = hub(&alice).await;
    ops::put_document(&ah, "Home", "Soup.md", "leek and potato\n", None)
        .await
        .unwrap();
    let v = ah.resolve("Home").await.unwrap();
    let vid = v.lock().await.id().to_string();
    note_shares::share_note(&alice, &v, "Soup", "carol", Role::Viewer, false)
        .await
        .unwrap();
    let mut mirror = Mirror::new(ah.clone(), &srv.path("alice-mirror"))
        .await
        .unwrap();
    mirror.reconcile_all(false).await.unwrap();
    let dir = mirror.vault_dir(&vid).unwrap().to_path_buf();
    std::fs::rename(dir.join("Soup.md"), dir.join("Winter soup.md")).unwrap();
    let r = mirror.reconcile_all(true).await.unwrap();
    assert_eq!(r.renamed, 1, "{r:?}");
    let shared = note_shares::shared_with_me(&carol).await.unwrap();
    assert_eq!(shared[0].title, "Winter soup");
}

/// The quick notes are a flat `Quick notes/` folder: new Markdown files
/// become quick notes (while there's room), folders in it aren't synced,
/// moving a file into a vault's folder moves the note, and deleting files
/// deletes the notes for good — a few at a time; more at once are held.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quick_notes_are_a_flat_folder_that_deletes_for_good() {
    use notekpr_core::protocol::{InstanceSettingsUpdate, Limit};
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Home").await.unwrap();
    let change = InstanceSettingsUpdate {
        quick_notes_per_user: Some(Limit::Max(6)),
        ..Default::default()
    };
    srv.state
        .instance
        .update(&srv.state.db, &change)
        .await
        .unwrap();
    let h = hub(&alice).await;
    let q = h.quick_notes().await.unwrap();
    let qid = q.lock().await.id().to_string();
    let home = h.resolve("Home").await.unwrap();
    let hid = home.lock().await.id().to_string();
    let root = srv.path("alice-mirror");
    let mut mirror = Mirror::new(h.clone(), &root).await.unwrap();
    mirror.reconcile_all(false).await.unwrap();
    let dir = mirror.vault_dir(&qid).unwrap().to_path_buf();
    assert_eq!(dir.file_name().unwrap(), "Quick notes");
    let home_dir = mirror.vault_dir(&hid).unwrap().to_path_buf();

    // Dropped files become quick notes; a folder in there doesn't.
    for i in 1..=6 {
        write(&dir.join(format!("idea {i}.md")), &format!("idea {i}\n"));
    }
    write(&dir.join("Later/kept.md"), "in a folder\n");
    write(&dir.join("idea 7.md"), "one too many\n");
    mirror.reconcile_all(true).await.unwrap();
    let six: Vec<String> = (1..=6).map(|i| format!("idea {i}")).collect();
    assert_eq!(names(&h).await, six, "full at 6, nothing from folders");
    // The extra file waits on disk.
    assert_eq!(read(&dir.join("idea 7.md")), "one too many\n");

    // Moving a file into a vault's folder moves the note; there's room now.
    std::fs::rename(dir.join("idea 1.md"), home_dir.join("idea 1.md")).unwrap();
    mirror.reconcile_all(true).await.unwrap();
    mirror.reconcile_all(true).await.unwrap();
    assert_eq!(
        note(&h, &hid, "idea 1.md").await.as_deref(),
        Some("idea 1\n")
    );
    let now: Vec<String> = names(&h).await;
    assert!(!now.contains(&"idea 1".to_string()), "{now:?}");
    assert!(now.contains(&"idea 7".to_string()), "{now:?}");
    // Deleted from the quick notes for good: nothing in their trash.
    std::fs::remove_file(dir.join("idea 2.md")).unwrap();
    mirror.reconcile_all(true).await.unwrap();
    assert!(!names(&h).await.contains(&"idea 2".to_string()));
    assert!(q.lock().await.trashed().is_empty());
    assert_eq!(note_count(&h, &qid).await, 5);

    // Many at once look like a sync tool gone wrong: held.
    for i in 3..=6 {
        std::fs::remove_file(dir.join(format!("idea {i}.md"))).unwrap();
    }
    let r = mirror.reconcile_all(true).await.unwrap();
    assert_eq!(r.held, 4);
    assert_eq!(note_count(&h, &qid).await, 5);
}

/// A file in `Quick notes/` goes up only once a quick note links to it; until
/// then it waits on disk.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quick_notes_folder_takes_files_only_for_their_notes() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let h = hub(&alice).await;
    let q = h.quick_notes().await.unwrap();
    let qid = q.lock().await.id().to_string();
    let root = srv.path("alice-mirror");
    let mut mirror = Mirror::new(h.clone(), &root).await.unwrap();
    mirror.reconcile_all(false).await.unwrap();
    let dir = mirror.vault_dir(&qid).unwrap().to_path_buf();
    let synced = |name: &'static str| {
        let q = q.clone();
        async move { q.lock().await.find(name).is_some() }
    };

    write(&dir.join("loose.pdf"), "a file of its own");
    mirror.reconcile_all(true).await.unwrap();
    assert!(!synced("loose.pdf").await);
    assert_eq!(read(&dir.join("loose.pdf")), "a file of its own");
    // A note and the picture it links to, in one look at the folder (the
    // picture comes first on disk).
    write(&dir.join("Receipt.md"), "Paid: ![](Photo.png)\n");
    write(&dir.join("Photo.png"), "a picture");
    mirror.ingest(&qid, false).await.unwrap();
    assert!(synced("Receipt.md").await && synced("Photo.png").await);
    assert!(!synced("loose.pdf").await);
    // Once linked, the waiting file goes up too.
    write(
        &dir.join("Receipt.md"),
        "Paid: ![](Photo.png) [[loose.pdf]]\n",
    );
    mirror.reconcile_all(true).await.unwrap();
    assert!(synced("loose.pdf").await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_folder_deleted_with_what_is_in_it_goes_to_the_trash() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Work").await.unwrap();
    let h = hub(&alice).await;
    let v = h.resolve("Work").await.unwrap();
    let vid = v.lock().await.id().to_string();
    for i in 0..4 {
        ops::put_document(&h, &vid, &format!("N{i}.md"), "x\n", None)
            .await
            .unwrap();
    }
    v.lock().await.create_folder("Board").unwrap();
    ops::put_document(&h, &vid, "Board/Notes.md", "notes\n", None)
        .await
        .unwrap();
    let root = srv.path("mirror");
    let mut m = Mirror::new(h.clone(), &root).await.unwrap();
    m.reconcile_all(true).await.unwrap();
    let dir = m.vault_dir(&vid).unwrap().to_path_buf();
    assert_eq!(read(&dir.join("Board/Notes.md")), "notes\n");

    std::fs::remove_dir_all(dir.join("Board")).unwrap();
    // Not yet final: the note may still turn up elsewhere (a move), so the
    // folder waits for it — and isn't made again on disk meanwhile.
    m.reconcile_all(false).await.unwrap();
    assert!(!dir.join("Board").exists());
    assert!(v.lock().await.is_folder("Board"));
    let r = m.reconcile_all(true).await.unwrap();
    assert_eq!((r.trashed, r.held), (2, 0), "{r:?}");
    assert!(!dir.join("Board").exists());
    let g = v.lock().await;
    assert!(g.find("Board/Notes.md").is_none());
    assert!(!g.is_folder("Board"));
}
