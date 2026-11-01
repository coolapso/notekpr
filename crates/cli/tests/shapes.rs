//! `notekpr shapes`: the saved shapes (Excalidraw's library), kept in the
//! person's Personal storage (their quick notes' index) and synced between
//! their devices.

mod common;

use clap::Parser;
use common::*;
use notekpr_cli::account::{self, LoginOptions};
use notekpr_cli::cli::Cli;
use notekpr_cli::commands;
use notekpr_cli::ops;
use notekpr_core::protocol::{InstanceSettingsUpdate, Limit};
use serde_json::json;

/// Run a command line as alice on one of her devices (`dirs`).
async fn run(srv: &TestServer, dirs: &str, args: &[&str]) -> anyhow::Result<()> {
    let mut argv = vec!["notekpr", "-q", "--profile", "alice"];
    argv.extend_from_slice(args);
    commands::run_in(Cli::try_parse_from(argv)?, srv.dirs(dirs)).await
}

/// Alice with two devices: `alice-a`, and `alice-b` (unlocked with her
/// recovery key).
async fn two_devices(srv: &TestServer) {
    let login = |device: &str, recovery_key: Option<String>| LoginOptions {
        server: srv.url.clone(),
        dev_user: Some("alice".into()),
        device_name: Some(device.into()),
        recovery_key,
        approval_timeout: Some(std::time::Duration::from_secs(1)),
        ..Default::default()
    };
    let first = account::login(&srv.dirs("alice-a"), "alice", login("a", None))
        .await
        .unwrap();
    account::login(
        &srv.dirs("alice-b"),
        "alice",
        login("b", first.recovery_key),
    )
    .await
    .unwrap();
}

/// An `.excalidrawlib` file with one item per `(item id, shape, text)`.
fn library_file(items: &[(&str, &str, &str)]) -> String {
    let items: Vec<_> = items
        .iter()
        .enumerate()
        .map(|(n, (id, shape, text))| {
            json!({
                "id": id,
                "status": "unpublished",
                "created": 1_700_000_000_000i64 + n as i64,
                "name": format!("{shape} {id}"),
                "elements": [
                    {"id": format!("{id}-shape"), "type": shape, "x": 0, "y": 0, "width": 80, "height": 40,
                     "versionNonce": 7},
                    {"id": format!("{id}-text"), "type": "text", "x": 0, "y": 50, "width": 80, "height": 20,
                     "text": text, "originalText": text, "versionNonce": 8},
                ],
            })
        })
        .collect();
    json!({"type": "excalidrawlib", "version": 2, "source": "https://excalidraw.com", "libraryItems": items})
        .to_string()
}

async fn library_ids(srv: &TestServer, dirs: &str) -> Vec<String> {
    let h = hub(&reopen(srv, dirs, "alice").await).await;
    ops::library(&h).await.into_iter().map(|i| i.id).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_library_is_imported_listed_and_exported() {
    // Drawings may not even be made here: saved shapes take no drawing's
    // place.
    let srv = start().await;
    let change = InstanceSettingsUpdate {
        drawings_per_vault: Some(Limit::Max(0)),
        ..Default::default()
    };
    srv.state
        .instance
        .update(&srv.state.db, &change)
        .await
        .unwrap();
    two_devices(&srv).await;
    run(&srv, "alice-a", &["shapes", "ls"]).await.unwrap();
    assert!(library_ids(&srv, "alice-a").await.is_empty());

    let file = srv.path("shapes.excalidrawlib");
    std::fs::write(
        &file,
        library_file(&[("box", "rectangle", "A box"), ("blob", "ellipse", "A blob")]),
    )
    .unwrap();
    let f = file.to_str().unwrap();
    run(&srv, "alice-a", &["shapes", "import", f])
        .await
        .unwrap();
    // Newest first.
    assert_eq!(library_ids(&srv, "alice-a").await, ["blob", "box"]);
    // As the command lists them, on her other device too.
    for device in ["alice-a", "alice-b"] {
        let (r, out) =
            notekpr_cli::ui::capture(None, true, run(&srv, device, &["shapes", "ls", "--json"]))
                .await;
        r.unwrap();
        let listed: Vec<serde_json::Value> = serde_json::from_str(&out.stdout).unwrap();
        assert_eq!(listed.len(), 2, "{device}: {}", out.stdout);
    }
    // Twice: nothing more.
    run(&srv, "alice-a", &["shapes", "import", f])
        .await
        .unwrap();
    let h = hub(&reopen(&srv, "alice-a", "alice").await).await;
    let listed = ops::list_library(&h).await;
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[1].name.as_deref(), Some("rectangle box"));
    assert_eq!(listed[1].elements, ["rectangle", "text"]);
    let again = ops::import_library(&h, &std::fs::read_to_string(&file).unwrap())
        .await
        .unwrap();
    assert_eq!((again.added, again.already), (0, 2));
    drop(h);

    // What isn't a library is refused.
    let note = srv.path("note.md");
    std::fs::write(&note, "# Hello").unwrap();
    let err = run(
        &srv,
        "alice-a",
        &["shapes", "import", note.to_str().unwrap()],
    )
    .await
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("not an Excalidraw library"),
        "{err:#}"
    );

    // Exported, Excalidraw's file holds the same items.
    let out = srv.path("mine.excalidrawlib");
    let o = out.to_str().unwrap();
    run(&srv, "alice-a", &["shapes", "export", "--out", o])
        .await
        .unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    let back =
        notekpr_draw::file::read_library(&text, 0.0, &mut notekpr_draw::restore::Ids::new(1))
            .unwrap();
    let h = hub(&reopen(&srv, "alice-a", "alice").await).await;
    assert_eq!(back, ops::library(&h).await);
    assert!(back[1].elements.iter().any(|e| e.text() == "A box"));
    drop(h);
    let err = run(&srv, "alice-a", &["shapes", "export", "--out", o])
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("--force"), "{err:#}");
    run(
        &srv,
        "alice-a",
        &["shapes", "export", "--out", o, "--force"],
    )
    .await
    .unwrap();

    // Her other device has it from the server (where it is encrypted).
    assert_eq!(library_ids(&srv, "alice-b").await, ["blob", "box"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_devices_adding_at_once_keep_both() {
    let srv = start().await;
    two_devices(&srv).await;
    // Both open before either adds: neither has seen the other's item.
    let a = hub(&reopen(&srv, "alice-a", "alice").await).await;
    let b = hub(&reopen(&srv, "alice-b", "alice").await).await;
    a.ensure_quick_notes().await.unwrap();
    b.ensure_quick_notes().await.unwrap();
    let ra = ops::import_library(&a, &library_file(&[("from-a", "rectangle", "A")]))
        .await
        .unwrap();
    let rb = ops::import_library(&b, &library_file(&[("from-b", "diamond", "B")]))
        .await
        .unwrap();
    assert_eq!((ra.added, rb.added), (1, 1));
    assert_eq!((ra.pending_changes, rb.pending_changes), (0, 0));
    drop((a, b));
    for d in ["alice-a", "alice-b"] {
        let mut ids = library_ids(&srv, d).await;
        ids.sort();
        assert_eq!(ids, ["from-a", "from-b"], "{d}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_item_too_large_to_sync_is_left_out() {
    // Updates of at most 8 KB: one local change carries at most 4 KB.
    let srv = start_with(|c| c.max_update_kb = 8).await;
    two_devices(&srv).await;
    let file = srv.path("big.excalidrawlib");
    std::fs::write(
        &file,
        library_file(&[
            ("small", "rectangle", "fits"),
            ("big", "rectangle", &"x".repeat(6000)),
        ]),
    )
    .unwrap();
    let h = hub(&reopen(&srv, "alice-a", "alice").await).await;
    let r = ops::import_library(&h, &std::fs::read_to_string(&file).unwrap())
        .await
        .unwrap();
    assert_eq!((r.added, r.too_large, r.pending_changes), (1, 1, 0));
    drop(h);
    assert_eq!(library_ids(&srv, "alice-a").await, ["small"]);
}

/// Shapes named, removed and exported by their ids or the start of them
/// (as `shapes ls` shows the first 8 characters).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shapes_are_renamed_removed_and_exported_by_id() {
    let srv = start().await;
    two_devices(&srv).await;
    let file = srv.path("shapes.excalidrawlib");
    std::fs::write(
        &file,
        library_file(&[
            ("bed-0001", "rectangle", "Bed"),
            ("bench-01", "ellipse", "Bench"),
            ("stone-01", "diamond", "Stone"),
            ("stone-02", "diamond", "Stone 2"),
        ]),
    )
    .unwrap();
    run(
        &srv,
        "alice-a",
        &["shapes", "import", file.to_str().unwrap()],
    )
    .await
    .unwrap();

    // Renamed, by the start of its id; "" takes the name away.
    run(&srv, "alice-a", &["shapes", "rename", "bed", "Raised bed"])
        .await
        .unwrap();
    run(&srv, "alice-a", &["shapes", "rename", "bench-01", ""])
        .await
        .unwrap();
    let names = |listed: Vec<ops::LibraryEntry>| -> Vec<(String, Option<String>)> {
        listed.into_iter().map(|i| (i.id, i.name)).collect()
    };
    let h = hub(&reopen(&srv, "alice-a", "alice").await).await;
    assert_eq!(
        names(ops::list_library(&h).await),
        [
            ("stone-02".into(), Some("diamond stone-02".into())),
            ("stone-01".into(), Some("diamond stone-01".into())),
            ("bench-01".into(), None),
            ("bed-0001".into(), Some("Raised bed".into())),
        ]
    );
    drop(h);
    // A start of several ids, or of none, names nothing.
    let err = run(&srv, "alice-a", &["shapes", "rename", "stone", "x"])
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("several"), "{err:#}");
    let err = run(&srv, "alice-a", &["shapes", "rm", "nothing"])
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("no saved shape"), "{err:#}");

    // Exported: those named, or all.
    let out = srv.path("some.excalidrawlib");
    let o = out.to_str().unwrap();
    run(
        &srv,
        "alice-a",
        &["shapes", "export", "bed", "stone-02", "--out", o],
    )
    .await
    .unwrap();
    let back = notekpr_draw::file::read_library(
        &std::fs::read_to_string(&out).unwrap(),
        0.0,
        &mut notekpr_draw::restore::Ids::new(1),
    )
    .unwrap();
    let ids: Vec<&str> = back.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["stone-02", "bed-0001"], "newest first");
    assert_eq!(back[1].name.as_deref(), Some("Raised bed"));
    let err = run(&srv, "alice-a", &["shapes", "export", "stone"])
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("several"), "{err:#}");

    // Removed, several at once.
    run(&srv, "alice-a", &["shapes", "rm", "stone-01", "bench"])
        .await
        .unwrap();
    assert_eq!(library_ids(&srv, "alice-a").await, ["stone-02", "bed-0001"]);
    // On her other device too, with the new name.
    let h = hub(&reopen(&srv, "alice-b", "alice").await).await;
    assert_eq!(
        names(ops::list_library(&h).await),
        [
            ("stone-02".into(), Some("diamond stone-02".into())),
            ("bed-0001".into(), Some("Raised bed".into())),
        ]
    );
}
