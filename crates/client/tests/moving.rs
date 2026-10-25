//! Moving a folder to another vault (`mv --to-vault`): everything in it,
//! and the files elsewhere its notes link to, then the originals to the
//! trash. Single notes: `quick.rs`.

mod common;

use clap::Parser;
use common::*;
use notekpr_client::cli::Cli;
use notekpr_client::commands;
use notekpr_client::hub::Hub;
use notekpr_client::ops;

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
