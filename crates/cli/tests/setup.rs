//! `notekpr setup` against an in-process server: the notes already in a
//! folder moved or copied into a vault, a first vault, and nothing changed
//! that it wasn't told to change.

mod common;

use std::path::Path;

use clap::Parser;
use common::*;
use notekpr_cli::cli::Cli;
use notekpr_cli::profile::ProfileStore;
use notekpr_cli::{commands, manage, ops};
use notekpr_core::protocol::Role;

async fn run(srv: &TestServer, who: &str, args: &[&str]) -> anyhow::Result<()> {
    let mut argv = vec!["notekpr", "-q", "--profile", who];
    argv.extend_from_slice(args);
    commands::run_in(Cli::try_parse_from(argv)?, srv.dirs(who)).await
}

/// `setup` for `who` on the test server, with these options.
async fn setup(srv: &TestServer, who: &str, args: &[&str]) -> anyhow::Result<()> {
    let mut argv = vec![
        "setup",
        "--server",
        &srv.url,
        "--dev-user",
        who,
        "--no-service",
    ];
    argv.extend_from_slice(args);
    run(srv, who, &argv).await
}

fn write(p: &Path, s: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

/// The files below `dir` with their content, the vault's marker left out.
fn tree(dir: &Path) -> Vec<(String, String)> {
    let mut out: Vec<_> = walkdir::WalkDir::new(dir)
        .into_iter()
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().is_file() && e.file_name() != ".notekpr-vault")
        .map(|e| {
            (
                e.path()
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                std::fs::read_to_string(e.path()).unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

/// An Obsidian vault, as people have them.
fn obsidian_vault(dir: &Path) {
    write(&dir.join("Daily/2026-11-01.md"), "# Today\n");
    write(&dir.join("Blog/post.md"), "A post with ![[img.png]]\n");
    write(&dir.join("img.png"), "png bytes");
    write(
        &dir.join("Excalidraw/scene.excalidraw.md"),
        "---\nexcalidraw-plugin: parsed\n---\n# Drawing\n",
    );
    write(&dir.join(".obsidian/app.json"), "{}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn setup_moves_the_notes_in_a_folder_into_a_vault() {
    let srv = start().await;
    let notes = srv.path("Notes");
    obsidian_vault(&notes);
    let before = tree(&notes);
    let dir = notes.to_str().unwrap();
    setup(
        &srv,
        "alice",
        &["--dir", dir, "--move-in", "--plaintext-ok"],
    )
    .await
    .unwrap();

    // Everything is in the vault's folder, as it was, Obsidian's settings too.
    let vault_dir = notes.join("My notes");
    assert_eq!(tree(&vault_dir), before);
    assert!(vault_dir.join(".notekpr-vault").is_file());
    for gone in ["Daily", "Blog", "img.png", ".obsidian"] {
        assert!(!notes.join(gone).exists(), "{gone} moved");
    }
    let profile = ProfileStore::new(&srv.dirs("alice"), "alice")
        .unwrap()
        .require()
        .unwrap();
    assert_eq!(profile.sync.mirror_dir.as_deref(), Some(notes.as_path()));

    // The server has them: Bob, whom Alice shares the vault with, reads them.
    let alice = reopen(&srv, "alice", "alice").await;
    let bob = login(&srv, "bob").await;
    let v = hub(&alice).await.resolve("My notes").await.unwrap();
    manage::share(&alice, &v, "bob", Role::Viewer, false)
        .await
        .unwrap();
    let bh = hub(&bob).await;
    assert_eq!(
        note(&bh, "My notes", "Daily/2026-11-01.md")
            .await
            .as_deref(),
        Some("# Today\n")
    );
    // Obsidian's drawings stay notes, as the folder keeps them.
    assert_eq!(
        note(&bh, "My notes", "Excalidraw/scene.excalidraw.md")
            .await
            .as_deref(),
        Some("---\nexcalidraw-plugin: parsed\n---\n# Drawing\n")
    );
    let shared = bh.resolve("My notes").await.unwrap();
    assert!(shared.lock().await.find("img.png").is_some());

    // Syncing again changes nothing.
    run(&srv, "alice", &["sync", "--once"]).await.unwrap();
    assert_eq!(tree(&vault_dir), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn setup_copies_the_notes_of_a_folder_into_a_vault_elsewhere() {
    let srv = start().await;
    let src = srv.path("Obsidian");
    obsidian_vault(&src);
    let before = tree(&src);
    let root = srv.path("notekpr");
    setup(
        &srv,
        "alice",
        &[
            "--dir",
            root.to_str().unwrap(),
            "--copy-from",
            src.to_str().unwrap(),
            "--vault",
            "Work",
            "--plaintext-ok",
        ],
    )
    .await
    .unwrap();
    assert_eq!(tree(&src), before, "the folder copied is as it was");
    assert_eq!(tree(&root.join("Work")), before);
    let h = hub(&reopen(&srv, "alice", "alice").await).await;
    assert_eq!(
        note(&h, "Work", "Blog/post.md").await.as_deref(),
        Some("A post with ![[img.png]]\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn setup_brings_notes_into_a_vault_of_the_same_name() {
    let srv = start().await;
    // As the web app makes it for a new account, with a note in it.
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "My notes").await.unwrap();
    let h = hub(&alice).await;
    ops::put_document(
        &h,
        "My notes",
        "From the web.md",
        "written in the browser\n",
        None,
    )
    .await
    .unwrap();
    drop(h);
    let notes = srv.path("Notes");
    write(&notes.join("Daily/a.md"), "from the folder\n");
    run(
        &srv,
        "alice",
        &[
            "setup",
            "--dir",
            notes.to_str().unwrap(),
            "--move-in",
            "--vault",
            "my notes",
            "--plaintext-ok",
            "--no-service",
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        tree(&notes.join("My notes")),
        [
            ("Daily/a.md".to_string(), "from the folder\n".to_string()),
            (
                "From the web.md".to_string(),
                "written in the browser\n".to_string()
            ),
        ]
    );
    let h = hub(&reopen(&srv, "alice", "alice").await).await;
    assert_eq!(
        note(&h, "My notes", "Daily/a.md").await.as_deref(),
        Some("from the folder\n")
    );
    let named = h
        .summaries()
        .await
        .into_iter()
        .filter(|s| s.name == "My notes")
        .count();
    assert_eq!(named, 1, "no second vault");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn setup_makes_a_first_vault_and_its_folder() {
    let srv = start().await;
    let root = srv.path("Notes");
    let dir = root.to_str().unwrap();
    setup(&srv, "alice", &["--dir", dir, "--plaintext-ok"])
        .await
        .unwrap();
    assert!(root.join("My notes/.notekpr-vault").is_file());
    // Again: signed in already, the folder known, nothing new.
    setup(&srv, "alice", &["--dir", dir]).await.unwrap();
    let h = hub(&reopen(&srv, "alice", "alice").await).await;
    let vaults: Vec<_> = h
        .summaries()
        .await
        .into_iter()
        .filter(|s| !s.kind.is_section())
        .map(|s| s.name)
        .collect();
    assert_eq!(vaults, ["My notes"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn setup_changes_nothing_it_was_not_told_to() {
    let srv = start().await;
    let notes = srv.path("Notes");
    obsidian_vault(&notes);
    let before = tree(&notes);
    let dir = notes.to_str().unwrap();

    // Notes there, and nothing said about them.
    let e = setup(&srv, "alice", &["--dir", dir, "--plaintext-ok"])
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("--move-in"), "{e:#}");
    // Decrypted files there not agreed to.
    let e = setup(&srv, "alice", &["--dir", dir, "--move-in"])
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("--plaintext-ok"), "{e:#}");
    // The vault's folder is there already.
    let e = setup(
        &srv,
        "alice",
        &[
            "--dir",
            dir,
            "--move-in",
            "--vault",
            "Blog",
            "--plaintext-ok",
        ],
    )
    .await
    .unwrap_err();
    assert!(format!("{e:#}").contains("there already"), "{e:#}");
    // Not where notekpr keeps its own settings and data.
    let own = srv.dirs("alice").data;
    let e = setup(
        &srv,
        "alice",
        &[
            "--dir",
            own.parent().unwrap().to_str().unwrap(),
            "--plaintext-ok",
        ],
    )
    .await
    .unwrap_err();
    assert!(format!("{e:#}").contains("notekpr's own"), "{e:#}");

    assert_eq!(tree(&notes), before);
    let h = hub(&reopen(&srv, "alice", "alice").await).await;
    assert!(
        h.summaries().await.iter().all(|s| s.kind.is_section()),
        "no vault was made"
    );
}
