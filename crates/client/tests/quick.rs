//! `notekpr quick`: quick notes land in `Quick notes/` of the
//! quick-notes vault (--vault, the synced preference, or the default).

mod common;

use clap::Parser;
use common::*;
use notekpr_client::cli::Cli;
use notekpr_client::commands;
use notekpr_client::hub::Hub;
use notekpr_client::ops;
use notekpr_client::session::Session;
use notekpr_core::protocol::{PrefsBlob, PutPrefsRequest};

async fn run(srv: &TestServer, who: &str, args: &[&str]) -> anyhow::Result<()> {
    let mut argv = vec!["notekpr", "-q", "--profile", who];
    argv.extend_from_slice(args);
    commands::run_in(Cli::try_parse_from(argv)?, srv.dirs(who)).await
}

/// Quick notes of a vault as (path, text), newest first.
async fn quick_notes(h: &Hub, vault: &str, archived: bool) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for q in ops::list_quick_notes(h, vault, archived, 100)
        .await
        .unwrap()
    {
        out.push((q.path.clone(), note(h, vault, &q.path).await.unwrap()));
    }
    out
}

fn is_stamped(path: &str) -> bool {
    let Some(name) = path
        .strip_prefix("Quick notes/")
        .and_then(|n| n.strip_suffix(".md"))
    else {
        return false;
    };
    let stamp = name.split(' ').take(2).collect::<Vec<_>>().join(" ");
    let b = stamp.as_bytes();
    stamp.len() == 16
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b' '
        && b[13] == b'.'
        && stamp.chars().filter(|c| c.is_ascii_digit()).count() == 12
}

/// What the web client does in Settings: store `quick_vault` in the
/// encrypted, synced preferences (keeping the other fields).
async fn set_quick_vault(s: &Session, vault_id: &str) {
    let blob: PrefsBlob = s.api.get("/me/prefs").await.unwrap();
    let mut prefs: serde_json::Value = match &blob.ciphertext {
        Some(ct) => {
            serde_json::from_slice(&s.account().open_private(&s.user_id, ct).unwrap()).unwrap()
        }
        None => serde_json::json!({ "favorites": [], "recent": [], "pins": {} }),
    };
    prefs["quick_vault"] = serde_json::json!(vault_id);
    let req = PutPrefsRequest {
        ciphertext: s
            .account()
            .seal_private(&s.user_id, &serde_json::to_vec(&prefs).unwrap()),
        expected_version: blob.version,
    };
    let _: PrefsBlob = s.api.put("/me/prefs", &req).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quick_notes_go_to_the_quick_notes_vault() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;

    // Nothing to write into yet.
    let err = run(&srv, "alice", &["quick", "lost thought"])
        .await
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("need a vault you can edit"),
        "{err:#}"
    );

    // The personal vault is the oldest own one.
    run(&srv, "alice", &["vault", "create", "My notes"])
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    run(&srv, "alice", &["vault", "create", "Work"])
        .await
        .unwrap();
    run(&srv, "alice", &["quick", "Call", "the", "dentist"])
        .await
        .unwrap();
    run(&srv, "alice", &["quick", "Second thought\n\n"])
        .await
        .unwrap();
    run(
        &srv,
        "alice",
        &["quick", "--vault", "Work", "Standup: demo"],
    )
    .await
    .unwrap();

    let h = hub(&reopen(&srv, "alice", "alice").await).await;
    let mine = quick_notes(&h, "My notes", false).await;
    assert_eq!(mine.len(), 2, "{mine:?}");
    // Newest first, named by the time; a second one in the same minute is numbered.
    assert_eq!(mine[0].1, "Second thought");
    assert_eq!(mine[1].1, "Call the dentist");
    assert!(mine.iter().all(|(p, _)| is_stamped(p)), "{mine:?}");
    if mine[0].0[..28] == mine[1].0[..28] {
        assert!(mine[0].0.ends_with(" 2.md"), "{mine:?}");
    }
    let work = quick_notes(&h, "Work", false).await;
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].1, "Standup: demo");
    assert!(is_stamped(&work[0].0), "{work:?}");
    drop(h);

    // The vault chosen in the (web client's) synced preferences.
    let work_id = {
        let h = hub(&alice).await;
        h.resolve("Work")
            .await
            .unwrap()
            .lock()
            .await
            .id()
            .to_string()
    };
    set_quick_vault(&alice, &work_id).await;
    run(&srv, "alice", &["quick", "From the preference"])
        .await
        .unwrap();
    // A choice that can't be used (a vault that is gone) falls back to the default.
    set_quick_vault(&alice, "no-such-vault").await;
    run(&srv, "alice", &["quick", "Back to the default"])
        .await
        .unwrap();

    let h = hub(&reopen(&srv, "alice", "alice").await).await;
    let work: Vec<String> = quick_notes(&h, "Work", false)
        .await
        .into_iter()
        .map(|(_, t)| t)
        .collect();
    assert_eq!(work, ["From the preference", "Standup: demo"]);
    assert_eq!(
        quick_notes(&h, "My notes", false).await[0].1,
        "Back to the default"
    );

    // Archived ones are only listed when asked for.
    let oldest = quick_notes(&h, "My notes", false).await.pop().unwrap().0;
    let name = oldest.strip_prefix("Quick notes/").unwrap().to_string();
    drop(h);
    run(
        &srv,
        "alice",
        &[
            "mv",
            "My notes",
            &oldest,
            &format!("Quick notes/Archive/{name}"),
        ],
    )
    .await
    .unwrap();
    let h = hub(&reopen(&srv, "alice", "alice").await).await;
    let listed = ops::list_quick_notes(&h, "My notes", false, 10)
        .await
        .unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|q| q.preview.as_str())
            .collect::<Vec<_>>(),
        ["Back to the default", "Second thought"]
    );
    let all = ops::list_quick_notes(&h, "My notes", true, 10)
        .await
        .unwrap();
    assert_eq!(all.len(), 3);
    assert!(
        all.iter()
            .any(|q| q.archived && q.preview == "Call the dentist")
    );
    drop(h);

    // The listing command itself (reads only).
    run(&srv, "alice", &["quick", "--list"]).await.unwrap();
    run(
        &srv,
        "alice",
        &["quick", "--list", "--archived", "--vault", "Work"],
    )
    .await
    .unwrap();
    // Text and --list don't go together; nor does an empty note.
    assert!(run(&srv, "alice", &["quick", "--list", "x"]).await.is_err());
    let err = run(&srv, "alice", &["quick", "   "]).await.unwrap_err();
    assert!(format!("{err:#}").contains("nothing to save"), "{err:#}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quick_notes_need_a_vault_you_can_edit() {
    let srv = start().await;
    login(&srv, "alice").await;
    login(&srv, "bob").await;
    run(&srv, "alice", &["vault", "create", "Shared"])
        .await
        .unwrap();
    run(
        &srv,
        "alice",
        &["vault", "share", "Shared", "bob", "--role", "viewer"],
    )
    .await
    .unwrap();
    // Bob can only read: no quick notes.
    let err = run(&srv, "bob", &["quick", "hello"]).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("need a vault you can edit"),
        "{err:#}"
    );
    // As an editor (and without a vault of his own) the shared vault is used.
    run(
        &srv,
        "alice",
        &["vault", "share", "Shared", "bob", "--role", "editor"],
    )
    .await
    .unwrap();
    run(&srv, "bob", &["quick", "hello"]).await.unwrap();
    let h = hub(&reopen(&srv, "bob", "bob").await).await;
    assert_eq!(quick_notes(&h, "Shared", false).await[0].1, "hello");
}
