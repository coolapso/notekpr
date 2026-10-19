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
