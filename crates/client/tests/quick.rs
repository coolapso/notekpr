//! `notekpr quick`: the person's quick notes — a vault of their own that is
//! flat, never shared, holds a limited number of notes and deletes for good;
//! notes leave it by moving to a vault (`--move`, `mv --to-vault`).

mod common;

use clap::Parser;
use common::*;
use notekpr_client::cli::Cli;
use notekpr_client::commands;
use notekpr_client::hub::Hub;
use notekpr_client::ops;
use notekpr_client::vault::Vault;
use notekpr_core::engine::{self, VaultMeta, VaultReplica};
use notekpr_core::protocol::{
    DEFAULT_QUICK_NOTES_PER_USER, InstanceSettingsUpdate, Limit, VaultInfo, VaultKind,
};

async fn run(srv: &TestServer, who: &str, args: &[&str]) -> anyhow::Result<()> {
    let mut argv = vec!["notekpr", "-q", "--profile", who];
    argv.extend_from_slice(args);
    commands::run_in(Cli::try_parse_from(argv)?, srv.dirs(who)).await
}

/// The quick notes as (name, text), newest first.
async fn quick_notes(h: &Hub) -> Vec<(String, String)> {
    let q = ops::list_quick_notes(h, 100).await.unwrap();
    let mut out = Vec::new();
    for n in q.notes {
        let v = h.quick_notes().await.unwrap();
        let text = v.lock().await.note_text(&n.id);
        out.push((n.title, text));
    }
    out
}

/// A name like `2026-10-25 09.15` (or `… 2` for a second one that minute).
fn is_stamped(name: &str) -> bool {
    let stamp = name.split(' ').take(2).collect::<Vec<_>>().join(" ");
    let b = stamp.as_bytes();
    stamp.len() == 16
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b' '
        && b[13] == b'.'
        && stamp.chars().filter(|c| c.is_ascii_digit()).count() == 12
}

async fn set_limit(srv: &TestServer, limit: Limit) {
    let change = InstanceSettingsUpdate {
        quick_notes_per_user: Some(limit),
        ..Default::default()
    };
    srv.state
        .instance
        .update(&srv.state.db, &change)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quick_notes_need_no_vault_and_stay_flat() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;

    // No vault needed: the quick notes are made the first time.
    run(&srv, "alice", &["quick", "Call", "the", "dentist"])
        .await
        .unwrap();
    run(&srv, "alice", &["quick", "Second thought\n\n"])
        .await
        .unwrap();
    let h = hub(&reopen(&srv, "alice", "alice").await).await;
    let notes = quick_notes(&h).await;
    // Newest first, named by the time; a second one in the same minute is numbered.
    assert_eq!(
        notes.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>(),
        ["Second thought", "Call the dentist"]
    );
    assert!(notes.iter().all(|(n, _)| is_stamped(n)), "{notes:?}");
    let q = ops::list_quick_notes(&h, 100).await.unwrap();
    assert_eq!(q.count, 2);
    assert_eq!(q.limit, Some(Limit::Max(DEFAULT_QUICK_NOTES_PER_USER)));
    // Once per person, and no vault in the lists.
    let infos = alice.api.vaults().await.unwrap();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].kind, VaultKind::QuickNotes);
    let summaries = h.summaries().await;
    assert_eq!(summaries[0].kind, VaultKind::QuickNotes);
    drop(h);

    // Nothing goes into folders.
    for args in [
        &["mkdir", "Quick notes", "Archive"][..],
        &["mv", "Quick notes", &notes[0].0, "Archive/"],
    ] {
        let err = run(&srv, "alice", args).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("no folders"),
            "{args:?}: {err:#}"
        );
    }
    // No vault may be named like them, in any case.
    run(&srv, "alice", &["vault", "create", "Ideas"])
        .await
        .unwrap();
    for args in [
        &["vault", "create", "Quick notes"][..],
        &["vault", "create", " quick NOTES "],
        &["vault", "rename", "Ideas", "Quick Notes"],
    ] {
        let err = run(&srv, "alice", args).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("the name of your quick notes"),
            "{args:?}: {err:#}"
        );
    }
    // One named so anyway, by a client that doesn't check, doesn't take the
    // name: it still means the quick notes, and that vault goes by its id.
    let meta = VaultMeta::named("Quick notes");
    let (req, ring) = engine::new_vault(
        alice.account(),
        &alice.user_id,
        &meta,
        notekpr_core::now_ms(),
    )
    .unwrap();
    let info: VaultInfo = alice.api.post("/vaults", &req).await.unwrap();
    let replica = VaultReplica::new(&info.id, info.role, ring, meta);
    let mut other = Vault::new(
        alice.vault_store(&info.id).unwrap(),
        replica,
        &info.owner_id,
        info.archived,
    );
    other.save().unwrap();
    let err = run(&srv, "alice", &["mkdir", "Quick notes", "Archive"])
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("no folders"), "{err:#}");
    run(&srv, "alice", &["mkdir", &info.id, "Archive"])
        .await
        .unwrap();

    // Deleted for good: no trash.
    run(&srv, "alice", &["quick", "--delete", &notes[1].0])
        .await
        .unwrap();
    let h = hub(&reopen(&srv, "alice", "alice").await).await;
    assert_eq!(
        quick_notes(&h).await,
        [(notes[0].0.clone(), "Second thought".to_string())]
    );
    let q = h.quick_notes().await.unwrap();
    assert!(q.lock().await.trashed().is_empty());

    // The listing command itself (reads only); text and --list don't go
    // together, nor does an empty note.
    run(&srv, "alice", &["quick", "--list"]).await.unwrap();
    assert!(run(&srv, "alice", &["quick", "--list", "x"]).await.is_err());
    let err = run(&srv, "alice", &["quick", "   "]).await.unwrap_err();
    assert!(format!("{err:#}").contains("nothing to save"), "{err:#}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quick_notes_move_to_a_vault_with_their_attachments() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    run(&srv, "alice", &["vault", "create", "Home"])
        .await
        .unwrap();
    run(&srv, "alice", &["vault", "create", "Work"])
        .await
        .unwrap();
    let h = hub(&alice).await;
    let q = h.quick_notes().await.unwrap();
    let qid = q.lock().await.id().to_string();
    // A file goes in only for a quick note linking to it.
    let err = ops::put_attachment(&h, &qid, "photo.png", b"one", None)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("link to 'photo.png' from a quick note first"),
        "{err}"
    );
    let made = ops::create_quick_note(&h, "Garden plan: beds\n\n![[photo.png]]")
        .await
        .unwrap();
    ops::put_attachment(&h, &qid, "photo.png", b"one", None)
        .await
        .unwrap();
    ops::put_attachment(&h, "Home", "Projects/photo.png", b"other", None)
        .await
        .unwrap();
    let name = notekpr_core::paths::title(&made.path).to_string();
    drop(h);

    // To a folder of a vault, named after its first line; the photo comes
    // along (renamed, as Projects/photo.png is taken) and the link follows.
    run(
        &srv,
        "alice",
        &[
            "quick", "--move", &name, "--to", "Home", "--folder", "Projects",
        ],
    )
    .await
    .unwrap();
    let h = hub(&reopen(&srv, "alice", "alice").await).await;
    let text = note(&h, "Home", "Projects/Garden plan- beds.md")
        .await
        .expect("moved");
    assert_eq!(text, "Garden plan: beds\n\n![[photo 2.png]]");
    {
        let home = h.resolve("Home").await.unwrap();
        let g = home.lock().await;
        let (_, e) = g.find("Projects/photo 2.png").unwrap();
        assert_eq!(
            g.cached_attachment(&e.id).unwrap().as_deref(),
            Some(&b"one"[..])
        );
    }
    // Gone from the quick notes for good, its photo too.
    assert!(quick_notes(&h).await.is_empty());
    {
        let q = h.quick_notes().await.unwrap();
        let g = q.lock().await;
        assert!(g.paths().is_empty() && g.trashed().is_empty());
    }
    drop(h);

    // Any note moves between vaults; the original goes to the trash.
    run(
        &srv,
        "alice",
        &[
            "mv",
            "Home",
            "Projects/Garden plan- beds.md",
            "Plans/",
            "--to-vault",
            "Work",
        ],
    )
    .await
    .unwrap();
    let h = hub(&reopen(&srv, "alice", "alice").await).await;
    assert_eq!(
        note(&h, "Work", "Plans/Garden plan- beds.md")
            .await
            .as_deref(),
        Some("Garden plan: beds\n\n![[photo 2.png]]")
    );
    let home = h.resolve("Home").await.unwrap();
    let trashed: Vec<String> = home
        .lock()
        .await
        .trashed()
        .into_iter()
        .map(|e| e.path)
        .collect();
    assert!(
        trashed.contains(&"Projects/Garden plan- beds.md".to_string()),
        "{trashed:?}"
    );
    assert!(
        trashed.contains(&"Projects/photo 2.png".to_string()),
        "{trashed:?}"
    );
    // The other photo stays: the note didn't link to it.
    assert!(home.lock().await.find("Projects/photo.png").is_some());
    // Not into the quick notes, nor within one vault.
    for args in [
        &[
            "mv",
            "Work",
            "Plans/Garden plan- beds.md",
            "/",
            "--to-vault",
            &qid,
        ][..],
        &[
            "mv",
            "Work",
            "Plans/Garden plan- beds.md",
            "/",
            "--to-vault",
            "Work",
        ],
    ] {
        assert!(run(&srv, "alice", args).await.is_err(), "{args:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quick_notes_hold_as_many_as_allowed() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    set_limit(&srv, Limit::Max(2)).await;
    run(&srv, "alice", &["quick", "one"]).await.unwrap();
    run(&srv, "alice", &["quick", "two"]).await.unwrap();
    let err = run(&srv, "alice", &["quick", "three"]).await.unwrap_err();
    assert!(format!("{err:#}").contains("full (2 of 2)"), "{err:#}");

    // An app that doesn't know yet: the server refuses, the note stays on
    // that device, and goes up once there's room.
    set_limit(&srv, Limit::Max(3)).await;
    let h = hub(&alice).await;
    set_limit(&srv, Limit::Max(2)).await;
    ops::create_quick_note(&h, "kept here").await.unwrap();
    let listed = ops::list_quick_notes(&h, 10).await.unwrap();
    assert!(
        listed
            .notes
            .iter()
            .any(|n| n.preview == "kept here" && n.waiting_for_room),
        "{listed:?}"
    );
    set_limit(&srv, Limit::Max(3)).await;
    let list = alice.api.vaults().await.unwrap();
    h.open_listed(&list).await.unwrap();
    let qid = h.quick_notes().await.unwrap().lock().await.id().to_string();
    h.sync_vault(&qid).await.unwrap();
    let fresh = hub(&reopen(&srv, "alice", "alice").await).await;
    let texts: Vec<String> = quick_notes(&fresh)
        .await
        .into_iter()
        .map(|(_, t)| t)
        .collect();
    assert_eq!(texts.len(), 3, "{texts:?}");
    assert!(texts.contains(&"kept here".to_string()));
}

/// A file whose link is edited out of its quick note goes for good once the
/// quick notes are caught up and it has been unlinked for a while.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quick_notes_keep_files_only_for_their_notes() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let h = hub(&alice).await;
    let made = ops::create_quick_note(&h, "Receipts: ![](scan.png) ![](keep.png)")
        .await
        .unwrap();
    let q = h.quick_notes().await.unwrap();
    let qid = q.lock().await.id().to_string();
    for f in ["scan.png", "keep.png"] {
        ops::put_attachment(&h, &qid, f, f.as_bytes(), None)
            .await
            .unwrap();
    }
    ops::put_document(&h, &qid, &made.path, "Receipts: ![](keep.png)", None)
        .await
        .unwrap();
    let now = notekpr_core::now_ms();
    // Just added: its link may still be on its way from another device.
    assert_eq!(ops::drop_unlinked_quick_files(&h, now).await.unwrap(), 0);
    let later = now + notekpr_core::quick::UNLINKED_FILE_GRACE_MS + 1;
    assert_eq!(ops::drop_unlinked_quick_files(&h, later).await.unwrap(), 1);
    let g = q.lock().await;
    assert!(g.find("keep.png").is_some());
    assert!(g.find("scan.png").is_none() && g.trashed().is_empty());
}
