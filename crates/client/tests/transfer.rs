//! Export / import round trips (directory and zip).

mod common;

use std::collections::BTreeMap;

use common::*;
use coolpsonotes_client::hub::Hub;
use coolpsonotes_client::{manage, ops, transfer};
use coolpsonotes_core::doc::EntryKind;

async fn snapshot(hub: &Hub, vault: &str) -> BTreeMap<String, Vec<u8>> {
    let v = hub.resolve(vault).await.unwrap();
    let entries = v.lock().await.paths();
    let mut out = BTreeMap::new();
    for (p, e) in entries {
        let data = match e.kind {
            EntryKind::Note => v.lock().await.note_text(&e.id).into_bytes(),
            EntryKind::Attachment => hub.session.attachment(&v, &e.id).await.unwrap(),
            EntryKind::Folder => b"<folder>".to_vec(),
        };
        out.insert(p, data);
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn export_import_round_trip_preserves_paths_and_content() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Source").await.unwrap();
    manage::create_vault(&alice, "From dir").await.unwrap();
    manage::create_vault(&alice, "From zip").await.unwrap();
    let h = hub(&alice).await;
    ops::put_document(&h, "Source", "README.md", "# Home\nSee [[Projects/Plan]]\n", None).await.unwrap();
    ops::put_document(&h, "Source", "Projects/Plan.md", "---\ntags: [work]\n---\n# Plän ✓\n", None).await.unwrap();
    ops::put_document(&h, "Source", "Projects/Deep/Nested note.md", "deep\n", None).await.unwrap();
    ops::put_attachment(&h, "Source", "assets/logo.png", &[0x89, b'P', b'N', b'G', 0, 1, 2, 255], None).await.unwrap();
    {
        let v = h.resolve("Source").await.unwrap();
        let mut g = v.lock().await;
        g.create_folder("Empty folder").unwrap();
        g.save().unwrap();
    }
    ops::put_document(&h, "Source", "Trashed.md", "gone", None).await.unwrap();
    ops::delete_document(&h, "Source", "Trashed.md").await.unwrap();
    let original = snapshot(&h, "Source").await;
    assert_eq!(original.len(), 5);

    let src = h.resolve("Source").await.unwrap();
    let dir = srv.path("export-dir");
    let r = transfer::export(&h, &src, &dir, false).await.unwrap();
    assert_eq!((r.notes, r.attachments, r.folders), (3, 1, 1));
    assert_eq!(std::fs::read_to_string(dir.join("Projects/Plan.md")).unwrap(), "---\ntags: [work]\n---\n# Plän ✓\n");
    assert!(dir.join("Empty folder").is_dir());
    assert!(!dir.join("Trashed.md").exists(), "trashed documents are not exported");
    // Refuses to overwrite a non-empty directory.
    assert!(transfer::export(&h, &src, &dir, false).await.is_err());
    let zip = srv.path("export.zip");
    transfer::export(&h, &src, &zip, false).await.unwrap();

    let to_dir = h.resolve("From dir").await.unwrap();
    let r = transfer::import(&h, &to_dir, &dir, None, false).await.unwrap();
    assert_eq!((r.notes, r.attachments, r.folders), (3, 1, 1), "{:?}", r.skipped);
    assert_eq!(snapshot(&h, "From dir").await, original);
    let to_zip = h.resolve("From zip").await.unwrap();
    transfer::import(&h, &to_zip, &zip, None, false).await.unwrap();
    assert_eq!(snapshot(&h, "From zip").await, original);

    // Re-importing is a no-op; changed files are merged into existing notes.
    let r = transfer::import(&h, &to_dir, &dir, None, false).await.unwrap();
    assert_eq!((r.notes, r.attachments), (0, 0));
    std::fs::write(dir.join("README.md"), "# Home\nSee [[Projects/Plan]]\nedited\n").unwrap();
    let r = transfer::import(&h, &to_dir, &dir, None, false).await.unwrap();
    assert_eq!(r.notes, 1);
    assert_eq!(note(&h, "From dir", "README.md").await.as_deref(), Some("# Home\nSee [[Projects/Plan]]\nedited\n"));

    // Import below a folder.
    let r = transfer::import(&h, &to_dir, &zip, Some("Imported"), false).await.unwrap();
    assert_eq!(r.notes, 3);
    assert!(note(&h, "From dir", "Imported/Projects/Deep/Nested note.md").await.is_some());

    // Everything was pushed: a fresh client sees the imported vault.
    let other = hub(&reopen(&srv, "alice", "alice").await).await;
    assert_eq!(snapshot(&other, "From zip").await, original);
}
