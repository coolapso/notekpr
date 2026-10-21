//! Encrypted exports and offline recovery: GET /me/export and
//! `notekpr-server export-user`, decrypted with only the person's keys.

mod common;

use common::*;
use notekpr_core::crypto::Purpose;
use notekpr_core::doc::{EntryKind, IndexDoc, NoteDoc};
use notekpr_core::engine;
use notekpr_core::protocol::*;
use notekpr_core::recover::{self, RecoveredVault};
use reqwest::Method;

fn file<'a>(v: &'a RecoveredVault, path: &str) -> Option<&'a [u8]> {
    v.files
        .iter()
        .find(|f| f.path == path)
        .map(|f| f.data.as_slice())
}

#[tokio::test]
async fn people_can_recover_their_notes_offline() {
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let mut bob = new_user(&srv.base, "bob").await;
    let carol = new_user(&srv.base, "carol").await;
    let v = alice.create_vault("Family").await;
    let index = IndexDoc::new();
    let trip = NoteDoc::new();
    let i1 = index
        .create("n1", "Plans/Trip.md", EntryKind::Note, 1)
        .unwrap();
    let t1 = trip.set_text("Pack sunscreen").unwrap();
    let i2 = index
        .create_with(
            "a1",
            "assets/photo.png",
            EntryKind::Attachment,
            1,
            Some(3),
            Some("image/png"),
        )
        .unwrap();
    alice
        .push(
            &v,
            vec![
                alice.encrypt_update(&v, INDEX_OBJECT_ID, &i1),
                alice.encrypt_update(&v, "n1", &t1),
                alice.encrypt_update(&v, INDEX_OBJECT_ID, &i2),
            ],
        )
        .await
        .unwrap();
    let blob = alice
        .ring(&v)
        .encrypt(Purpose::Attachment, "a1", b"PNG")
        .unwrap();
    let st = alice
        .api
        .raw()
        .put(format!("{}/vaults/{v}/attachments/a1", srv.base))
        .bearer_auth(alice.api.token.clone().unwrap())
        .body(blob)
        .send()
        .await
        .unwrap()
        .status();
    assert!(st.is_success());

    // Bob joins, the key is rotated, and a note is written with the new key.
    alice.share(&v, &bob.id, Role::Editor).await.unwrap();
    bob.load_keys(&v).await.unwrap();
    alice.rotate(&v).await;
    let later = NoteDoc::new();
    let i3 = index.create("n2", "Later.md", EntryKind::Note, 2).unwrap();
    let t3 = later.set_text("written after the rotation").unwrap();
    alice
        .push(
            &v,
            vec![
                alice.encrypt_update(&v, INDEX_OBJECT_ID, &i3),
                alice.encrypt_update(&v, "n2", &t3),
            ],
        )
        .await
        .unwrap();

    // Carol gets only the trip note, shared individually.
    let head: ChangesResponse = alice.api.get(&format!("/vaults/{v}/changes?since=0")).await;
    let (doc, req) = engine::start_sharing(
        alice.ring(&v),
        "n1",
        &engine::ObjectMeta::titled("Trip"),
        &trip.encode_state(),
        head.head_seq,
    )
    .unwrap();
    let _: () = alice
        .api
        .put(&format!("/vaults/{v}/objects/n1/key"), &req)
        .await;
    let carol_pub: PublicUser = alice.api.get(&format!("/users/{}", carol.id)).await;
    let env = engine::object_share_envelope(&doc, "n1", &carol_pub).unwrap();
    let _: ObjectShareInfo = alice
        .api
        .post(
            &format!("/vaults/{v}/objects/n1/shares"),
            &AddObjectShareRequest {
                user_id: carol.id.clone(),
                role: Role::Viewer,
                envelope: env,
                log_entry: None,
            },
        )
        .await;

    // Alice: everything back from the export and her recovery key alone.
    let export: EncryptedExport = alice.api.get("/me/export").await;
    let account = recover::unlock_with_recovery_key(&export, &alice.recovery.to_display()).unwrap();
    let got = recover::decrypt_export(&account, &export);
    assert_eq!(got.len(), 1);
    let fam = &got[0];
    assert!(fam.problems.is_empty(), "{:?}", fam.problems);
    assert_eq!(file(fam, "Plans/Trip.md"), Some(&b"Pack sunscreen"[..]));
    assert_eq!(
        file(fam, "Later.md"),
        Some(&b"written after the rotation"[..])
    );
    assert_eq!(file(fam, "assets/photo.png"), Some(&b"PNG"[..]));

    // Bob, a member, reads the vault including content after the rotation.
    let export: EncryptedExport = bob.api.get("/me/export").await;
    let got = recover::decrypt_export(&bob.account, &export);
    assert_eq!(
        file(&got[0], "Later.md"),
        Some(&b"written after the rotation"[..])
    );

    // Carol gets only her shared note: no vault, no other notes or files.
    let export: EncryptedExport = carol.api.get("/me/export").await;
    assert!(export.vaults.is_empty());
    let got = recover::decrypt_export(&carol.account, &export);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].name, recover::SHARED_DIR);
    assert_eq!(got[0].files.len(), 1);
    assert_eq!(file(&got[0], "Trip.md"), Some(&b"Pack sunscreen"[..]));

    // Offline from a backup file with the server tool, then decrypted again.
    let bin = env!("CARGO_BIN_EXE_notekpr-server");
    let data = srv.dir.path();
    let out = tempfile::tempdir().unwrap();
    let backup = out.path().join("backup.db");
    let run = |args: &[&str]| {
        let o = std::process::Command::new(bin)
            .arg("--data-dir")
            .arg(data)
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    };
    run(&["backup", "--out", backup.to_str().unwrap()]);
    let bundle = out.path().join("alice.json");
    run(&[
        "export-user",
        "--backup",
        backup.to_str().unwrap(),
        "--user",
        "alice",
        "--out",
        bundle.to_str().unwrap(),
    ]);
    let export: EncryptedExport = serde_json::from_slice(&std::fs::read(&bundle).unwrap()).unwrap();
    let account = recover::unlock_with_recovery_key(&export, &alice.recovery.to_display()).unwrap();
    let got = recover::decrypt_export(&account, &export);
    assert_eq!(file(&got[0], "Plans/Trip.md"), Some(&b"Pack sunscreen"[..]));
    // Somebody else's recovery key doesn't open it.
    assert!(recover::unlock_with_recovery_key(&export, &bob.recovery.to_display()).is_err());

    // Without a session there is no export.
    let anon = reqwest::Client::new()
        .get(format!("{}/me/export", srv.base))
        .send()
        .await
        .unwrap();
    assert_eq!(anon.status(), 401);
    let _ = Method::GET;
}
