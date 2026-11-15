//! One keeper per Markdown folder (docs/cli.md §4, "One keeper per
//! folder"): two syncs never keep one folder, whoever they are, and a sync
//! never reaches into another's.

use std::path::Path;

use crate::common::*;
use notekpr_cli::folder_claim::MARKER;
use notekpr_cli::mirror::{Mirror, MirrorOptions, Scope};
use notekpr_cli::{manage, ops};
use tokio::sync::watch;

/// Why a mirror couldn't start there.
async fn refused(hub: std::sync::Arc<notekpr_cli::hub::Hub>, root: &Path) -> String {
    match Mirror::new(hub, root).await {
        Ok(_) => panic!("a second keeper started on {}", root.display()),
        Err(e) => format!("{e:#}"),
    }
}

fn write(p: &Path, s: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

/// Two people — in settings neither knows of, as with a copy of the app
/// that has folders of its own — on one folder: the second never starts
/// there, while the first runs or after it stopped, until told to take it
/// over; then the first can't start again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_never_keep_one_folder() {
    let srv = start().await;
    let alice = login_profile(&srv, "alice-own", "alice").await;
    let bob = login_profile(&srv, "bob-own", "bob").await;
    manage::create_vault(&alice, "Mine").await.unwrap();
    manage::create_vault(&bob, "Theirs").await.unwrap();
    let root = srv.path("Notes");
    let (alice_hub, bob_hub) = (hub(&alice).await, hub(&bob).await);

    let mut a = Mirror::new(alice_hub.clone(), &root).await.unwrap();
    a.reconcile_all(false).await.unwrap();
    assert!(root.join("Mine").is_dir() && root.join(MARKER).is_file());
    let e = refused(bob_hub.clone(), &root).await;
    assert!(e.contains("right now"), "{e}");
    drop(a);
    let e = refused(bob_hub.clone(), &root).await;
    assert!(
        e.contains("the CLI, profile 'alice'") && e.contains("--take-over"),
        "{e}"
    );
    // Bob's notes never came: nothing of his vault there.
    assert!(!root.join("Theirs").exists());

    let take = MirrorOptions {
        take_over: true,
        ..Default::default()
    };
    let mut b = Mirror::with_options(bob_hub.clone(), &root, take)
        .await
        .unwrap();
    b.reconcile_all(false).await.unwrap();
    drop(b);
    let e = refused(alice_hub, &root).await;
    assert!(e.contains("the CLI, profile 'bob'"), "{e}");
}

/// A Markdown folder that turns up inside one being kept (moved in, made
/// by an older release): the sync stops before touching it, or anything
/// more.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sync_never_reaches_into_another_s_folder() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Mine").await.unwrap();
    let root = srv.path("Notes");
    let alice_hub = hub(&alice).await;
    let mut m = Mirror::new(alice_hub.clone(), &root).await.unwrap();
    m.reconcile_all(false).await.unwrap();
    let vault = root.join("Mine");

    let other = vault.join("Someone else's");
    write(&other.join(MARKER), r#"{"v":1,"id":"x","by":"another"}"#);
    write(&other.join("theirs.md"), "not alice's\n");
    let e = format!("{:#}", m.reconcile_all(false).await.unwrap_err());
    assert!(e.contains("another Markdown folder"), "{e}");
    // Stopped for good: a pass later, still nothing taken in.
    let id = alice_hub
        .resolve("Mine")
        .await
        .unwrap()
        .lock()
        .await
        .id()
        .to_string();
    assert!(m.ingest(&id, false).await.is_err());
    let (_stop, rx) = watch::channel(false);
    let e = format!("{:#}", m.run(rx).await.unwrap_err());
    assert!(e.contains("another Markdown folder"), "{e}");
    alice_hub.sync_vault(&id).await.unwrap();
    assert!(
        note(&alice_hub, &id, "Someone else's/theirs.md")
            .await
            .is_none()
    );
}

/// Where a note from elsewhere would be written, or straight in a vault's
/// folder: nothing of another's Markdown folder is read or written.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nothing_in_another_s_folder_is_read_or_written() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Mine").await.unwrap();
    let root = srv.path("Notes");
    let alice_hub = hub(&alice).await;
    let id = alice_hub
        .resolve("Mine")
        .await
        .unwrap()
        .lock()
        .await
        .id()
        .to_string();
    let mut m = Mirror::new(alice_hub.clone(), &root).await.unwrap();
    m.reconcile_all(false).await.unwrap();
    let vault = root.join("Mine");

    // A note from another device, into a folder that is someone else's.
    let other = vault.join("Theirs");
    write(&other.join(MARKER), r#"{"v":1,"id":"x","by":"another"}"#);
    ops::put_document(&alice_hub, &id, "Theirs/new.md", "from elsewhere\n", None)
        .await
        .unwrap();
    let _ = m.write_remote(&id, &Scope::All).await;
    assert!(!other.join("new.md").exists());

    // A Markdown folder straight in the vault's folder: stopped.
    std::fs::remove_dir_all(&other).unwrap();
    write(&vault.join(MARKER), r#"{"v":1,"id":"x","by":"another"}"#);
    write(&vault.join("theirs.md"), "not alice's\n");
    let e = format!("{:#}", m.reconcile_all(false).await.unwrap_err());
    assert!(e.contains("another Markdown folder"), "{e}");
    assert!(note(&alice_hub, &id, "theirs.md").await.is_none());
}

/// The folder replaced under its name while kept (moved away, another
/// made there): not the one taken, so nothing in it is touched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_folder_made_in_place_of_the_one_kept_is_left_alone() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Mine").await.unwrap();
    let root = srv.path("Notes");
    let alice_hub = hub(&alice).await;
    let id = alice_hub
        .resolve("Mine")
        .await
        .unwrap()
        .lock()
        .await
        .id()
        .to_string();
    let mut m = Mirror::new(alice_hub.clone(), &root).await.unwrap();
    m.reconcile_all(false).await.unwrap();

    std::fs::rename(&root, srv.path("Notes moved")).unwrap();
    write(&root.join("Mine").join("foreign.md"), "someone else's\n");
    let e = format!("{:#}", m.reconcile_all(true).await.unwrap_err());
    assert!(e.contains("was replaced"), "{e}");
    assert!(note(&alice_hub, &id, "foreign.md").await.is_none());
}
