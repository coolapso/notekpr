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
    manage::share(&alice, &v, "bob", Role::Editor, false).await.unwrap();
    ops::put_document(&alice_hub, &vid, "Notes/Plan.md", "alpha\nbeta\n", None).await.unwrap();

    // ---- live daemon: watcher + websocket
    let mirror_root = srv.path("bob-mirror");
    let bob_live = live(hub(&bob).await).await;
    let mut mirror = Mirror::new(bob_live.hub.clone(), &mirror_root).await.unwrap();
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
    ops::put_document(&alice_hub, &vid, "Notes/Plan.md", "alpha\nbeta\ngamma\n", None).await.unwrap();
    eventually(15, "bob's file receives alice's edit", || async { read(&plan) == "alpha\nbeta\ngamma\n" }).await;
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
    ops::put_document(&alice_hub, &vid, "Notes/Plan.md", "ALPHA\nbeta\ngamma\n", None).await.unwrap();
    bob_hub.sync_vault(&vid).await.unwrap();
    mirror.reconcile_all(false).await.unwrap();
    assert_eq!(read(&plan), "ALPHA\nbeta\ngamma\n");
    tokio::time::sleep(Duration::from_millis(20)).await;
    write(&plan, &format!("{v1}delta (bob)\n"));
    mirror.reconcile_all(true).await.unwrap();
    let both = "ALPHA\nbeta\ngamma\ndelta (bob)\n";
    assert_eq!(read(&plan), both, "the stale save must not revert the remote edit");
    alice_hub.sync_vault(&vid).await.unwrap();
    assert_eq!(note(&alice_hub, &vid, "Notes/Plan.md").await.as_deref(), Some(both));

    // Race 2: a remote edit arrives while a local edit is not ingested yet.
    write(&plan, &format!("{both}epsilon (bob)\n"));
    ops::put_document(&alice_hub, &vid, "Notes/Plan.md", "ALPHA\nBETA (alice)\ngamma\ndelta (bob)\n", None)
        .await
        .unwrap();
    bob_hub.sync_vault(&vid).await.unwrap();
    mirror.reconcile_all(true).await.unwrap();
    let all = "ALPHA\nBETA (alice)\ngamma\ndelta (bob)\nepsilon (bob)\n";
    assert_eq!(read(&plan), all);
    alice_hub.sync_vault(&vid).await.unwrap();
    assert_eq!(note(&alice_hub, &vid, "Notes/Plan.md").await.as_deref(), Some(all));

    // Deleting a file moves the entry to the trash (restorable).
    let new_id = bob_hub.get(&vid).unwrap().lock().await.find("Ideas/new.md").unwrap().1.id;
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
    assert_eq!(read(&dir.join("Ideas/new.md")), "from bob's editor\n", "restored note is written back");

    // Renaming a file moves the entry (same id, same content).
    let plan_id = bob_hub.get(&vid).unwrap().lock().await.find("Notes/Plan.md").unwrap().1.id;
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
    ops::move_document(&alice_hub, &vid, "Archive/Old plan.md", "Plan.md", false).await.unwrap();
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
    manage::share(&alice, &v, "bob", Role::Editor, false).await.unwrap();

    // Bob is offline (uploads deferred) while both create the same path.
    let bob_hub = hub(&bob).await;
    bob_hub.set_push_now(false);
    let root = srv.path("bob-mirror");
    let mut mirror = Mirror::new(bob_hub.clone(), &root).await.unwrap();
    mirror.reconcile_all(true).await.unwrap();
    let dir = mirror.vault_dir(&vid).unwrap().to_path_buf();
    write(&dir.join("Idea.md"), "bob's idea\n");
    mirror.reconcile_all(true).await.unwrap();
    ops::put_document(&alice_hub, &vid, "Idea.md", "alice's idea\n", None).await.unwrap();

    // Reconnect: both entries exist; the older keeps the name.
    bob_hub.set_push_now(true);
    bob_hub.sync_vault(&vid).await.unwrap();
    mirror.reconcile_all(true).await.unwrap();
    alice_hub.sync_vault(&vid).await.unwrap();
    let mut texts = vec![read(&dir.join("Idea.md")), read(&dir.join("Idea 2.md"))];
    texts.sort();
    assert_eq!(texts, vec!["alice's idea\n".to_string(), "bob's idea\n".to_string()]);
    let a_paths: Vec<String> = v.lock().await.paths().into_keys().collect();
    assert_eq!(a_paths, vec!["Idea 2.md".to_string(), "Idea.md".to_string()]);

    // Editing the suffixed file edits that entry; nothing is renamed in the shared state.
    let second = read(&dir.join("Idea 2.md"));
    write(&dir.join("Idea 2.md"), &format!("{second}more\n"));
    mirror.reconcile_all(true).await.unwrap();
    alice_hub.sync_vault(&vid).await.unwrap();
    assert_eq!(note(&alice_hub, &vid, "Idea 2.md").await.unwrap(), format!("{second}more\n"));
    let g = v.lock().await;
    assert!(g.replica.index.entries().iter().all(|e| e.path == "Idea.md"));
}
