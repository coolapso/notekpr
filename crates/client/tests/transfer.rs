//! Export / import round trips (directory and zip).

mod common;

use std::collections::BTreeMap;

use common::*;
use notekpr_client::hub::Hub;
use notekpr_client::{manage, ops, transfer};
use notekpr_core::doc::EntryKind;

async fn src_id(hub: &Hub) -> String {
    hub.resolve("Source")
        .await
        .unwrap()
        .lock()
        .await
        .id()
        .to_string()
}

async fn snapshot(hub: &Hub, vault: &str) -> BTreeMap<String, Vec<u8>> {
    let v = hub.resolve(vault).await.unwrap();
    let entries = v.lock().await.paths();
    let mut out = BTreeMap::new();
    for (p, e) in entries {
        let data = match e.kind {
            EntryKind::Note => v.lock().await.note_text(&e.id).into_bytes(),
            EntryKind::Attachment => hub.session.attachment(&v, &e.id).await.unwrap(),
            EntryKind::Drawing => {
                notekpr_draw::file::write_drawing(&v.lock().await.drawing(&e.id)).into_bytes()
            }
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
    ops::put_document(
        &h,
        "Source",
        "README.md",
        "# Home\nSee [[Projects/Plan]]\n",
        None,
    )
    .await
    .unwrap();
    ops::put_document(
        &h,
        "Source",
        "Projects/Plan.md",
        "---\ntags: [work]\n---\n# Plän ✓\n",
        None,
    )
    .await
    .unwrap();
    ops::put_document(&h, "Source", "Projects/Deep/Nested note.md", "deep\n", None)
        .await
        .unwrap();
    ops::put_attachment(
        &h,
        "Source",
        "assets/logo.png",
        &[0x89, b'P', b'N', b'G', 0, 1, 2, 255],
        None,
    )
    .await
    .unwrap();
    {
        let v = h.resolve("Source").await.unwrap();
        let mut g = v.lock().await;
        g.create_folder("Empty folder").unwrap();
        // A drawing: exported as a .drawing file, imported as a drawing.
        let file = include_str!("../../draw/tests/fixtures/example.excalidraw");
        let mut ids = notekpr_draw::restore::Ids::new(1);
        let scene = notekpr_draw::file::read_drawing(file, &mut ids)
            .unwrap()
            .scene;
        g.create_drawing("Projects/Architecture", &scene).unwrap();
        g.save().unwrap();
        // Search finds the text in it.
        let hits = g.search(&notekpr_core::search::parse_query("searchbase core"));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "Projects/Architecture.drawing");
    }
    h.local_changed(&src_id(&h).await, vec![], true).await;
    ops::put_document(&h, "Source", "Trashed.md", "gone", None)
        .await
        .unwrap();
    ops::delete_document(&h, "Source", "Trashed.md")
        .await
        .unwrap();
    let original = snapshot(&h, "Source").await;
    assert_eq!(original.len(), 6);
    assert!(original.contains_key("Projects/Architecture.drawing"));

    let src = h.resolve("Source").await.unwrap();
    let dir = srv.path("export-dir");
    let r = transfer::export(&h, &src, &dir, false).await.unwrap();
    assert_eq!(
        (r.notes, r.drawings, r.attachments, r.folders),
        (3, 1, 1, 1)
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("Projects/Plan.md")).unwrap(),
        "---\ntags: [work]\n---\n# Plän ✓\n"
    );
    assert!(dir.join("Empty folder").is_dir());
    assert!(
        !dir.join("Trashed.md").exists(),
        "trashed documents are not exported"
    );
    // Refuses to overwrite a non-empty directory.
    assert!(transfer::export(&h, &src, &dir, false).await.is_err());
    let zip = srv.path("export.zip");
    transfer::export(&h, &src, &zip, false).await.unwrap();

    let to_dir = h.resolve("From dir").await.unwrap();
    let r = transfer::import(&h, &to_dir, &dir, None, false, false)
        .await
        .unwrap();
    assert_eq!(
        (r.notes, r.drawings, r.attachments, r.folders),
        (3, 1, 1, 1),
        "{:?}",
        r.skipped
    );
    assert_eq!(snapshot(&h, "From dir").await, original);
    let kinds: Vec<EntryKind> = to_dir
        .lock()
        .await
        .paths()
        .values()
        .map(|e| e.kind)
        .collect();
    assert!(kinds.contains(&EntryKind::Drawing));
    let to_zip = h.resolve("From zip").await.unwrap();
    transfer::import(&h, &to_zip, &zip, None, false, false)
        .await
        .unwrap();
    assert_eq!(snapshot(&h, "From zip").await, original);

    // Re-importing is a no-op; changed files are merged into existing notes.
    let r = transfer::import(&h, &to_dir, &dir, None, false, false)
        .await
        .unwrap();
    assert_eq!((r.notes, r.attachments), (0, 0));
    std::fs::write(
        dir.join("README.md"),
        "# Home\nSee [[Projects/Plan]]\nedited\n",
    )
    .unwrap();
    let r = transfer::import(&h, &to_dir, &dir, None, false, false)
        .await
        .unwrap();
    assert_eq!(r.notes, 1);
    assert_eq!(
        note(&h, "From dir", "README.md").await.as_deref(),
        Some("# Home\nSee [[Projects/Plan]]\nedited\n")
    );

    // Import below a folder.
    let r = transfer::import(&h, &to_dir, &zip, Some("Imported"), false, false)
        .await
        .unwrap();
    assert_eq!(r.notes, 3);
    assert!(
        note(&h, "From dir", "Imported/Projects/Deep/Nested note.md")
            .await
            .is_some()
    );

    // Everything was pushed: a fresh client sees the imported vault.
    let other = hub(&reopen(&srv, "alice", "alice").await).await;
    assert_eq!(snapshot(&other, "From zip").await, original);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn import_skip_existing_and_dry_run_change_nothing_they_should_not() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Notes").await.unwrap();
    let h = hub(&alice).await;
    ops::put_document(&h, "Notes", "README.md", "mine\n", None)
        .await
        .unwrap();
    let src = srv.path("old-vault");
    std::fs::create_dir_all(src.join("Ideas")).unwrap();
    std::fs::write(src.join("readme.md"), "theirs\n").unwrap();
    std::fs::write(src.join("Ideas/New.md"), "new\n").unwrap();
    let v = h.resolve("Notes").await.unwrap();

    // A dry run reports and changes nothing.
    let r = transfer::import(&h, &v, &src, None, false, true)
        .await
        .unwrap();
    assert_eq!((r.notes, r.unchanged), (2, 0), "{:?}", r.skipped);
    assert_eq!(v.lock().await.notes().len(), 1);
    assert!(v.lock().await.replica.pending.is_empty());
    assert_eq!(
        note(&h, "Notes", "README.md").await.as_deref(),
        Some("mine\n")
    );

    // --skip-existing treats "readme.md" as the existing "README.md" (as a
    // write would): left alone, not merged.
    let r = transfer::import(&h, &v, &src, None, true, true)
        .await
        .unwrap();
    assert_eq!((r.notes, r.unchanged), (1, 1));
    let r = transfer::import(&h, &v, &src, None, true, false)
        .await
        .unwrap();
    assert_eq!((r.notes, r.unchanged), (1, 1));
    assert_eq!(
        note(&h, "Notes", "README.md").await.as_deref(),
        Some("mine\n")
    );
    assert_eq!(
        note(&h, "Notes", "Ideas/New.md").await.as_deref(),
        Some("new\n")
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exports_are_private_and_never_left_half_written() {
    use std::os::unix::fs::PermissionsExt;
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Private").await.unwrap();
    let h = hub(&alice).await;
    ops::put_document(&h, "Private", "Diary/Today.md", "secret\n", None)
        .await
        .unwrap();
    let v = h.resolve("Private").await.unwrap();
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

    let dir = srv.path("export");
    transfer::export(&h, &v, &dir, false).await.unwrap();
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join("Diary")), 0o700);
    assert_eq!(mode(&dir.join("Diary/Today.md")), 0o600);

    let zip = srv.path("export.zip");
    transfer::export(&h, &v, &zip, false).await.unwrap();
    assert_eq!(mode(&zip), 0o600);
    let mut a = zip::ZipArchive::new(std::fs::File::open(&zip).unwrap()).unwrap();
    let entry = a.by_name("Diary/Today.md").unwrap();
    assert_eq!(entry.unix_mode().map(|m| m & 0o777), Some(0o600));
    drop(entry);

    // A failing zip export (here: a directory is in the way of the final
    // name) leaves no partial plaintext archive behind.
    let blocked = srv.path("blocked.zip");
    std::fs::create_dir_all(blocked.join("inside")).unwrap();
    assert!(transfer::export(&h, &v, &blocked, true).await.is_err());
    assert!(!srv.path("blocked.zip.partial").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exports_carry_comments_that_imports_leave_out() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Source").await.unwrap();
    manage::create_vault(&alice, "Copy").await.unwrap();
    let h = hub(&alice).await;
    ops::put_document(&h, "Source", "Projects/Plan.md", "Ship in May\n", None)
        .await
        .unwrap();
    ops::put_document(&h, "Source", "Quiet.md", "no comments\n", None)
        .await
        .unwrap();
    let sent = ops::add_comment(&h, "Source", "Projects/Plan", "May", "Or June?")
        .await
        .unwrap();
    ops::comment_action(
        &h,
        "Source",
        "Projects/Plan",
        &sent.id,
        notekpr_client::vault::CommentAct::Reply("June".into()),
    )
    .await
    .unwrap();
    let src = h.resolve("Source").await.unwrap();
    let dir = srv.path("export");
    let r = transfer::export(&h, &src, &dir, false).await.unwrap();
    assert_eq!((r.notes, r.comments), (2, 1));
    let md = std::fs::read_to_string(dir.join("_comments/Projects/Plan.md")).unwrap();
    assert!(
        md.starts_with("# Comments on Plan\n\n## On “May”\n\n**Alice** · "),
        "{md}"
    );
    assert!(
        md.contains("> Or June?") && md.contains("**Alice** · reply") && md.contains("> June"),
        "{md}"
    );
    assert!(!dir.join("_comments/Quiet.md").exists());
    let zip = srv.path("export.zip");
    let r = transfer::export(&h, &src, &zip, false).await.unwrap();
    assert_eq!(r.comments, 1);

    // Importing an export brings the notes back, not the comments as notes.
    let copy = h.resolve("Copy").await.unwrap();
    for from in [&dir, &zip] {
        let r = transfer::import(&h, &copy, from, None, false, false)
            .await
            .unwrap();
        assert!(
            r.skipped.iter().any(|(p, _)| p == "_comments/"),
            "{:?}",
            r.skipped
        );
    }
    let paths: Vec<String> = copy.lock().await.paths().into_keys().collect();
    assert_eq!(paths, ["Projects/Plan.md", "Quiet.md"]);
}

async fn run(srv: &TestServer, who: &str, args: &[&str]) -> anyhow::Result<()> {
    use clap::Parser;
    let mut argv = vec!["notekpr", "-q", "--profile", who];
    argv.extend_from_slice(args);
    notekpr_client::commands::run_in(
        notekpr_client::cli::Cli::try_parse_from(argv)?,
        srv.dirs(who),
    )
    .await
}

/// Moving to another server: the vaults (but those left out) and the quick
/// notes exported as one .zip, imported there by a new account — named as
/// they were (a name taken gets a number), with folders, files and icons.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vaults_and_quick_notes_move_to_another_server() {
    let old = start().await;
    let alice = login(&old, "alice").await;
    manage::create_vault(&alice, "Family").await.unwrap();
    manage::create_vault(&alice, "Secret").await.unwrap();
    let h = hub(&alice).await;
    ops::put_document(&h, "Family", "Trips/2026.md", "Lisbon\n", None)
        .await
        .unwrap();
    ops::put_attachment(&h, "Family", "assets/map.png", &[1, 2, 3], None)
        .await
        .unwrap();
    {
        let v = h.resolve("Family").await.unwrap();
        let mut g = v.lock().await;
        g.create_folder("Empty").unwrap();
        g.set_icon("Trips/2026.md", Some("✈️")).unwrap();
        g.save().unwrap();
    }
    let family = h.resolve("Family").await.unwrap();
    manage::set_vault_icon(&alice, &family, Some("🏡"))
        .await
        .unwrap();
    ops::put_document(&h, "Secret", "Hidden.md", "stays\n", None)
        .await
        .unwrap();
    ops::create_quick_note(&h, "Call mum").await.unwrap();
    let shed =
        r#"{"type":"excalidraw","version":2,"elements":[{"id":"s","type":"ellipse","width":5}]}"#;
    ops::put_drawing(&h, "Drawings", "Plans/Shed.drawing", shed)
        .await
        .unwrap();
    for id in h.vault_ids() {
        h.push(&id).await.unwrap();
    }
    drop((h, family));
    let zip = old.path("notes.zip");
    let zip_arg = zip.to_str().unwrap();
    run(
        &old,
        "alice",
        &["export", "--out", zip_arg, "--skip", "secret"],
    )
    .await
    .unwrap();
    // A name that isn't a vault is refused, not silently ignored.
    let e = run(
        &old,
        "alice",
        &[
            "export",
            "--out",
            old.path("x.zip").to_str().unwrap(),
            "--skip",
            "Nope",
        ],
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("no vault 'Nope'"), "{e:#}");

    // Another server, where the new account has a "Family" already.
    let new = start().await;
    let bob = login(&new, "bob").await;
    manage::create_vault(&bob, "Family").await.unwrap();
    drop(bob);
    run(&new, "bob", &["import", zip_arg, "--dry-run"])
        .await
        .unwrap();
    let hb = hub(&reopen(&new, "bob", "bob").await).await;
    assert!(
        hb.resolve("Family 2").await.is_err(),
        "a dry run makes nothing"
    );
    drop(hb);
    run(&new, "bob", &["import", zip_arg]).await.unwrap();

    let hb = hub(&reopen(&new, "bob", "bob").await).await;
    assert_eq!(
        note(&hb, "Family 2", "Trips/2026.md").await.as_deref(),
        Some("Lisbon\n")
    );
    let moved = hb.resolve("Family 2").await.unwrap();
    {
        let g = moved.lock().await;
        assert_eq!(g.replica.meta.icon.as_deref(), Some("🏡"));
        assert!(g.is_folder("Empty"));
        let (_, e) = g.find("Trips/2026.md").unwrap();
        assert_eq!(e.icon.as_deref(), Some("✈️"));
    }
    let (_, map) = moved.lock().await.find("assets/map.png").unwrap();
    assert_eq!(
        hb.session.attachment(&moved, &map.id).await.unwrap(),
        [1, 2, 3]
    );
    assert!(
        hb.resolve("Secret").await.is_err(),
        "what was left out stays"
    );
    let quick = ops::list_quick_notes(&hb, 10).await.unwrap();
    let q = hb.quick_notes().await.unwrap();
    assert!(
        quick
            .notes
            .iter()
            .any(|n| q.try_lock().unwrap().note_text(&n.id) == "Call mum"),
        "the quick notes join that account's"
    );
    // The Drawings go into that account's Drawings, not a vault.
    let d = hb.drawings().await.unwrap();
    let shed = d.lock().await.find("Plans/Shed").map(|(_, e)| e.kind);
    assert_eq!(shed, Some(EntryKind::Drawing));
    assert!(hb.resolve("Drawings 2").await.is_err());
    drop((hb, q, moved, d));

    // The same as a directory, into a third account there.
    let dir = old.path("notes-dir");
    run(
        &old,
        "alice",
        &["export", "--out", dir.to_str().unwrap(), "--no-quick-notes"],
    )
    .await
    .unwrap();
    assert!(
        dir.join(".notekpr-export.json").is_file() && dir.join("Family/Trips/2026.md").is_file()
    );
    drop(login(&new, "carol").await);
    run(&new, "carol", &["import", dir.to_str().unwrap()])
        .await
        .unwrap();
    let hc = hub(&reopen(&new, "carol", "carol").await).await;
    assert_eq!(
        note(&hc, "Secret", "Hidden.md").await.as_deref(),
        Some("stays\n")
    );
    assert!(
        ops::list_quick_notes(&hc, 10)
            .await
            .map_or(true, |q| q.notes.is_empty()),
        "the quick notes were left out"
    );
    drop(hc);

    // Last, the account on the old server goes, with this profile.
    let old_id = alice.user_id.clone();
    drop(alice);
    run(&old, "alice", &["delete-account", "--yes"])
        .await
        .unwrap();
    let store = notekpr_client::profile::ProfileStore::new(&old.dirs("alice"), "alice").unwrap();
    assert!(store.load().unwrap().is_none(), "the profile is gone");
    let again = login(&old, "alice").await;
    assert_ne!(again.user_id, old_id, "signing in again is a new account");
    drop(again);

    // Something that isn't such an export: told how to import it instead.
    let plain = new.path("plain");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("a.md"), "a\n").unwrap();
    let e = run(&new, "bob", &["import", plain.to_str().unwrap()])
        .await
        .unwrap_err();
    assert!(e.to_string().contains("notekpr import <vault>"), "{e:#}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writing_a_drawing_file_makes_a_drawing() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Art").await.unwrap();
    let h = hub(&alice).await;
    let file = include_str!("../../draw/tests/fixtures/obsidian.excalidraw.md");
    let out = ops::put_drawing(&h, "Art", "Plans/Board", file)
        .await
        .unwrap();
    assert!(out.created);
    assert_eq!(out.path, "Plans/Board.drawing");
    let v = h.resolve("Art").await.unwrap();
    let (_, e) = v.lock().await.find("Plans/Board").unwrap();
    assert_eq!(e.kind, EntryKind::Drawing);
    assert_eq!(v.lock().await.drawing(&e.id).elements.len(), 24);
    // The same again changes nothing; not a drawing is refused.
    assert!(
        !ops::put_drawing(&h, "Art", "Plans/Board.drawing", file)
            .await
            .unwrap()
            .changed
    );
    assert!(
        ops::put_drawing(&h, "Art", "Other.drawing", "no")
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn importing_other_apps_drawings_makes_drawings() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Obsidian").await.unwrap();
    let h = hub(&alice).await;
    // An Obsidian vault: the plugin's drawing, Excalidraw's file, and a note
    // that only has the name.
    let dir = srv.path("obsidian-vault");
    std::fs::create_dir_all(dir.join("Excalidraw")).unwrap();
    std::fs::write(
        dir.join("Excalidraw/Concept.excalidraw.md"),
        include_str!("../../draw/tests/fixtures/obsidian.excalidraw.md"),
    )
    .unwrap();
    std::fs::write(
        dir.join("Board.excalidraw"),
        include_str!("../../draw/tests/fixtures/example.excalidraw"),
    )
    .unwrap();
    std::fs::write(dir.join("Just a name.excalidraw.md"), "# not a drawing\n").unwrap();
    let v = h.resolve("Obsidian").await.unwrap();
    let r = transfer::import(&h, &v, &dir, None, false, false)
        .await
        .unwrap();
    assert_eq!((r.notes, r.drawings), (1, 2), "{:?}", r.skipped);
    let paths = v.lock().await.paths();
    let kind = |p: &str| paths.get(p).map(|e| e.kind);
    assert_eq!(kind("Excalidraw/Concept.drawing"), Some(EntryKind::Drawing));
    assert_eq!(kind("Board.drawing"), Some(EntryKind::Drawing));
    assert_eq!(kind("Just a name.excalidraw.md"), Some(EntryKind::Note));
    assert_eq!(kind("Board.excalidraw"), None);
}

/// A picture in a drawing file becomes a file of the vault that the drawing
/// shows; written out (`cat`, exports), the drawing holds it inside again,
/// and an import makes it a file of that vault too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pictures_in_drawings_are_files_of_the_vault() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Art").await.unwrap();
    manage::create_vault(&alice, "Copy").await.unwrap();
    let h = hub(&alice).await;
    let png = notekpr_core::pictures::data_url("image/png", b"\x89PNG picture");
    let file = serde_json::json!({
        "type": "excalidraw", "version": 2,
        "elements": [{"id": "i", "type": "image", "x": 0, "y": 0, "width": 50, "height": 40,
            "fileId": "f1", "status": "saved", "scale": [1, 1]}],
        "files": {"f1": {"id": "f1", "mimeType": "image/png", "dataURL": png, "created": 5}},
    })
    .to_string();
    ops::put_drawing(&h, "Art", "Plans/Board", &file)
        .await
        .unwrap();
    let v = h.resolve("Art").await.unwrap();
    let (att, drawing) = {
        let g = v.lock().await;
        let (_, a) = g
            .find("attachments/Board.png")
            .expect("the picture, a file");
        assert_eq!(a.kind, EntryKind::Attachment);
        let (_, d) = g.find("Plans/Board").unwrap();
        let scene = g.drawing(&d.id);
        assert_eq!(scene.files["f1"]["attachment"], a.id.as_str());
        assert!(
            scene.files["f1"].get("dataURL").is_none(),
            "not in the drawing"
        );
        (a.id, d.id)
    };
    assert_eq!(
        h.session.attachment(&v, &att).await.unwrap(),
        b"\x89PNG picture"
    );
    // The same file again: no second copy of the picture.
    ops::put_drawing(&h, "Art", "Plans/Board", &file)
        .await
        .unwrap();
    assert!(v.lock().await.find("attachments/Board 2.png").is_none());
    // Written out, it's inside.
    let out = ops::drawing_file(&h, &v, &drawing).await;
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(json["files"]["f1"]["dataURL"], png.as_str());
    // Exported and imported elsewhere: a file there, shown from there.
    h.local_changed(&src_id_of(&h, "Art").await, vec![], true)
        .await;
    let dir = srv.path("art-export");
    transfer::export(&h, &v, &dir, false).await.unwrap();
    let to = h.resolve("Copy").await.unwrap();
    transfer::import(&h, &to, &dir, None, false, false)
        .await
        .unwrap();
    let g = to.lock().await;
    let (_, copy) = g.find("attachments/Board.png").expect("the picture there");
    let (_, d) = g.find("Plans/Board").unwrap();
    assert_eq!(g.drawing(&d.id).files["f1"]["attachment"], copy.id.as_str());
    assert_ne!(copy.id, att);
}

async fn src_id_of(hub: &Hub, vault: &str) -> String {
    hub.resolve(vault)
        .await
        .unwrap()
        .lock()
        .await
        .id()
        .to_string()
}
