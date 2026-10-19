//! The `coolpsonotes` command line itself: confirmations, profile settings
//! and refusals that protect data.

mod common;

use clap::Parser;
use common::*;
use coolpsonotes_client::cli::Cli;
use coolpsonotes_client::commands;
use coolpsonotes_client::profile::ProfileStore;

/// Run a command line for the test client `who` (its own config/data dirs).
async fn run(srv: &TestServer, who: &str, args: &[&str]) -> anyhow::Result<()> {
    let mut argv = vec!["coolpsonotes", "-q", "--profile", who];
    argv.extend_from_slice(args);
    commands::run_in(Cli::try_parse_from(argv)?, srv.dirs(who)).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plaintext_mirror_is_confirmed_per_directory() {
    let srv = start().await;
    login(&srv, "alice").await;
    run(&srv, "alice", &["vault", "create", "Home"])
        .await
        .unwrap();
    let a = srv.path("notes-a");
    let b = srv.path("notes-b");
    let a_str = a.to_str().unwrap();
    let b_str = b.to_str().unwrap();

    // Without confirmation (and no terminal), decrypted files are refused.
    let err = run(&srv, "alice", &["sync", "--once", "--dir", a_str])
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("--plaintext-ok"), "{err:#}");
    assert!(!a.exists());

    run(
        &srv,
        "alice",
        &["sync", "--once", "--dir", a_str, "--plaintext-ok"],
    )
    .await
    .unwrap();
    assert!(a.join("Home").is_dir());
    // Plain `sync` reuses the remembered (confirmed) directory.
    run(&srv, "alice", &["sync", "--once"]).await.unwrap();

    // Another directory needs its own confirmation.
    let err = run(&srv, "alice", &["sync", "--once", "--dir", b_str])
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("--plaintext-ok"), "{err:#}");
    assert!(!b.exists());
    let p = ProfileStore::new(&srv.dirs("alice"), "alice")
        .unwrap()
        .require()
        .unwrap();
    assert_eq!(p.sync.mirror_dir.as_deref(), Some(a.as_path()));
    assert!(p.sync.plaintext_confirmed(&a) && !p.sync.plaintext_confirmed(&b));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_missing_remembered_mirror_directory_is_not_recreated() {
    let srv = start().await;
    login(&srv, "alice").await;
    run(&srv, "alice", &["vault", "create", "Home"])
        .await
        .unwrap();
    let a = srv.path("usb/Notes");
    let a_str = a.to_str().unwrap();
    run(
        &srv,
        "alice",
        &["sync", "--once", "--dir", a_str, "--plaintext-ok"],
    )
    .await
    .unwrap();
    std::fs::write(a.join("Home/Plan.md"), "plan\n").unwrap();
    run(&srv, "alice", &["sync", "--once"]).await.unwrap();

    // The drive is unmounted.
    std::fs::rename(srv.path("usb"), srv.path("usb-unmounted")).unwrap();
    let err = run(&srv, "alice", &["sync", "--once"]).await.unwrap_err();
    assert!(format!("{err:#}").contains("does not exist"), "{err:#}");
    // Naming it explicitly is refused too (notes were mirrored there).
    let err = run(&srv, "alice", &["sync", "--once", "--dir", a_str])
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("missing"), "{err:#}");
    assert!(!a.exists());
    let h = hub(&reopen(&srv, "alice", "alice").await).await;
    assert_eq!(note(&h, "Home", "Plan.md").await.as_deref(), Some("plan\n"));

    // --restore-missing writes a fresh copy.
    run(&srv, "alice", &["sync", "--once", "--restore-missing"])
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(a.join("Home/Plan.md")).unwrap(),
        "plan\n"
    );
}

/// Every file below `dir` with its content.
fn files(dir: &std::path::Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| (e.path().to_path_buf(), std::fs::read(e.path()).unwrap()))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn commands_do_not_overwrite_a_running_daemons_local_copies() {
    use coolpsonotes_client::profile::ProfileLock;
    let srv = start().await;
    login(&srv, "alice").await;
    run(&srv, "alice", &["vault", "create", "Home"])
        .await
        .unwrap();
    run(&srv, "alice", &["mkdir", "Home", "Projects"])
        .await
        .unwrap();

    // A sync daemon (or the gateway) owns the profile.
    let daemon = ProfileLock::try_acquire(&srv.dirs("alice"), "alice")
        .unwrap()
        .expect("free");
    // Changing commands are refused, with a clear message.
    let err = run(&srv, "alice", &["mkdir", "Home", "Ideas"])
        .await
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("in use by another coolpsonotes process"),
        "{err:#}"
    );
    // A second daemon is refused right away.
    let err = run(&srv, "alice", &["sync"]).await.unwrap_err();
    assert!(format!("{err:#}").contains("in use"), "{err:#}");
    // Reading works on the daemon's local copies, which are left untouched.
    let data = srv.dirs("alice").profile_data("alice");
    let before = files(&data);
    run(&srv, "alice", &["ls", "Home"]).await.unwrap();
    run(&srv, "alice", &["status"]).await.unwrap();
    assert_eq!(files(&data), before, "a read-only view writes nothing");

    drop(daemon);
    run(&srv, "alice", &["mkdir", "Home", "Ideas"])
        .await
        .unwrap();
    let h = hub(&reopen(&srv, "alice", "alice").await).await;
    assert!(
        h.resolve("Home")
            .await
            .unwrap()
            .lock()
            .await
            .is_folder("Ideas")
    );
}
