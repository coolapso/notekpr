//! Moving a folder to another vault (`mv --to-vault`): everything in it,
//! and the files elsewhere its notes link to, then the originals to the
//! trash; a drawing, replacing its original. Single notes: `quick.rs`.

mod common;

use clap::Parser;
use common::*;
use notekpr_cli::cli::Cli;
use notekpr_cli::commands;
use notekpr_cli::hub::Hub;
use notekpr_cli::ops;
use notekpr_core::doc::EntryKind;
use notekpr_core::protocol::{InstanceSettingsUpdate, Limit};

async fn run(srv: &TestServer, who: &str, args: &[&str]) -> anyhow::Result<()> {
    let mut argv = vec!["notekpr", "-q", "--profile", who];
    argv.extend_from_slice(args);
    commands::run_in(Cli::try_parse_from(argv)?, srv.dirs(who)).await
}

async fn file(h: &Hub, vault: &str, path: &str) -> Option<Vec<u8>> {
    let v = h.resolve(vault).await.unwrap();
    let g = v.lock().await;
    let (_, e) = g.find(path)?;
    g.cached_attachment(&e.id).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_folder_moves_to_another_vault_with_everything_in_it() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    for v in ["Home", "Work"] {
        run(&srv, "alice", &["vault", "create", v]).await.unwrap();
    }
    let h = hub(&alice).await;
    let docs = [
        (
            "Home",
            "Projects/Garden/Beds.md",
            "Beds\n\n![[photo.png]] ![[plan.pdf]]",
        ),
        ("Home", "Projects/Roadmap.md", "[[Beds]] and ![[logo.png]]"),
        ("Home", "Other.md", "uses ![[logo.png]]"),
        ("Work", "Archive/Projects/Old.md", "already there"),
    ];
    for (v, p, t) in docs {
        ops::put_document(&h, v, p, t, None).await.unwrap();
    }
    for (p, data) in [
        ("Projects/Garden/photo.png", &b"photo"[..]),
        ("attachments/plan.pdf", b"plan"),
        ("attachments/logo.png", b"logo"),
    ] {
        ops::put_attachment(&h, "Home", p, data, None)
            .await
            .unwrap();
    }
    drop(h);
    run(&srv, "alice", &["mkdir", "Home", "Projects/Empty"])
        .await
        .unwrap();
    run(&srv, "alice", &["icon", "Home", "Projects/Garden", "🌱"])
        .await
        .unwrap();
    // A file moves only with the notes linking to it.
    let err = run(
        &srv,
        "alice",
        &[
            "mv",
            "Home",
            "attachments/logo.png",
            "/",
            "--to-vault",
            "Work",
        ],
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("is a file"), "{err}");

    // Archive/Projects is taken there: the copy is "Projects 2".
    run(
        &srv,
        "alice",
        &["mv", "Home", "Projects", "Archive/", "--to-vault", "Work"],
    )
    .await
    .unwrap();
    let h = hub(&reopen(&srv, "alice", "alice").await).await;
    assert_eq!(
        note(&h, "Work", "Archive/Projects 2/Garden/Beds.md")
            .await
            .as_deref(),
        Some("Beds\n\n![[photo.png]] ![[plan.pdf]]")
    );
    assert_eq!(
        note(&h, "Work", "Archive/Projects 2/Roadmap.md")
            .await
            .as_deref(),
        Some("[[Beds]] and ![[logo.png]]")
    );
    // The files in it keep their places; those from elsewhere go to its top.
    for (p, data) in [
        ("Archive/Projects 2/Garden/photo.png", &b"photo"[..]),
        ("Archive/Projects 2/plan.pdf", b"plan"),
        ("Archive/Projects 2/logo.png", b"logo"),
    ] {
        assert_eq!(file(&h, "Work", p).await.as_deref(), Some(data), "{p}");
    }
    {
        let work = h.resolve("Work").await.unwrap();
        let g = work.lock().await;
        assert!(g.is_folder("Archive/Projects 2/Empty"), "empty folders too");
        assert_eq!(
            g.paths()["Archive/Projects 2/Garden"].icon.as_deref(),
            Some("🌱"),
            "with their icons"
        );
        assert!(g.find("Archive/Projects/Old.md").is_some());
    }
    // The originals are in the trash, but for the logo another note uses.
    let home = h.resolve("Home").await.unwrap();
    let g = home.lock().await;
    let trashed: Vec<String> = g.trashed().into_iter().map(|e| e.path).collect();
    for p in [
        "Projects/Garden/Beds.md",
        "Projects/Roadmap.md",
        "Projects/Garden/photo.png",
        "Projects/Garden",
        "Projects/Empty",
        "attachments/plan.pdf",
    ] {
        assert!(trashed.iter().any(|t| t == p), "{p}: {trashed:?}");
    }
    assert!(!g.is_folder("Projects"));
    assert!(g.find("attachments/logo.png").is_some());
    assert!(g.find("Other.md").is_some());
}

/// Each vault holds so many drawings: a drawing moves to another vault only
/// if there's room there, and nothing is copied otherwise.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_drawing_moves_only_to_a_vault_with_room() {
    let srv = start().await;
    srv.state
        .instance
        .update(
            &srv.state.db,
            &InstanceSettingsUpdate {
                drawings_per_vault: Some(Limit::Max(1)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let alice = login(&srv, "alice").await;
    for v in ["Home", "Work"] {
        run(&srv, "alice", &["vault", "create", v]).await.unwrap();
    }
    let file = include_str!("../../draw/tests/fixtures/example.excalidraw");
    let scene = notekpr_draw::file::read_drawing(file, &mut notekpr_draw::restore::Ids::new(1))
        .unwrap()
        .scene;
    let h = hub(&alice).await;
    let mut made = Vec::new();
    for (vault, name) in [("Home", "Sketch"), ("Work", "Other")] {
        let v = h.resolve(vault).await.unwrap();
        let mut g = v.lock().await;
        assert_eq!(g.drawing_limit, Limit::Max(1));
        let id = g.create_drawing(name, &scene).unwrap().id;
        g.save().unwrap();
        made.push((g.id().to_string(), id));
    }
    for (v, id) in &made {
        h.local_changed(v, vec![id.clone()], true).await;
    }
    // Full: no other one here.
    {
        let v = h.resolve("Home").await.unwrap();
        let mut g = v.lock().await;
        let e = g.create_drawing("Third", &scene).unwrap_err();
        assert!(e.to_string().contains("1 drawing"), "{e}");
    }
    drop(h);

    // Work is full too: the move is refused, nothing is copied.
    let e = run(
        &srv,
        "alice",
        &["mv", "Home", "Sketch.drawing", "/", "--to-vault", "Work"],
    )
    .await
    .unwrap_err();
    assert!(format!("{e:#}").contains("no room"), "{e:#}");
    {
        let h = hub(&reopen(&srv, "alice", "alice").await).await;
        let work = h.resolve("Work").await.unwrap();
        assert!(work.lock().await.find("Sketch.drawing").is_none());
        let home = h.resolve("Home").await.unwrap();
        assert!(home.lock().await.find("Sketch.drawing").is_some());
    }

    // With room there, it moves; the original goes to the trash.
    let work = &made[1].0;
    srv.state
        .instance
        .update(
            &srv.state.db,
            &InstanceSettingsUpdate {
                drawings_per_vault: Some(Limit::Max(2)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    run(
        &srv,
        "alice",
        &["mv", "Home", "Sketch.drawing", "/", "--to-vault", "Work"],
    )
    .await
    .unwrap();
    let h = hub(&reopen(&srv, "alice", "alice").await).await;
    {
        let w = h.resolve(work).await.unwrap();
        let g = w.lock().await;
        let (_, e) = g.find("Sketch.drawing").expect("the copy");
        assert_eq!(e.kind, EntryKind::Drawing);
        assert_eq!(g.drawing(&e.id).elements.len(), scene.elements.len());
        assert_eq!(g.drawing_count(), 2);
    }
    let home = h.resolve("Home").await.unwrap();
    let trashed: Vec<String> = home
        .lock()
        .await
        .trashed()
        .into_iter()
        .map(|e| e.path)
        .collect();
    assert_eq!(trashed, ["Sketch.drawing"]);
}

/// A drawing moved to another vault takes its pictures along; the copy
/// shows the copies.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_drawing_moves_with_its_pictures() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    notekpr_cli::manage::create_vault(&alice, "Home")
        .await
        .unwrap();
    notekpr_cli::manage::create_vault(&alice, "Work")
        .await
        .unwrap();
    let h = hub(&alice).await;
    let png = notekpr_core::pictures::data_url("image/png", b"pixels");
    let board = serde_json::json!({
        "type": "excalidraw", "version": 2,
        "elements": [{"id": "i", "type": "image", "x": 0, "y": 0, "width": 5, "height": 5,
            "fileId": "f1", "status": "saved"}],
        "files": {"f1": {"id": "f1", "mimeType": "image/png", "dataURL": png}},
    })
    .to_string();
    ops::put_drawing(&h, "Home", "Board", &board).await.unwrap();
    for id in h.vault_ids() {
        h.push(&id).await.unwrap();
    }
    drop(h);
    run(
        &srv,
        "alice",
        &["mv", "Home", "Board.drawing", "/", "--to-vault", "Work"],
    )
    .await
    .unwrap();
    let h = hub(&reopen(&srv, "alice", "alice").await).await;
    let work = h.resolve("Work").await.unwrap();
    let (pic, scene) = {
        let g = work.lock().await;
        let (_, pic) = g.find("Board.png").expect("the picture came along");
        let (_, d) = g.find("Board.drawing").expect("the copy");
        (pic.id, g.drawing(&d.id))
    };
    assert_eq!(scene.files["f1"]["attachment"], pic.as_str());
    assert_eq!(
        file(&h, "Work", "Board.png").await.unwrap_or_else(Vec::new),
        b"pixels"
    );
    let home = h.resolve("Home").await.unwrap();
    assert!(
        home.lock().await.find("attachments/Board.png").is_none(),
        "moved, not copied"
    );
}
