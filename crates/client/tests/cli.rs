//! The `notekpr` command line itself: confirmations, profile settings
//! and refusals that protect data.

mod common;

use clap::Parser;
use common::*;
use notekpr_client::cli::Cli;
use notekpr_client::commands;
use notekpr_client::profile::ProfileStore;

/// Run a command line for the test client `who` (its own config/data dirs).
async fn run(srv: &TestServer, who: &str, args: &[&str]) -> anyhow::Result<()> {
    let mut argv = vec!["notekpr", "-q", "--profile", who];
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
    use notekpr_client::profile::ProfileLock;
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
        format!("{err:#}").contains("in use by another notekpr process"),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leaving_uploads_first_and_can_remove_the_mirror() {
    use notekpr_client::manage;
    use notekpr_core::protocol::Role;
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    login(&srv, "bob").await;
    for name in ["Family", "Work"] {
        manage::create_vault(&alice, name).await.unwrap();
    }
    let alice_hub = hub(&alice).await;
    for name in ["Family", "Work"] {
        let v = alice_hub.resolve(name).await.unwrap();
        manage::share(&alice, &v, "bob", Role::Editor, false)
            .await
            .unwrap();
    }
    let m = srv.path("bob-notes");
    let m_str = m.to_str().unwrap();
    run(
        &srv,
        "bob",
        &["sync", "--once", "--dir", m_str, "--plaintext-ok"],
    )
    .await
    .unwrap();
    assert!(m.join("Family").is_dir() && m.join("Work").is_dir());

    // Leaving one vault can take its decrypted folder along; the other stays.
    run(
        &srv,
        "bob",
        &["vault", "leave", "Work", "--yes", "--remove-mirror"],
    )
    .await
    .unwrap();
    assert!(!m.join("Work").exists());
    assert!(m.join("Family").is_dir());

    // An edit made offline is still queued...
    let store = ProfileStore::new(&srv.dirs("bob"), "bob").unwrap();
    let real = store.require().unwrap().server;
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = format!("http://{}", l.local_addr().unwrap());
    drop(l);
    store.update(|p| p.server = dead.clone()).unwrap();
    run(&srv, "bob", &["mkdir", "Family", "Made offline"])
        .await
        .unwrap();
    store.update(|p| p.server = real.clone()).unwrap();
    // ... `logout --forget` without confirmation (no terminal) deletes nothing.
    let data = srv.dirs("bob").profile_data("bob");
    assert!(run(&srv, "bob", &["logout", "--forget"]).await.is_err());
    assert!(data.exists());
    // Confirmed: the edit is uploaded first, then everything is removed.
    run(
        &srv,
        "bob",
        &["logout", "--forget", "--yes", "--remove-mirror"],
    )
    .await
    .unwrap();
    assert!(!data.exists());
    assert!(!m.exists(), "the emptied mirror directory is removed too");
    let fam = alice_hub.resolve("Family").await.unwrap();
    let fam_id = fam.lock().await.id().to_string();
    alice_hub.sync_vault(&fam_id).await.unwrap();
    assert!(fam.lock().await.is_folder("Made offline"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_only_gateway_runs_no_changing_commands() {
    use notekpr_client::control;
    use notekpr_client::hub::{Hub, HubOptions};
    use notekpr_client::profile::ProfileLock;
    let srv = start().await;
    login(&srv, "alice").await;
    run(&srv, "alice", &["vault", "create", "Home"])
        .await
        .unwrap();
    let lock = ProfileLock::try_acquire(&srv.dirs("alice"), "alice")
        .unwrap()
        .expect("free");
    let gateway = Hub::start(
        reopen(&srv, "alice", "alice").await,
        HubOptions {
            read_only: true,
            catch_up: true,
            offline_ok: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let _endpoint = control::serve(&srv.dirs("alice"), "alice", gateway.clone(), None)
        .await
        .unwrap();
    // Not only the ones that check on their own (write, rm): every change.
    for args in [
        &["mkdir", "Home", "Ideas"][..],
        &["vault", "rename", "Home", "House"],
        &["quick", "--vault", "Home", "a", "thought"],
    ] {
        let err = run(&srv, "alice", args).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("read-only"),
            "{args:?}: {err:#}"
        );
    }
    let home = gateway.resolve("Home").await.unwrap();
    assert!(!home.lock().await.is_folder("Ideas"));
    // Reading still works.
    run(&srv, "alice", &["ls", "Home"]).await.unwrap();
    drop(lock);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_gateway_logs_the_commands_handed_to_it() {
    use notekpr_client::audit::AuditLog;
    use notekpr_client::control;
    use notekpr_client::profile::ProfileLock;
    let srv = start().await;
    login(&srv, "alice").await;
    run(&srv, "alice", &["vault", "create", "Home"])
        .await
        .unwrap();
    let lock = ProfileLock::try_acquire(&srv.dirs("alice"), "alice")
        .unwrap()
        .expect("free");
    let session = reopen(&srv, "alice", "alice").await;
    let log = tempfile::tempdir().unwrap();
    let audit = std::sync::Arc::new(AuditLog::open(log.path()));
    let gateway = hub(&session).await;
    let _endpoint = control::serve(
        &srv.dirs("alice"),
        "alice",
        gateway.clone(),
        Some(audit.clone()),
    )
    .await
    .unwrap();
    run(&srv, "alice", &["mkdir", "Home", "Secret plans"])
        .await
        .unwrap();
    run(&srv, "alice", &["vault", "rename", "Home", "House"])
        .await
        .unwrap();
    // A failed command changed nothing: not logged.
    run(&srv, "alice", &["rm", "House", "Nope.md"])
        .await
        .unwrap_err();
    let id = gateway
        .resolve("House")
        .await
        .unwrap()
        .lock()
        .await
        .id()
        .to_string();
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(audit.path())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let ops: Vec<(&str, &str, &str)> = lines
        .iter()
        .map(|l| {
            (
                l["via"].as_str().unwrap(),
                l["op"].as_str().unwrap(),
                l["vault"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        ops,
        [
            ("cli", "mkdir", id.as_str()),
            ("cli", "vault rename", id.as_str())
        ]
    );
    // Names and paths stay out of it.
    let raw = std::fs::read_to_string(audit.path()).unwrap();
    assert!(!raw.contains("Secret") && !raw.contains("Home") && !raw.contains("House"));
    drop(lock);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changing_commands_are_handed_to_the_running_daemon() {
    use notekpr_client::control;
    use notekpr_client::profile::ProfileLock;
    let srv = start().await;
    login(&srv, "alice").await;
    run(&srv, "alice", &["vault", "create", "Home"])
        .await
        .unwrap();

    // A sync daemon owns the profile, and takes the changing commands.
    let lock = ProfileLock::try_acquire(&srv.dirs("alice"), "alice")
        .unwrap()
        .expect("free");
    let daemon = hub(&reopen(&srv, "alice", "alice").await).await;
    let endpoint = control::serve(&srv.dirs("alice"), "alice", daemon.clone(), None)
        .await
        .unwrap();
    // Handed over right away, not after waiting for the profile.
    let t = std::time::Instant::now();
    run(&srv, "alice", &["mkdir", "Home", "Ideas"])
        .await
        .unwrap();
    assert!(
        t.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        t.elapsed()
    );
    run(
        &srv,
        "alice",
        &["quick", "--vault", "Home", "call", "the", "dentist"],
    )
    .await
    .unwrap();
    // Done on the daemon's own copies.
    let home = daemon.resolve("Home").await.unwrap();
    {
        let v = home.lock().await;
        assert!(v.is_folder("Ideas"));
        let quick: Vec<String> = v
            .paths()
            .into_iter()
            .filter(|(p, _)| p.starts_with("Quick notes/"))
            .map(|(_, e)| v.note_text(&e.id))
            .collect();
        assert_eq!(quick, ["call the dentist"]);
    }
    // Its errors come back as the command's own.
    let err = run(&srv, "alice", &["rm", "Home", "Nope.md"])
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("Nope"), "{err:#}");
    // Never `sync` itself.
    let err = run(&srv, "alice", &["sync", "--once"]).await.unwrap_err();
    assert!(format!("{err:#}").contains("in use"), "{err:#}");
    // Without a daemon taking them, refused as before.
    drop(endpoint);
    let err = run(&srv, "alice", &["mkdir", "Home", "More"])
        .await
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("in use by another notekpr process"),
        "{err:#}"
    );
    drop(lock);
}

/// `ls` lines names up when only some entries have an icon.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ls_lines_names_up_with_and_without_icons() {
    use notekpr_client::ui;
    let srv = start().await;
    login(&srv, "alice").await;
    run(&srv, "alice", &["vault", "create", "Home"])
        .await
        .unwrap();
    for folder in ["Garden", "Quick notes"] {
        run(&srv, "alice", &["mkdir", "Home", folder])
            .await
            .unwrap();
    }
    for note in ["Start here.md", "Plain.md"] {
        let (r, _) = ui::capture(
            Some(b"# A note\n".to_vec()),
            true,
            run(&srv, "alice", &["write", "Home", note]),
        )
        .await;
        r.unwrap();
    }
    run(&srv, "alice", &["icon", "Home", "Garden", "🌱"])
        .await
        .unwrap();
    run(&srv, "alice", &["icon", "Home", "Start here.md", "✈️"])
        .await
        .unwrap();
    let (r, out) = ui::capture(None, true, run(&srv, "alice", &["ls", "Home"])).await;
    r.unwrap();
    // The column each name starts at.
    let col = |name: &str| {
        let line = out.stdout.lines().find(|l| l.contains(name)).unwrap();
        ui::display_width(&line[..line.find(name).unwrap()])
    };
    let cols: Vec<usize> = ["Garden/", "Quick notes/", "Start here.md", "Plain.md"]
        .iter()
        .map(|n| col(n))
        .collect();
    assert!(
        cols.iter().all(|c| *c == cols[0]),
        "{cols:?}\n{}",
        out.stdout
    );
}
