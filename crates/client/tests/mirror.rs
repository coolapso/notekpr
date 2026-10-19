//! The plaintext file mirror: two-way sync with a directory, external edits
//! racing remote edits, deletes and renames.

mod common;

use std::path::Path;
use std::time::Duration;

use common::*;
use coolpsonotes_client::mirror::Mirror;
use coolpsonotes_client::{manage, ops};
use coolpsonotes_core::protocol::Role;
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

    // A file written by an editor shows up for Alice.
    write(&dir.join("Ideas/new.md"), "from bob's editor\n");
    eventually(15, "alice sees the file bob created", || async {
        alice_hub.sync_vault(&vid).await.unwrap();
        note(&alice_hub, &vid, "Ideas/new.md").await.as_deref() == Some("from bob's editor\n")
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
        "from bob's editor\n",
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
    std::sync::Arc<coolpsonotes_client::hub::Hub>,
    String,
    std::sync::Arc<coolpsonotes_client::hub::Hub>,
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
    use coolpsonotes_core::doc::EntryKind;
    use coolpsonotes_core::protocol::INDEX_OBJECT_ID;
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
        let id = coolpsonotes_core::random_id();
        let u = g
            .replica
            .index
            .create_with(
                &id,
                &long,
                EntryKind::Note,
                coolpsonotes_core::now_ms() + 1000,
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

async fn note_count(h: &coolpsonotes_client::hub::Hub, vid: &str) -> usize {
    h.get(vid).unwrap().lock().await.notes().len()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vanished_folder_or_mass_deletion_is_held_not_trashed() {
    use coolpsonotes_client::mirror::MirrorOptions;
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
