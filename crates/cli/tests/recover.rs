//! Offline recovery: the encrypted export decrypts into plain files.

mod common;

use common::*;
use notekpr_cli::{manage, ops, recover};

#[tokio::test]
async fn an_encrypted_export_decrypts_into_plain_files() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Family").await.unwrap();
    let h = hub(&alice).await;
    ops::put_document(&h, "Family", "Plans/Trip.md", "Pack sunscreen ☀\n", None)
        .await
        .unwrap();
    ops::put_attachment(
        &h,
        "Family",
        "assets/photo.png",
        &[0x89, b'P', b'N', b'G'],
        None,
    )
    .await
    .unwrap();
    ops::add_comment(&h, "Family", "Plans/Trip", "sunscreen", "SPF 50?")
        .await
        .unwrap();
    for id in h.vault_ids() {
        h.push(&id).await.unwrap();
    }

    let export = recover::download_export(&alice).await.unwrap();
    let bundle = srv.path("alice-export.json");
    std::fs::write(&bundle, serde_json::to_vec(&export).unwrap()).unwrap();
    let export = recover::read_export(&bundle).unwrap();

    let out = srv.path("recovered");
    let report = recover::recover_with_device(&alice, &export, &out).unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    // The quick notes come back too (made when the app first started).
    assert_eq!(report.vaults, 2);
    assert_eq!(
        std::fs::read_to_string(out.join("Family/Plans/Trip.md")).unwrap(),
        "Pack sunscreen ☀\n"
    );
    assert_eq!(
        std::fs::read(out.join("Family/assets/photo.png")).unwrap(),
        vec![0x89, b'P', b'N', b'G']
    );
    // The comments, with their author's name.
    let md = std::fs::read_to_string(out.join("Family/_comments/Plans/Trip.md")).unwrap();
    assert!(
        md.contains("## On “sunscreen”") && md.contains("**Alice** · ") && md.contains("> SPF 50?"),
        "{md}"
    );
    // Never into a folder that already has content.
    assert!(recover::recover_with_device(&alice, &export, &out).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(out.join("Family/Plans/Trip.md"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "recovered plaintext must be private");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cli_saves_an_export_and_recovers_it_with_the_recovery_key() {
    use clap::Parser;
    use notekpr_cli::account::{self, LoginOptions};
    use notekpr_cli::cli::Cli;
    use notekpr_cli::commands;

    let srv = start().await;
    let dirs = srv.dirs("carol");
    let out = account::login(
        &dirs,
        "carol",
        LoginOptions {
            server: srv.url.clone(),
            dev_user: Some("carol".into()),
            device_name: Some("carol's test device".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let recovery_key = out
        .recovery_key
        .expect("a new account shows its recovery key");
    let carol = reopen(&srv, "carol", "carol").await;
    manage::create_vault(&carol, "Work").await.unwrap();
    let h = hub(&carol).await;
    ops::put_document(&h, "Work", "Ideas.md", "ship it\n", None)
        .await
        .unwrap();
    for id in h.vault_ids() {
        h.push(&id).await.unwrap();
    }
    let run = |args: &[&str]| {
        let mut argv = vec!["notekpr", "-q", "--profile", "carol"];
        argv.extend_from_slice(args);
        commands::run_in(Cli::try_parse_from(argv).unwrap(), srv.dirs("carol"))
    };

    let bundle = srv.path("carol.json");
    let b = bundle.to_str().unwrap();
    run(&["export-encrypted", "--out", b]).await.unwrap();
    let saved = std::fs::read_to_string(&bundle).unwrap();
    assert!(!saved.contains("ship it"), "the export is encrypted");
    assert!(
        run(&["export-encrypted", "--out", b]).await.is_err(),
        "no silent overwrite"
    );

    let wrong = srv.path("wrong.key");
    std::fs::write(
        &wrong,
        "AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA-AAAAA",
    )
    .unwrap();
    let out_dir = srv.path("from-key");
    let o = out_dir.to_str().unwrap();
    assert!(
        run(&[
            "recover",
            "--from",
            b,
            "--out",
            o,
            "--recovery-key-file",
            wrong.to_str().unwrap()
        ])
        .await
        .is_err()
    );

    let key_file = srv.path("carol.key");
    std::fs::write(&key_file, format!("{recovery_key}\n")).unwrap();
    run(&[
        "recover",
        "--from",
        b,
        "--out",
        o,
        "--recovery-key-file",
        key_file.to_str().unwrap(),
    ])
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(out_dir.join("Work/Ideas.md")).unwrap(),
        "ship it\n"
    );

    // With the signed-in device instead of the recovery key.
    let dev_dir = srv.path("from-device");
    run(&["recover", "--out", dev_dir.to_str().unwrap()])
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dev_dir.join("Work/Ideas.md")).unwrap(),
        "ship it\n"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_the_server_a_device_exports_and_recovers_from_its_local_copies() {
    use clap::Parser;
    use notekpr_cli::account::{self, LoginOptions};
    use notekpr_cli::cli::Cli;
    use notekpr_cli::commands;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let srv = start().await;
    // The device reaches the server through a proxy that can go away.
    let down = Arc::new(AtomicBool::new(false));
    let d = down.clone();
    let url = proxy(
        &srv.url,
        Arc::new(move |_, _| d.load(Ordering::SeqCst).then_some(Fault::Reply(502))),
    )
    .await;
    account::login(
        &srv.dirs("dana"),
        "dana",
        LoginOptions {
            server: url,
            dev_user: Some("dana".into()),
            device_name: Some("dana's laptop".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let dana = reopen(&srv, "dana", "dana").await;
    manage::create_vault(&dana, "Home").await.unwrap();
    let h = hub(&dana).await;
    ops::put_document(&h, "Home", "Plans/Trip.md", "Pack sunscreen\n", None)
        .await
        .unwrap();
    ops::put_document(&h, "Home", "Old.md", "outdated\n", None)
        .await
        .unwrap();
    ops::put_attachment(&h, "Home", "assets/kept.png", &[1, 2, 3], None)
        .await
        .unwrap();
    ops::put_attachment(&h, "Home", "assets/gone.png", &[4, 5, 6], None)
        .await
        .unwrap();
    ops::delete_document(&h, "Home", "Old.md").await.unwrap();
    for id in h.vault_ids() {
        h.push(&id).await.unwrap();
    }
    // A file this device never downloaded (as if added from elsewhere).
    {
        let v = h.resolve("Home").await.unwrap();
        let v = v.lock().await;
        let gone = v
            .replica
            .index
            .entries()
            .into_iter()
            .find(|e| e.path == "assets/gone.png")
            .unwrap();
        v.store().delete_blob(&gone.id).unwrap();
    }
    // The server goes away; an edit made now stays on the device, unsent.
    down.store(true, Ordering::SeqCst);
    ops::put_document(
        &h,
        "Home",
        "Plans/Trip.md",
        "Pack sunscreen and hats\n",
        None,
    )
    .await
    .unwrap();
    h.save_all().await;
    drop(h);

    let run = |args: &[&str]| {
        let mut argv = vec!["notekpr", "-q", "--profile", "dana"];
        argv.extend_from_slice(args);
        commands::run_in(Cli::try_parse_from(argv).unwrap(), srv.dirs("dana"))
    };
    let read = |p: std::path::PathBuf| std::fs::read(&p).unwrap_or_else(|_| panic!("{p:?}"));

    // `export` works from the local copy; the file it never had is left out.
    let exported = srv.path("exported");
    run(&["export", "Home", "--out", exported.to_str().unwrap()])
        .await
        .unwrap();
    assert_eq!(
        read(exported.join("Plans/Trip.md")),
        b"Pack sunscreen and hats\n"
    );
    assert_eq!(read(exported.join("assets/kept.png")), [1, 2, 3]);
    assert!(!exported.join("assets/gone.png").exists());

    // `recover` falls back to the device's copies, trash included.
    let recovered = srv.path("recovered");
    run(&["recover", "--out", recovered.to_str().unwrap()])
        .await
        .unwrap();
    let home = recovered.join("Home");
    assert_eq!(
        read(home.join("Plans/Trip.md")),
        b"Pack sunscreen and hats\n"
    );
    assert_eq!(read(home.join("assets/kept.png")), [1, 2, 3]);
    assert_eq!(read(home.join("_trash/Old.md")), b"outdated\n");
    assert!(!home.join("assets/gone.png").exists());

    // `--local` uses them even with the server back: the unsent edit shows
    // it didn't come from the server.
    down.store(false, Ordering::SeqCst);
    let local = srv.path("local");
    run(&["recover", "--local", "--out", local.to_str().unwrap()])
        .await
        .unwrap();
    assert_eq!(
        read(local.join("Home/Plans/Trip.md")),
        b"Pack sunscreen and hats\n"
    );
}

/// What an export holds is the server's word, so a recovery checks it as
/// the apps do. A vault key its owner never made, and what it opens, are
/// left out; without the vault's membership history, or with a shared
/// note's key from someone who isn't an owner, the content is recovered,
/// but in a folder marked unchecked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_recovery_checks_what_the_apps_check() {
    use notekpr_core::crypto::Purpose;
    use notekpr_core::doc::{EntryKind, IndexDoc, NoteDoc};
    use notekpr_core::engine::VaultMeta;
    use notekpr_core::keys::{self, VaultKey, VaultKeyring};
    use notekpr_core::memberlog;
    use notekpr_core::protocol::*;
    use notekpr_core::recover::{RecoveredVault, SHARED_DIR, UNCHECKED, decrypt_export};
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    let eve = login(&srv, "eve").await;
    manage::create_vault(&alice, "Family").await.unwrap();
    let h = hub(&alice).await;
    ops::put_document(&h, "Family", "Trip.md", "Pack sunscreen\n", None)
        .await
        .unwrap();
    for id in h.vault_ids() {
        h.push(&id).await.unwrap();
    }
    let v = h.resolve("Family").await.unwrap();
    let vid = v.lock().await.id().to_string();
    share_note(&alice, &v, "Trip", "bob", Role::Viewer, false)
        .await
        .unwrap();
    let family = |got: Vec<RecoveredVault>| got.into_iter().find(|v| v.vault_id == vid).unwrap();
    let has = |v: &RecoveredVault, path: &str| v.files.iter().any(|f| f.path == path);

    // As it is: checked, nothing to say.
    let export = recover::download_export(&alice).await.unwrap();
    let fam = family(decrypt_export(alice.account(), &export));
    assert_eq!(fam.name, "Family");
    assert!(fam.problems.is_empty(), "{:?}", fam.problems);
    assert!(has(&fam, "Trip.md"));

    // The server slips in a vault key of its own, made for Alice, and a
    // note under it.
    let mut forged = export.clone();
    let fv = forged
        .vaults
        .iter_mut()
        .find(|v| v.vault_id == vid)
        .unwrap();
    let epoch = fv.keys.current_epoch + 1;
    let fake = VaultKey::generate();
    fv.keys.envelopes.push(KeyEnvelope {
        user_id: alice.user_id.clone(),
        epoch,
        envelope: keys::wrap_vault_key(
            &fake,
            &vid,
            epoch,
            &alice.user_id,
            &alice.account().public().enc_public,
        )
        .unwrap(),
        signature: None,
        signed_by: None,
    });
    fv.keys.current_epoch = epoch;
    let mut ring = VaultKeyring::new(&vid);
    ring.insert(epoch, fake);
    let head = fv.changes.head_seq;
    let slipped = [
        (
            INDEX_OBJECT_ID,
            IndexDoc::new()
                .create("n9", "Important.md", EntryKind::Note, 1)
                .unwrap(),
        ),
        ("n9", NoteDoc::new().set_text("Wire the money").unwrap()),
    ];
    for (i, (object_id, update)) in slipped.into_iter().enumerate() {
        fv.changes.updates.push(Update {
            object_id: object_id.into(),
            seq: head + 1 + i as i64,
            author_id: alice.user_id.clone(),
            payload: ring.encrypt(Purpose::Update, object_id, &update).unwrap(),
            created_at: 0,
            signature: None,
        });
    }
    let fam = family(decrypt_export(alice.account(), &forged));
    assert!(
        !has(&fam, "Important.md"),
        "{:?}",
        fam.files.iter().map(|f| &f.path).collect::<Vec<_>>()
    );
    assert!(has(&fam, "Trip.md"));
    assert_eq!(fam.name, "Family");
    assert!(
        fam.problems
            .iter()
            .any(|p| p.contains("its owner never made")),
        "{:?}",
        fam.problems
    );

    // Or a key "from before" the first epoch, and the vault's name under it.
    let mut forged = export.clone();
    let fv = forged
        .vaults
        .iter_mut()
        .find(|v| v.vault_id == vid)
        .unwrap();
    let fake = VaultKey::generate();
    fv.keys.envelopes.push(KeyEnvelope {
        user_id: alice.user_id.clone(),
        epoch: 0,
        envelope: keys::wrap_vault_key(
            &fake,
            &vid,
            0,
            &alice.user_id,
            &alice.account().public().enc_public,
        )
        .unwrap(),
        signature: None,
        signed_by: None,
    });
    let mut ring = VaultKeyring::new(&vid);
    ring.insert(0, fake);
    fv.encrypted_meta = VaultMeta::named("Pay here").encrypt(&ring).unwrap();
    let fam = family(decrypt_export(alice.account(), &forged));
    assert_ne!(fam.name, "Pay here");
    assert!(
        fam.problems
            .iter()
            .any(|p| p.contains("its owner never made")),
        "{:?}",
        fam.problems
    );

    // Or leaves the vault's membership history out: recovered, but marked.
    let mut stripped = export.clone();
    for v in &mut stripped.vaults {
        v.member_log.clear();
    }
    let fam = family(decrypt_export(alice.account(), &stripped));
    assert_eq!(fam.name, format!("Family{UNCHECKED}"));
    assert!(
        fam.problems
            .iter()
            .any(|p| p.contains("which your apps have seen")),
        "{:?}",
        fam.problems
    );
    assert!(has(&fam, "Trip.md"));

    // Bob's export has the note shared with him, its key proven an owner's.
    let export = recover::download_export(&bob).await.unwrap();
    let shared = |got: Vec<RecoveredVault>| {
        got.into_iter()
            .find(|v| v.name.starts_with(SHARED_DIR))
            .unwrap()
    };
    let got = shared(decrypt_export(bob.account(), &export));
    assert_eq!(got.name, SHARED_DIR, "{:?}", got.problems);
    assert!(has(&got, "Trip.md"));
    // A newer key for him, signed by Eve: the server can't show her an
    // owner.
    let mut forged = export.clone();
    let n = &mut forged.shared_notes[0];
    let oid = n.keys.object_id.clone();
    let next = n.keys.envelopes.iter().map(|e| e.epoch).max().unwrap() + 1;
    let fake = VaultKey::generate();
    n.keys.envelopes.push(KeyEnvelope {
        user_id: bob.user_id.clone(),
        epoch: next,
        envelope: keys::wrap_vault_key(
            &fake,
            &keys::object_scope(&vid, &oid),
            next,
            &bob.user_id,
            &bob.account().public().enc_public,
        )
        .unwrap(),
        signature: Some(memberlog::sign_note_grant(
            eve.account(),
            &vid,
            &oid,
            &bob.user_id,
            next,
            &fake,
        )),
        signed_by: Some(eve.user_id.clone()),
    });
    n.keys.current_epoch = next;
    n.keys.signer_proof.clear();
    let got = shared(decrypt_export(bob.account(), &forged));
    assert_eq!(got.name, format!("{SHARED_DIR}{UNCHECKED}"));
    assert!(
        got.problems.iter().any(|p| p.contains("owns its vault")),
        "{:?}",
        got.problems
    );

    // With this device's keys: it pinned Alice's key, while the settings
    // it last synced name another (one taken on another device). This
    // device's pin counts, as in the apps: the genuine note is checked.
    let alice_key = alice.account().public().fingerprint();
    let eve_key = eve.account().public().fingerprint();
    bob.profiles
        .update(|p| {
            p.pins.insert(alice.user_id.clone(), alice_key.clone());
            p.synced_trust
                .pins
                .insert(alice.user_id.clone(), eve_key.clone());
        })
        .unwrap();
    let out = srv.path("bob-recovered");
    recover::recover_with_device(&bob, &export, &out).unwrap();
    assert!(
        out.join(SHARED_DIR).join("Trip.md").exists(),
        "{:?}",
        std::fs::read_dir(&out)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<Vec<_>>()
    );
}
