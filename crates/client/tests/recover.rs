//! Offline recovery: the encrypted export decrypts into plain files.

mod common;

use common::*;
use coolpsonotes_client::{manage, ops, recover};

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
    assert_eq!(report.vaults, 1);
    assert_eq!(
        std::fs::read_to_string(out.join("Family/Plans/Trip.md")).unwrap(),
        "Pack sunscreen ☀\n"
    );
    assert_eq!(
        std::fs::read(out.join("Family/assets/photo.png")).unwrap(),
        vec![0x89, b'P', b'N', b'G']
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
    use coolpsonotes_client::account::{self, LoginOptions};
    use coolpsonotes_client::cli::Cli;
    use coolpsonotes_client::commands;

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
        let mut argv = vec!["coolpsonotes", "-q", "--profile", "carol"];
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
