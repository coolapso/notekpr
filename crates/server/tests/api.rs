//! End-to-end tests of the server API with real client-side cryptography.

mod common;

use common::*;
use coolpsonotes_core::crypto::Purpose;
use coolpsonotes_core::doc::{EntryKind, IndexDoc, NoteDoc};
use coolpsonotes_core::engine::VaultMeta;
use coolpsonotes_core::keys::{self, DeviceKey};
use coolpsonotes_core::protocol::*;
use coolpsonotes_core::random_id;
use reqwest::Method;

#[tokio::test]
async fn private_vaults_are_isolated_and_sharing_works() {
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let mut bob = new_user(&srv.base, "bob").await;

    let v = alice.create_vault("Alice private").await;
    let index = IndexDoc::new();
    let note = NoteDoc::new();
    let u1 = index
        .create("n1", "Ideas/First.md", EntryKind::Note, 1)
        .unwrap();
    let u2 = note.set_text("# Hello\nsecret plan").unwrap();
    alice
        .push(
            &v,
            vec![
                alice.encrypt_update(&v, INDEX_OBJECT_ID, &u1),
                alice.encrypt_update(&v, "n1", &u2),
            ],
        )
        .await
        .unwrap();

    // Bob cannot see, read or write Alice's private vault.
    let bobs: Vec<VaultInfo> = bob.api.get("/vaults").await;
    assert!(bobs.is_empty());
    assert_eq!(
        bob.api
            .status::<()>(Method::GET, &format!("/vaults/{v}"), None)
            .await,
        404
    );
    assert_eq!(
        bob.api
            .status::<()>(Method::GET, &format!("/vaults/{v}/changes?since=0"), None)
            .await,
        404
    );
    assert_eq!(
        bob.api
            .status::<()>(Method::GET, &format!("/vaults/{v}/keys"), None)
            .await,
        404
    );
    assert!(bob.load_keys(&v).await.is_err());
    let forged = PushRequest {
        updates: vec![PushUpdate {
            object_id: "n1".into(),
            client_update_id: random_id(),
            payload: vec![1; 64],
            signature: None,
        }],
    };
    assert_eq!(
        bob.api
            .status(Method::POST, &format!("/vaults/{v}/updates"), Some(&forged))
            .await,
        404
    );

    // The server only holds ciphertext.
    let ch: ChangesResponse = alice.api.get(&format!("/vaults/{v}/changes?since=0")).await;
    for u in &ch.updates {
        assert!(!String::from_utf8_lossy(&u.payload).contains("secret plan"));
        assert!(!String::from_utf8_lossy(&u.payload).contains("First"));
    }
    let info: VaultInfo = alice.api.get(&format!("/vaults/{v}")).await;
    assert!(!String::from_utf8_lossy(&info.encrypted_meta).contains("Alice private"));

    // Share with Bob as editor.
    alice.share(&v, &bob.id, Role::Editor).await.unwrap();
    let bobs: Vec<VaultInfo> = bob.api.get("/vaults").await;
    assert_eq!(bobs.len(), 1);
    assert_eq!(bobs[0].role, Role::Editor);
    bob.load_keys(&v).await.unwrap();
    let meta = bob.ring(&v).decrypt_meta(&bobs[0].encrypted_meta).unwrap();
    assert!(String::from_utf8(meta).unwrap().contains("Alice private"));
    let (bidx, bnotes) = bob.read_vault(&v).await;
    assert_eq!(bidx.get("n1").unwrap().path, "Ideas/First.md");
    assert_eq!(bnotes["n1"].text(), "# Hello\nsecret plan");

    // Concurrent edits from both sides merge without loss.
    let bob_note = NoteDoc::from_updates([u2.as_slice()]).unwrap();
    let ua = note.set_text("# Hello\nsecret plan\nalice line").unwrap();
    let ub = bob_note.set_text("# Hello world\nsecret plan").unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n1", &ua)])
        .await
        .unwrap();
    bob.push(&v, vec![bob.encrypt_update(&v, "n1", &ub)])
        .await
        .unwrap();
    let (_, an) = alice.read_vault(&v).await;
    let (_, bn) = bob.read_vault(&v).await;
    assert_eq!(an["n1"].text(), "# Hello world\nsecret plan\nalice line");
    assert_eq!(an["n1"].text(), bn["n1"].text());

    // Members list shows both, with keys.
    let members: Vec<MemberInfo> = alice.api.get(&format!("/vaults/{v}/members")).await;
    assert_eq!(members.len(), 2);
    assert!(members.iter().all(|m| m.has_current_key));

    // Bob (editor) cannot manage members or delete the vault.
    let carol = new_user(&srv.base, "carol").await;
    assert_eq!(
        bob.share(&v, &carol.id, Role::Viewer).await.unwrap_err().0,
        403
    );
    assert_eq!(
        bob.api
            .status::<()>(Method::DELETE, &format!("/vaults/{v}"), None)
            .await,
        403
    );
}

#[tokio::test]
async fn viewers_are_read_only_and_revocation_rotates_keys() {
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let mut bob = new_user(&srv.base, "bob").await;
    let mut carol = new_user(&srv.base, "carol").await;
    let v = alice.create_vault("Family").await;
    let note = NoteDoc::new();
    let u = note.set_text("before rotation").unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n", &u)])
        .await
        .unwrap();
    alice.share(&v, &bob.id, Role::Editor).await.unwrap();
    alice.share(&v, &carol.id, Role::Viewer).await.unwrap();
    carol.load_keys(&v).await.unwrap();
    bob.load_keys(&v).await.unwrap();

    // Viewer: can read, cannot write, snapshot, upload or purge.
    let (_, n) = carol.read_vault(&v).await;
    assert_eq!(n["n"].text(), "before rotation");
    let w = carol.encrypt_update(&v, "n", &u);
    assert_eq!(carol.push(&v, vec![w]).await.unwrap_err().0, 403);
    let snap = CreateSnapshotRequest {
        snapshot_id: random_id(),
        covers_seq: 1,
        payload: vec![1; 50],
        signature: None,
        carries: false,
    };
    assert_eq!(
        carol
            .api
            .status(
                Method::POST,
                &format!("/vaults/{v}/objects/n/snapshots"),
                Some(&snap)
            )
            .await,
        403
    );
    let st = carol
        .api
        .raw()
        .put(format!("{}/vaults/{v}/attachments/a1", srv.base))
        .bearer_auth(carol.api.token.clone().unwrap())
        .body(vec![1u8; 10])
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(st.as_u16(), 403);
    assert_eq!(
        carol
            .api
            .status::<()>(Method::DELETE, &format!("/vaults/{v}/objects/n"), None)
            .await,
        403
    );

    // Remove Bob and rotate: Bob loses access; Carol can read old and new content.
    assert_eq!(
        alice
            .api
            .status::<()>(
                Method::DELETE,
                &format!("/vaults/{v}/members/{}", bob.id),
                None
            )
            .await,
        204
    );
    alice.rotate(&v).await;
    let u2 = note.set_text("after rotation").unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n", &u2)])
        .await
        .unwrap();
    assert_eq!(
        bob.api
            .status::<()>(Method::GET, &format!("/vaults/{v}/changes?since=0"), None)
            .await,
        404
    );
    assert!(bob.load_keys(&v).await.is_err());

    let ch: ChangesResponse = alice.api.get(&format!("/vaults/{v}/changes?since=0")).await;
    let last = ch.updates.last().unwrap();
    assert_eq!(
        coolpsonotes_core::crypto::sym_epoch(&last.payload).unwrap(),
        2
    );
    // Bob's old keyring (epoch 1) cannot decrypt post-rotation content even if he obtained it.
    assert!(
        bob.ring(&v)
            .decrypt(Purpose::Update, "n", &last.payload)
            .is_err()
    );

    carol.load_keys(&v).await.unwrap();
    assert_eq!(carol.ring(&v).epochs().collect::<Vec<_>>(), vec![1, 2]);
    let (_, n) = carol.read_vault(&v).await;
    assert_eq!(n["n"].text(), "after rotation");

    // A member added after rotation still reads history through the key chain.
    let mut dave = new_user(&srv.base, "dave").await;
    alice.share(&v, &dave.id, Role::Viewer).await.unwrap();
    dave.load_keys(&v).await.unwrap();
    assert_eq!(dave.ring(&v).epochs().collect::<Vec<_>>(), vec![1, 2]);
    let (_, n) = dave.read_vault(&v).await;
    assert_eq!(n["n"].text(), "after rotation");

    // Rotation must include exactly the current members.
    let st = alice
        .api
        .status(
            Method::POST,
            &format!("/vaults/{v}/keys/rotate"),
            Some(&RotateKeysRequest {
                new_epoch: 3,
                chain_link: vec![1; 60],
                encrypted_meta: vec![1; 60],
                envelopes: vec![],
                objects: vec![],
                log_entry: None,
                without: vec![],
            }),
        )
        .await;
    assert_eq!(st, 409);
}

#[tokio::test]
async fn pushes_are_idempotent_and_snapshots_compact() {
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let v = alice.create_vault("Log").await;
    let note = NoteDoc::new();
    let mut pushed = vec![];
    for i in 0..3 {
        let u = note.set_text(&format!("version {i}")).unwrap();
        let p = alice.encrypt_update(&v, "n", &u);
        pushed.push(p.clone());
        alice.push(&v, vec![p]).await.unwrap();
    }
    // Retrying the same update returns the same sequence number.
    let again = alice.push(&v, vec![pushed[1].clone()]).await.unwrap();
    assert_eq!(again.results[0].seq, 2);
    assert_eq!(again.head_seq, 3);

    // First snapshot covers seq 3; nothing is pruned yet (one generation kept).
    let s1 = alice
        .ring(&v)
        .encrypt(Purpose::Snapshot, "n", &note.encode_state())
        .unwrap();
    let _: SnapshotInfo = alice
        .api
        .post(
            &format!("/vaults/{v}/objects/n/snapshots"),
            &CreateSnapshotRequest {
                snapshot_id: random_id(),
                covers_seq: 3,
                payload: s1,
                signature: None,
                carries: false,
            },
        )
        .await;
    let u = note.set_text("version 3").unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n", &u)])
        .await
        .unwrap();
    let s2 = alice
        .ring(&v)
        .encrypt(Purpose::Snapshot, "n", &note.encode_state())
        .unwrap();
    let _: SnapshotInfo = alice
        .api
        .post(
            &format!("/vaults/{v}/objects/n/snapshots"),
            &CreateSnapshotRequest {
                snapshot_id: random_id(),
                covers_seq: 4,
                payload: s2,
                signature: None,
                carries: false,
            },
        )
        .await;
    let u = note.set_text("version 4").unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n", &u)])
        .await
        .unwrap();

    // Catch-up from scratch: latest snapshot + updates after it.
    let ch: ChangesResponse = alice.api.get(&format!("/vaults/{v}/changes?since=0")).await;
    assert_eq!(ch.snapshots.len(), 1);
    assert_eq!(ch.snapshots[0].covers_seq, 4);
    assert_eq!(
        ch.updates.iter().map(|u| u.seq).collect::<Vec<_>>(),
        vec![5]
    );
    let (_, notes) = decode_changes(alice.ring(&v), &[ch]);
    assert_eq!(notes["n"].text(), "version 4");

    // Catch-up from seq 4 needs no snapshot.
    let ch: ChangesResponse = alice.api.get(&format!("/vaults/{v}/changes?since=4")).await;
    assert!(ch.snapshots.is_empty());
    assert_eq!(ch.updates.len(), 1);

    // History lists both revisions, and each can be decrypted.
    let hist: Vec<SnapshotInfo> = alice
        .api
        .get(&format!("/vaults/{v}/objects/n/snapshots"))
        .await;
    assert_eq!(hist.len(), 2);
    let old: Snapshot = alice
        .api
        .get(&format!("/vaults/{v}/objects/n/snapshots/{}", hist[1].id))
        .await;
    let doc = NoteDoc::from_updates([alice
        .ring(&v)
        .decrypt(Purpose::Snapshot, "n", &old.payload)
        .unwrap()
        .as_slice()])
    .unwrap();
    assert_eq!(doc.text(), "version 2");

    // Pagination yields the same result.
    let mut pages = vec![];
    let mut since = 0;
    let mut first = true;
    loop {
        let ch: ChangesResponse = alice
            .api
            .get(&format!(
                "/vaults/{v}/changes?since={since}&limit=1&snapshots={first}"
            ))
            .await;
        first = false;
        since = ch.next_since;
        let more = ch.has_more;
        pages.push(ch);
        if !more {
            break;
        }
    }
    let (_, notes) = decode_changes(alice.ring(&v), &pages);
    assert_eq!(notes["n"].text(), "version 4");

    // Purge removes data; further writes to the object are rejected.
    assert_eq!(
        alice
            .api
            .status::<()>(Method::DELETE, &format!("/vaults/{v}/objects/n"), None)
            .await,
        204
    );
    let u = note.set_text("zombie").unwrap();
    let resp = alice
        .push(&v, vec![alice.encrypt_update(&v, "n", &u)])
        .await
        .unwrap();
    assert!(resp.results.is_empty());
    assert_eq!(resp.rejected[0].code, REJECT_PURGED);
    let ch: ChangesResponse = alice.api.get(&format!("/vaults/{v}/changes?since=5")).await;
    assert_eq!(
        ch.purged,
        vec![PurgedObject {
            object_id: "n".into(),
            seq: 6
        }]
    );
}

#[tokio::test]
async fn websocket_live_sync_acks_and_presence() {
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let mut bob = new_user(&srv.base, "bob").await;
    let v = alice.create_vault("Live").await;
    let note = NoteDoc::new();
    let u0 = note.set_text("start").unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n", &u0)])
        .await
        .unwrap();
    alice.share(&v, &bob.id, Role::Editor).await.unwrap();
    bob.load_keys(&v).await.unwrap();

    let mut wa = Ws::connect(&srv.ws, alice.api.token.as_ref().unwrap()).await;
    let mut wb = Ws::connect(&srv.ws, bob.api.token.as_ref().unwrap()).await;
    wa.send(&ClientMessage::Subscribe {
        vault_id: v.clone(),
        since: None,
        object_id: None,
    })
    .await;
    wb.send(&ClientMessage::Subscribe {
        vault_id: v.clone(),
        since: Some(0),
        object_id: None,
    })
    .await;
    let head = wa
        .until(|m| match m {
            ServerMessage::Synced { head_seq, .. } => Some(*head_seq),
            _ => None,
        })
        .await;
    assert_eq!(head, 1);
    // Bob catches up from 0 and reconstructs the note.
    let pages = wb
        .until(|m| match m {
            ServerMessage::Changes { changes, .. } => Some(changes.clone()),
            _ => None,
        })
        .await;
    let (_, n) = decode_changes(bob.ring(&v), &[pages]);
    assert_eq!(n["n"].text(), "start");
    wb.until(|m| matches!(m, ServerMessage::Synced { .. }).then_some(()))
        .await;

    // Alice pushes over the socket: she gets an ack, Bob gets the update.
    let bob_doc = NoteDoc::from_updates([u0.as_slice()]).unwrap();
    let u1 = note.set_text("start + alice").unwrap();
    let p = alice.encrypt_update(&v, "n", &u1);
    let cid = p.client_update_id.clone();
    wa.send(&ClientMessage::Push {
        vault_id: v.clone(),
        updates: vec![p.clone()],
    })
    .await;
    let ack = wa
        .until(|m| match m {
            ServerMessage::Ack { results, .. } => Some(results.clone()),
            _ => None,
        })
        .await;
    assert_eq!(
        ack,
        vec![PushResult {
            client_update_id: cid.clone(),
            seq: 2
        }]
    );
    let up = wb
        .until(|m| match m {
            ServerMessage::Update { update, .. } => Some(update.clone()),
            _ => None,
        })
        .await;
    assert_eq!(up.seq, 2);
    assert_eq!(up.author_id, alice.id);
    bob_doc
        .apply_update(
            &bob.ring(&v)
                .decrypt(Purpose::Update, "n", &up.payload)
                .unwrap(),
        )
        .unwrap();
    assert_eq!(bob_doc.text(), "start + alice");

    // A retried push is acked directly and not re-broadcast.
    wa.send(&ClientMessage::Push {
        vault_id: v.clone(),
        updates: vec![p],
    })
    .await;
    let ack = wa
        .until(|m| match m {
            ServerMessage::Ack { results, .. } => Some(results.clone()),
            _ => None,
        })
        .await;
    assert_eq!(ack[0].seq, 2);
    wb.expect_silence(300).await;

    // Presence is relayed to others only.
    wb.send(&ClientMessage::Awareness {
        vault_id: v.clone(),
        object_id: "n".into(),
        payload: vec![9, 9, 9],
    })
    .await;
    let (from, payload) = wa
        .until(|m| match m {
            ServerMessage::Awareness {
                from_user, payload, ..
            } => Some((from_user.clone(), payload.clone())),
            _ => None,
        })
        .await;
    assert_eq!(from, bob.id);
    assert_eq!(payload, vec![9, 9, 9]);

    // Revoking Bob drops his subscription immediately.
    assert_eq!(
        alice
            .api
            .status::<()>(
                Method::DELETE,
                &format!("/vaults/{v}/members/{}", bob.id),
                None
            )
            .await,
        204
    );
    wb.until(|m| {
        matches!(
            m,
            ServerMessage::VaultEvent {
                event: VaultEventKind::AccessRevoked,
                ..
            }
        )
        .then_some(())
    })
    .await;
    let u2 = note.set_text("after bob left").unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n", &u2)])
        .await
        .unwrap();
    loop {
        match tokio::time::timeout(std::time::Duration::from_millis(300), wb.recv()).await {
            Err(_) => break,
            Ok(ServerMessage::Update { .. }) => panic!("revoked member received an update"),
            Ok(_) => {}
        }
    }
    // Unauthenticated sockets are rejected.
    let (mut raw, _) = tokio_tungstenite::connect_async(&srv.ws).await.unwrap();
    use futures::SinkExt;
    raw.send(tokio_tungstenite::tungstenite::Message::Text(
        r#"{"type":"auth","token":"nope"}"#.into(),
    ))
    .await
    .unwrap();
    use futures::StreamExt;
    let m = raw.next().await.unwrap().unwrap();
    assert!(m.to_text().unwrap().contains("unauthorized"));
}

#[tokio::test]
async fn live_connections_are_told_when_the_server_restarts() {
    use futures::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    let srv = start().await;
    let alice = new_user(&srv.base, "alice").await;
    let token = alice.api.token.clone().unwrap();
    let mut ws = Ws::connect(&srv.ws, &token).await;
    async fn close_code(ws: &mut Ws) -> Option<u16> {
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), ws.stream.next())
                .await
                .expect("no close within 5 s")
            {
                Some(Ok(Message::Close(f))) => return f.map(|f| f.code.into()),
                Some(Ok(_)) => continue,
                _ => return None,
            }
        }
    }
    // Shutting down waits until every connection was told.
    srv.state
        .close_connections(std::time::Duration::from_secs(3))
        .await;
    assert_eq!(srv.state.hub.connection_count(), 0);
    assert_eq!(close_code(&mut ws).await, Some(CLOSE_RESTARTING));
    // Someone connecting meanwhile hears the same right away.
    let mut late = Ws::connect(&srv.ws, &token).await;
    assert_eq!(close_code(&mut late).await, Some(CLOSE_RESTARTING));
}

#[tokio::test]
async fn device_enrollment_and_recovery() {
    let srv = start().await;
    let alice = new_user(&srv.base, "alice").await;
    // A second browser signs in (new session) and registers a pending device.
    let (api2, _) = dev_login(&srv.base, "alice").await;
    let dev2 = DeviceKey::generate();
    let dev2_id = random_id();
    let _: () = api2
        .post(
            "/devices",
            &DeviceRegistration {
                device_id: dev2_id.clone(),
                name: "laptop".into(),
                enc_public: dev2.public.to_vec(),
            },
        )
        .await;
    assert_eq!(
        api2.status::<()>(Method::GET, &format!("/devices/{dev2_id}/envelope"), None)
            .await,
        409
    );

    // Both devices display the same verification code.
    let devices: Vec<DeviceInfo> = alice.api.get("/devices").await;
    let pending = devices.iter().find(|d| d.id == dev2_id).unwrap();
    assert_eq!(pending.status, DeviceStatus::Pending);
    let shown_on_old = keys::device_verification_code(
        &alice.id,
        &pending.id,
        pending.enc_public.as_slice().try_into().unwrap(),
        &alice.account.public(),
    );
    // The new device computes it from the published account key.
    let me: MeResponse = api2.get("/me").await;
    let published = me.account_key.unwrap().verify().unwrap();
    assert_eq!(
        shown_on_old,
        keys::device_verification_code(&alice.id, &dev2_id, &dev2.public, &published)
    );

    // A session without the account secret cannot activate a device.
    let bogus = coolpsonotes_core::keys::AccountSecret::generate();
    let bad = ActivateDeviceRequest {
        account_envelope: vec![1; 100],
        signature: bogus
            .sign_device_approval(&alice.id, &dev2_id, &dev2.public)
            .to_vec(),
    };
    assert_eq!(
        api2.status(
            Method::POST,
            &format!("/devices/{dev2_id}/activate"),
            Some(&bad)
        )
        .await,
        403
    );

    // The existing device approves.
    let req = ActivateDeviceRequest {
        account_envelope: keys::wrap_account_for_device(
            &alice.account,
            &dev2.public,
            &alice.id,
            &dev2_id,
        )
        .unwrap(),
        signature: alice
            .account
            .sign_device_approval(&alice.id, &dev2_id, &dev2.public)
            .to_vec(),
    };
    let _: () = alice
        .api
        .post(&format!("/devices/{dev2_id}/activate"), &req)
        .await;
    let env: DeviceEnvelope = api2.get(&format!("/devices/{dev2_id}/envelope")).await;
    let acct = dev2
        .unwrap_account(&env.account_envelope, &alice.id, &dev2_id)
        .unwrap();
    assert_eq!(acct.public(), alice.account.public());

    // Device loss: a third device recovers with the recovery key alone.
    let (api3, _) = dev_login(&srv.base, "alice").await;
    let rec: RecoveryEnvelope = api3.get("/me/recovery").await;
    let shown = alice.recovery.to_display();
    let rk = coolpsonotes_core::keys::RecoveryKey::parse(&shown).unwrap();
    let recovered = rk
        .unwrap_account(&rec.recovery_envelope, &alice.id)
        .unwrap();
    assert_eq!(recovered.public(), alice.account.public());
    let dev3 = DeviceKey::generate();
    let dev3_id = random_id();
    let _: () = api3
        .post(
            "/devices",
            &DeviceRegistration {
                device_id: dev3_id.clone(),
                name: "new phone".into(),
                enc_public: dev3.public.to_vec(),
            },
        )
        .await;
    let req = ActivateDeviceRequest {
        account_envelope: keys::wrap_account_for_device(
            &recovered,
            &dev3.public,
            &alice.id,
            &dev3_id,
        )
        .unwrap(),
        signature: recovered
            .sign_device_approval(&alice.id, &dev3_id, &dev3.public)
            .to_vec(),
    };
    let _: () = api3
        .post(&format!("/devices/{dev3_id}/activate"), &req)
        .await;

    // Removing a device revokes its sessions.
    assert_eq!(
        alice
            .api
            .status::<()>(Method::DELETE, &format!("/devices/{dev2_id}"), None)
            .await,
        204
    );
    assert_eq!(api2.status::<()>(Method::GET, "/me", None).await, 401);
    assert_eq!(api3.status::<()>(Method::GET, "/me", None).await, 200);
    // ...and says so explicitly, so clients can tell it from any other 404
    // before they forget the device's keys.
    let r = api3
        .req(Method::GET, &format!("/devices/{dev2_id}/envelope"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 404);
    let body: ApiError = r.json().await.unwrap();
    assert_eq!(body.error, "device_revoked");

    // Rotating the recovery key requires an account signature.
    let new_rk = coolpsonotes_core::keys::RecoveryKey::generate();
    let env = new_rk.wrap_account(&alice.account, &alice.id);
    let bad = RecoveryUpdate {
        recovery_envelope: env.clone(),
        signature: vec![0; 64],
        version: None,
    };
    assert_eq!(
        alice
            .api
            .status(Method::PUT, "/me/recovery", Some(&bad))
            .await,
        403
    );
    let good = RecoveryUpdate {
        signature: alice.account.sign_recovery_update(&alice.id, &env).to_vec(),
        recovery_envelope: env.clone(),
        version: None,
    };
    let put = |u: &RecoveryUpdate| {
        let (api, u) = (&alice.api, u.clone());
        async move { api.status(Method::PUT, "/me/recovery", Some(&u)).await }
    };
    assert_eq!(put(&good).await, 204);
    // Versioned updates only go forward [S-L8]: once one is stored, neither
    // an older nor an unversioned (replayed) update replaces it.
    let (_, v10) = coolpsonotes_core::engine::rotate_recovery(&alice.account, &alice.id, 10);
    let mut forged = v10.clone();
    forged.version = Some(11);
    assert_eq!(put(&forged).await, 403, "the version is signed");
    assert_eq!(put(&v10).await, 204);
    assert_eq!(put(&v10).await, 409, "replayed");
    let (_, v5) = coolpsonotes_core::engine::rotate_recovery(&alice.account, &alice.id, 5);
    assert_eq!(put(&v5).await, 409, "older");
    assert_eq!(put(&good).await, 409, "unversioned, after a versioned one");
    let (_, v11) = coolpsonotes_core::engine::rotate_recovery(&alice.account, &alice.id, 11);
    assert_eq!(put(&v11).await, 204);
}

#[tokio::test]
async fn device_authorization_grant_for_cli() {
    let srv = start().await;
    let alice = new_user(&srv.base, "alice").await;
    let http = reqwest::Client::new();
    let da: DeviceAuthorizationResponse = http
        .post(format!("{}/oauth/device_authorization", srv.base))
        .form(&[
            ("client_id", "coolpsonotes-cli"),
            ("client_name", "test laptop"),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let poll = || async {
        http.post(format!("{}/oauth/token", srv.base))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", da.device_code.as_str()),
            ])
            .send()
            .await
            .unwrap()
    };
    let r = poll().await;
    assert_eq!(r.status().as_u16(), 400);
    assert_eq!(
        r.json::<OAuthError>().await.unwrap().error,
        "authorization_pending"
    );
    let r = poll().await;
    assert_eq!(r.json::<OAuthError>().await.unwrap().error, "slow_down");

    let look: DeviceCodeLookup = alice
        .api
        .get(&format!(
            "/oauth/device?user_code={}",
            da.user_code.to_lowercase()
        ))
        .await;
    assert_eq!(look.client_name, "test laptop");
    let _: () = alice
        .api
        .post(
            "/oauth/device/decision",
            &DeviceCodeDecision {
                user_code: da.user_code.clone(),
                approve: true,
            },
        )
        .await;
    let tok: TokenResponse = poll().await.json().await.unwrap();
    let mut cli = Api::new(&srv.base);
    cli.token = Some(tok.access_token);
    let me: MeResponse = cli.get("/me").await;
    assert_eq!(me.user.id, alice.id);
    assert_eq!(me.session.kind, "cli");
    // Device codes are single use.
    assert_eq!(
        poll().await.json::<OAuthError>().await.unwrap().error,
        "invalid_grant"
    );
}

#[tokio::test]
async fn agents_are_scoped_to_shared_vaults() {
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let shared = alice.create_vault("For the agent").await;
    let private = alice.create_vault("Diary").await;
    let note = NoteDoc::new();
    let u = note.set_text("agent may read this").unwrap();
    alice
        .push(&shared, vec![alice.encrypt_update(&shared, "n", &u)])
        .await
        .unwrap();

    let created: CreateAgentResponse = alice
        .api
        .post(
            "/agents",
            &CreateAgentRequest {
                name: "Assistant".into(),
            },
        )
        .await;
    let http = reqwest::Client::new();
    let tok = http
        .post(format!("{}/oauth/token", srv.base))
        .basic_auth(
            &created.credential.client_id,
            Some(&created.credential.client_secret),
        )
        .form(&[("grant_type", "client_credentials")])
        .send()
        .await
        .unwrap();
    assert_eq!(tok.status().as_u16(), 200);
    let tok: TokenResponse = tok.json().await.unwrap();
    let mut agent_api = Api::new(&srv.base);
    agent_api.token = Some(tok.access_token);

    // The agent generates its own key pair; no device or recovery needed.
    let agent_secret = coolpsonotes_core::keys::AccountSecret::generate();
    let aid = created.agent.id.clone();
    let _: () = agent_api
        .put(
            "/me/account-key",
            &AccountSetupRequest {
                key: PublicAccountKey::new(
                    &aid,
                    1,
                    &agent_secret.public(),
                    &agent_secret.sign_account_key(&aid, 1),
                ),
                recovery_envelope: None,
                device: None,
            },
        )
        .await;
    // Agents cannot create vaults or manage agents.
    assert_eq!(
        agent_api.status::<()>(Method::GET, "/agents", None).await,
        403
    );

    alice.share(&shared, &aid, Role::Viewer).await.unwrap();
    let vaults: Vec<VaultInfo> = agent_api.get("/vaults").await;
    assert_eq!(
        vaults.iter().map(|v| v.id.clone()).collect::<Vec<_>>(),
        vec![shared.clone()]
    );
    assert_eq!(
        agent_api
            .status::<()>(
                Method::GET,
                &format!("/vaults/{private}/changes?since=0"),
                None
            )
            .await,
        404
    );

    let mut agent = User {
        api: agent_api.clone(),
        id: aid.clone(),
        name: "agent".into(),
        account: agent_secret,
        device: DeviceKey::generate(),
        device_id: String::new(),
        recovery: coolpsonotes_core::keys::RecoveryKey::generate(),
        keyrings: Default::default(),
    };
    agent.load_keys(&shared).await.unwrap();
    let (_, n) = agent.read_vault(&shared).await;
    assert_eq!(n["n"].text(), "agent may read this");
    let w = agent.encrypt_update(&shared, "n", &note.set_text("agent edit").unwrap());
    assert_eq!(
        agent.push(&shared, vec![w.clone()]).await.unwrap_err().0,
        403
    );

    // Promote to editor: writes are attributed to the agent.
    let _: () = alice
        .api
        .call(
            Method::PATCH,
            &format!("/vaults/{shared}/members/{aid}"),
            Some(&UpdateMemberRequest {
                role: Role::Editor,
                log_entry: None,
            }),
        )
        .await
        .unwrap();
    agent.push(&shared, vec![w]).await.unwrap();
    let ch: ChangesResponse = alice
        .api
        .get(&format!("/vaults/{shared}/changes?since=1"))
        .await;
    assert_eq!(ch.updates[0].author_id, aid);

    // Revoking the credential ends the agent's sessions; other users can't see the agent.
    let bob = new_user(&srv.base, "bob").await;
    assert_eq!(
        bob.api
            .status::<()>(Method::GET, &format!("/users/{aid}"), None)
            .await,
        404
    );
    let st = alice
        .api
        .status::<()>(
            Method::DELETE,
            &format!("/agents/{aid}/credentials/{}", created.credential.client_id),
            None,
        )
        .await;
    assert_eq!(st, 204);
    assert_eq!(
        agent_api.status::<()>(Method::GET, "/vaults", None).await,
        401
    );
}

#[tokio::test]
async fn attachments_prefs_admin_and_limits() {
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let bob = new_user(&srv.base, "bob").await;
    let v = alice.create_vault("Files").await;
    let blob = alice
        .ring(&v)
        .encrypt(Purpose::Attachment, "att1", b"PNG bytes")
        .unwrap();
    let url = format!("{}/vaults/{v}/attachments/att1", srv.base);
    let st = alice
        .api
        .raw()
        .put(&url)
        .bearer_auth(alice.api.token.clone().unwrap())
        .body(blob.clone())
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(st.as_u16(), 201);
    let got = alice
        .api
        .raw()
        .get(&url)
        .bearer_auth(alice.api.token.clone().unwrap())
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(
        alice
            .ring(&v)
            .decrypt(Purpose::Attachment, "att1", &got)
            .unwrap(),
        b"PNG bytes"
    );
    let st = bob
        .api
        .raw()
        .get(&url)
        .bearer_auth(bob.api.token.clone().unwrap())
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(st.as_u16(), 404);

    // Encrypted prefs with optimistic concurrency.
    let p: PrefsBlob = alice.api.get("/me/prefs").await;
    assert_eq!(p.version, 0);
    let ct = alice
        .account
        .seal_private(&alice.id, br#"{"favorites":[]}"#);
    let p: PrefsBlob = alice
        .api
        .put(
            "/me/prefs",
            &PutPrefsRequest {
                ciphertext: ct.clone(),
                expected_version: 0,
            },
        )
        .await;
    assert_eq!(p.version, 1);
    assert_eq!(
        alice
            .api
            .status(
                Method::PUT,
                "/me/prefs",
                Some(&PutPrefsRequest {
                    ciphertext: ct,
                    expected_version: 0
                })
            )
            .await,
        409
    );

    // First user is admin; admins see accounts, not content.
    let users: Vec<AdminUser> = alice.api.get("/admin/users").await;
    assert_eq!(users.len(), 2);
    assert_eq!(
        bob.api
            .status::<()>(Method::GET, "/admin/users", None)
            .await,
        403
    );
    let stats: AdminStats = alice.api.get("/admin/stats").await;
    assert_eq!(stats.vaults, 1);
    assert_eq!(
        alice
            .api
            .status::<()>(Method::GET, &format!("/vaults/{v}/audit"), None)
            .await,
        200
    );

    // Oversized updates are rejected.
    let big = PushRequest {
        updates: vec![PushUpdate {
            object_id: "n".into(),
            client_update_id: random_id(),
            payload: vec![0; 3 * 1024 * 1024],
            signature: None,
        }],
    };
    let resp = alice.push(&v, big.updates).await.unwrap();
    assert!(resp.results.is_empty());
    assert_eq!(resp.rejected[0].code, REJECT_TOO_LARGE);
    // Unknown API routes are JSON 404s; unauthenticated calls are 401.
    assert_eq!(
        alice.api.status::<()>(Method::GET, "/nope", None).await,
        404
    );
    assert_eq!(
        Api::new(&srv.base)
            .status::<()>(Method::GET, "/vaults", None)
            .await,
        401
    );
}

#[tokio::test]
async fn dev_login_is_disabled_outside_dev_mode() {
    let srv = start_with(|c| c.dev_mode = false).await;
    let st = Api::new(&srv.base)
        .status(
            Method::POST,
            "/auth/dev-login",
            Some(&DevLoginRequest {
                username: "x".into(),
                display_name: None,
            }),
        )
        .await;
    assert_eq!(st, 404);
    let info: ServerInfo = Api::new(&srv.base).get("/info").await;
    assert!(!info.dev_mode);
}

#[tokio::test]
async fn backup_and_restore_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("live");
    let mut cfg = coolpsonotes_server::config::Config::for_tests(data.clone());
    cfg.dev_mode = true;
    let (app, state) = coolpsonotes_server::build(cfg, None).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let base = format!("http://{addr}/api/v1");
    let mut alice = new_user(&base, "alice").await;
    let v = alice.create_vault("Backed up").await;
    let note = NoteDoc::new();
    let u = note.set_text("survives a restore").unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n", &u)])
        .await
        .unwrap();
    let blob = alice
        .ring(&v)
        .encrypt(Purpose::Attachment, "a1", b"file bytes")
        .unwrap();
    let st = alice
        .api
        .raw()
        .put(format!("{base}/vaults/{v}/attachments/a1"))
        .bearer_auth(alice.api.token.clone().unwrap())
        .body(blob)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(st.as_u16(), 201);

    // Online backup while the server is running.
    let backup = dir.path().join("backup.db");
    coolpsonotes_server::db::backup_to(&state.db, &backup)
        .await
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&std::fs::read(&backup).unwrap()).contains("survives a restore")
    );

    // Restore into a fresh data directory and run a new server on it.
    let restored = dir.path().join("restored");
    let target = restored.join("coolpsonotes.db");
    assert!(
        coolpsonotes_server::db::restore_from(&target, &backup)
            .await
            .unwrap()
            .is_none()
    );
    let cfg2 = coolpsonotes_server::config::Config::for_tests(restored.clone());
    let (app2, _s2) = coolpsonotes_server::build(cfg2, None).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr2 = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app2.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    // The old session token and keys keep working against the restored server.
    alice.api.base = format!("http://{addr2}/api/v1");
    alice.load_keys(&v).await.unwrap();
    let (_, notes) = alice.read_vault(&v).await;
    assert_eq!(notes["n"].text(), "survives a restore");
    let got = alice
        .api
        .raw()
        .get(format!("{}/vaults/{v}/attachments/a1", alice.api.base))
        .bearer_auth(alice.api.token.clone().unwrap())
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(
        alice
            .ring(&v)
            .decrypt(Purpose::Attachment, "a1", &got)
            .unwrap(),
        b"file bytes"
    );
    // Restoring over an existing database keeps the previous copy aside.
    let aside = coolpsonotes_server::db::restore_from(
        &dir.path().join("restored2/coolpsonotes.db"),
        &backup,
    )
    .await
    .unwrap();
    assert!(aside.is_none());
    let aside = coolpsonotes_server::db::restore_from(
        &dir.path().join("restored2/coolpsonotes.db"),
        &backup,
    )
    .await
    .unwrap();
    assert!(aside.is_some_and(|p| p.exists()));
}

#[tokio::test]
async fn individual_note_sharing() {
    use coolpsonotes_core::engine;
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let mut bob = new_user(&srv.base, "bob").await;
    let carol = new_user(&srv.base, "carol").await;
    let v = alice.create_vault("Private").await;
    let index = IndexDoc::new();
    let n1 = NoteDoc::new();
    let n2 = NoteDoc::new();
    let ui = index
        .create("n1", "Plans/Trip.md", EntryKind::Note, 1)
        .unwrap();
    let u1 = n1.set_text("trip to the coast").unwrap();
    let u2 = n2.set_text("private diary").unwrap();
    alice
        .push(
            &v,
            vec![
                alice.encrypt_update(&v, INDEX_OBJECT_ID, &ui),
                alice.encrypt_update(&v, "n1", &u1),
                alice.encrypt_update(&v, "n2", &u2),
            ],
        )
        .await
        .unwrap();
    let blob = alice
        .ring(&v)
        .encrypt(Purpose::Attachment, "a1", b"photo")
        .unwrap();
    alice
        .api
        .raw()
        .put(format!("{}/vaults/{v}/attachments/a1", srv.base))
        .bearer_auth(alice.api.token.clone().unwrap())
        .body(blob)
        .send()
        .await
        .unwrap();

    // Some history from before sharing (encrypted with the vault key).
    let pre = CreateSnapshotRequest {
        snapshot_id: random_id(),
        covers_seq: 3,
        payload: alice
            .ring(&v)
            .encrypt(Purpose::Snapshot, "n1", &n1.encode_state())
            .unwrap(),
        signature: None,
        carries: false,
    };
    assert_eq!(
        alice
            .api
            .status(
                Method::POST,
                &format!("/vaults/{v}/objects/n1/snapshots"),
                Some(&pre)
            )
            .await,
        201
    );

    // A snapshot that misses earlier changes of the note is refused, and so
    // is one older than the newest existing snapshot (covers 3): readers would
    // otherwise get that one, which they can't decrypt.
    for covers in [1, 2] {
        let (_, bad_req) = engine::start_sharing(
            alice.ring(&v),
            "n1",
            &engine::ObjectMeta::titled("Trip"),
            &n1.encode_state(),
            covers,
        )
        .unwrap();
        assert_eq!(
            alice
                .api
                .status(
                    Method::PUT,
                    &format!("/vaults/{v}/objects/n1/key"),
                    Some(&bad_req)
                )
                .await,
            412,
            "covers_seq {covers}"
        );
    }

    // Alice shares n1 only.
    let (ring, req) = engine::start_sharing(
        alice.ring(&v),
        "n1",
        &engine::ObjectMeta::titled("Trip"),
        &n1.encode_state(),
        3,
    )
    .unwrap();
    let _: () = alice
        .api
        .put(&format!("/vaults/{v}/objects/n1/key"), &req)
        .await;
    let bob_pub: PublicUser = alice.api.get(&format!("/users/{}", bob.id)).await;
    let env = engine::object_share_envelope(&ring, "n1", &bob_pub).unwrap();
    let _: ObjectShareInfo = alice
        .api
        .post(
            &format!("/vaults/{v}/objects/n1/shares"),
            &AddObjectShareRequest {
                user_id: bob.id.clone(),
                role: Role::Editor,
                envelope: env,
                log_entry: None,
            },
        )
        .await;
    // Adding a vault member as a note recipient is pointless and refused.
    let shares: Vec<ObjectShareInfo> = alice
        .api
        .get(&format!("/vaults/{v}/objects/n1/shares"))
        .await;
    assert_eq!(shares.len(), 1);

    // Bob sees it under "shared with me" and can read it with the document key only.
    let mine: Vec<SharedNote> = bob.api.get("/shares").await;
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].owner.id, alice.id);
    let keys: ObjectKeysResponse = bob.api.get(&format!("/vaults/{v}/objects/n1/keys")).await;
    let bring =
        engine::object_keyring_for_recipient(&bob.account, &bob.id, &v, &keys, None).unwrap();
    assert_eq!(
        engine::ObjectMeta::decrypt(&bring, "n1", mine[0].encrypted_meta.as_ref().unwrap())
            .unwrap()
            .title,
        "Trip"
    );
    let mut br = engine::VaultReplica::for_shared_notes(&v);
    br.role = keys.role;
    br.doc_keys.insert("n1".into(), bring.clone());
    let ch: ChangesResponse = bob
        .api
        .get(&format!("/vaults/{v}/objects/n1/changes?since=0"))
        .await;
    assert!(ch.updates.iter().all(|u| u.object_id == "n1"));
    let (applied, errs) = br.apply_changes(&ch);
    assert!(errs.is_empty(), "{errs:?}");
    assert!(applied.objects.contains("n1"));
    assert_eq!(br.note_text("n1"), "trip to the coast");
    // The note's history from before it was shared is never handed out.
    let ch: ChangesResponse = bob
        .api
        .get(&format!(
            "/vaults/{v}/objects/n1/changes?since=0&snapshots=false"
        ))
        .await;
    assert!(ch.updates.is_empty() && ch.snapshots.is_empty());
    let bob_snaps: Vec<SnapshotInfo> = bob
        .api
        .get(&format!("/vaults/{v}/objects/n1/snapshots"))
        .await;
    let alice_snaps: Vec<SnapshotInfo> = alice
        .api
        .get(&format!("/vaults/{v}/objects/n1/snapshots"))
        .await;
    assert_eq!((bob_snaps.len(), alice_snaps.len()), (1, 2));
    assert_eq!(
        bob.api
            .status::<()>(
                Method::GET,
                &format!("/vaults/{v}/objects/n1/snapshots/{}", pre.snapshot_id),
                None
            )
            .await,
        404
    );
    let st: ObjectState = bob.api.get(&format!("/vaults/{v}/objects/n1")).await;
    assert!(st.updates.is_empty() && st.snapshot.is_some_and(|s| s.id == bob_snaps[0].id));

    // Nothing else of the vault is reachable.
    for path in [
        format!("/vaults/{v}"),
        format!("/vaults/{v}/changes?since=0"),
        format!("/vaults/{v}/keys"),
        format!("/vaults/{v}/members"),
        format!("/vaults/{v}/objects/n2"),
        format!("/vaults/{v}/objects/index"),
        format!("/vaults/{v}/objects/n2/keys"),
        format!("/vaults/{v}/attachments/a1"),
        format!("/vaults/{v}/objects/n1/shares"),
    ] {
        assert_eq!(
            bob.api.status::<()>(Method::GET, &path, None).await,
            404,
            "{path}"
        );
    }
    let vaults: Vec<VaultInfo> = bob.api.get("/vaults").await;
    assert!(vaults.is_empty());
    // Bob can't share it further or change its key.
    let carol_pub: PublicUser = alice.api.get(&format!("/users/{}", carol.id)).await;
    let env = engine::object_share_envelope(&bring, "n1", &carol_pub).unwrap();
    let st = bob
        .api
        .status(
            Method::POST,
            &format!("/vaults/{v}/objects/n1/shares"),
            Some(&AddObjectShareRequest {
                user_id: carol.id.clone(),
                role: Role::Viewer,
                envelope: env,
                log_entry: None,
            }),
        )
        .await;
    assert_eq!(st, 404);

    // Live: Bob subscribes to the single note and only gets its activity.
    let mut wb = Ws::connect(&srv.ws, bob.api.token.as_ref().unwrap()).await;
    wb.send(&ClientMessage::Subscribe {
        vault_id: v.clone(),
        since: None,
        object_id: Some("n1".into()),
    })
    .await;
    wb.until(|m| matches!(m, ServerMessage::Synced { .. }).then_some(()))
        .await;
    // Bob can't subscribe to the whole vault.
    wb.send(&ClientMessage::Subscribe {
        vault_id: v.clone(),
        since: Some(0),
        object_id: None,
    })
    .await;
    wb.until(|m| {
        matches!(m, ServerMessage::Error { code, .. } if code == "not_found").then_some(())
    })
    .await;
    let mut ar = engine::VaultReplica::new(
        &v,
        Role::Owner,
        alice.ring(&v).clone(),
        VaultMeta::named("Private"),
    );
    let vkeys: VaultKeysResponse = alice.api.get(&format!("/vaults/{v}/keys")).await;
    ar.doc_keys = engine::object_keyrings(alice.ring(&v), &vkeys);
    ar.shared = engine::shared_objects(&vkeys);
    assert!(ar.is_shared_object("n1"));
    assert!(!ar.is_shared_object("n2"));
    let p_private = ar
        .local_change("n2", n2.set_text("private diary, more").unwrap())
        .unwrap();
    alice.push(&v, vec![p_private]).await.unwrap();
    let p_shared = ar
        .local_change("n1", n1.set_text("trip to the coast in May").unwrap())
        .unwrap();
    alice.push(&v, vec![p_shared]).await.unwrap();
    let up = wb
        .until(|m| match m {
            ServerMessage::Update { update, .. } => Some(update.clone()),
            _ => None,
        })
        .await;
    assert_eq!(up.object_id, "n1");
    br.apply_update(&up).unwrap();
    assert_eq!(br.note_text("n1"), "trip to the coast in May");

    // A client that still encrypts the shared note with the vault key is told to reload keys.
    let stale = alice.encrypt_update(&v, "n1", &n1.set_text("stale").unwrap());
    let resp = alice.push(&v, vec![stale]).await.unwrap();
    assert_eq!(resp.rejected[0].code, REJECT_STALE_KEY);
    assert!(resp.results.is_empty());

    // Bob edits (editor share); Alice reads it with the document key.
    let pb = br
        .local_change(
            "n1",
            br.notes
                .get("n1")
                .unwrap()
                .set_text("trip to the coast in May, with Bob")
                .unwrap(),
        )
        .unwrap();
    bob.push(&v, vec![pb.clone()]).await.unwrap();
    let ch: ChangesResponse = alice.api.get(&format!("/vaults/{v}/changes?since=0")).await;
    let (_, errs) = ar.apply_changes(&ch);
    assert!(
        errs.iter()
            .all(|e| e.to_string().contains("epoch") || e.to_string().contains("decrypt"))
    );
    assert_eq!(ar.note_text("n1"), "trip to the coast in May, with Bob");
    // Bob can't write to other notes (even with a valid-looking payload).
    let other = br.local_change("n1", vec![]).ok();
    let _ = other;
    let forged = PushUpdate {
        object_id: "n2".into(),
        client_update_id: random_id(),
        payload: pb.payload.clone(),
        signature: None,
    };
    assert_eq!(bob.push(&v, vec![forged]).await.unwrap_err().0, 404);

    // Viewer shares are read-only.
    let carol_env = engine::object_share_envelope(&ring, "n1", &carol_pub).unwrap();
    let _: ObjectShareInfo = alice
        .api
        .post(
            &format!("/vaults/{v}/objects/n1/shares"),
            &AddObjectShareRequest {
                user_id: carol.id.clone(),
                role: Role::Viewer,
                envelope: carol_env,
                log_entry: None,
            },
        )
        .await;
    let ckeys: ObjectKeysResponse = carol.api.get(&format!("/vaults/{v}/objects/n1/keys")).await;
    let cring =
        engine::object_keyring_for_recipient(&carol.account, &carol.id, &v, &ckeys, None).unwrap();
    let mut cr = engine::VaultReplica::for_shared_notes(&v);
    cr.doc_keys.insert("n1".into(), cring);
    let ch: ChangesResponse = carol
        .api
        .get(&format!("/vaults/{v}/objects/n1/changes?since=0"))
        .await;
    cr.apply_changes(&ch);
    assert_eq!(cr.note_text("n1"), "trip to the coast in May, with Bob");
    let pc = cr
        .local_change(
            "n1",
            cr.notes
                .get("n1")
                .unwrap()
                .set_text("carol was here")
                .unwrap(),
        )
        .unwrap();
    assert_eq!(carol.push(&v, vec![pc]).await.unwrap_err().0, 403);

    // Revoking Bob: his access ends immediately and the key rotates.
    assert_eq!(
        alice
            .api
            .status::<()>(
                Method::DELETE,
                &format!("/vaults/{v}/objects/n1/shares/{}", bob.id),
                None
            )
            .await,
        204
    );
    wb.until(|m| {
        matches!(
            m,
            ServerMessage::ObjectEvent {
                event: ObjectEventKind::AccessRevoked,
                ..
            }
        )
        .then_some(())
    })
    .await;
    assert_eq!(
        bob.api
            .status::<()>(Method::GET, &format!("/vaults/{v}/objects/n1"), None)
            .await,
        404
    );
    let remaining: Vec<ObjectShareInfo> = alice
        .api
        .get(&format!("/vaults/{v}/objects/n1/shares"))
        .await;
    let users: Vec<PublicUser> = remaining.into_iter().map(|s| s.user).collect();
    let (next, rot) = engine::rotate_object_key(
        alice.ring(&v),
        &ring,
        "n1",
        &engine::ObjectMeta::titled("Trip"),
        &users,
    )
    .unwrap();
    // Rotation must cover exactly the remaining recipients.
    let mut wrong = rot.clone();
    wrong.envelopes.clear();
    assert_eq!(
        alice
            .api
            .status(
                Method::PUT,
                &format!("/vaults/{v}/objects/n1/key"),
                Some(&wrong)
            )
            .await,
        409
    );
    let _: () = alice
        .api
        .put(&format!("/vaults/{v}/objects/n1/key"), &rot)
        .await;
    ar.doc_keys.insert("n1".into(), next);
    let pn = ar
        .local_change(
            "n1",
            ar.notes
                .get("n1")
                .unwrap()
                .set_text("new plan, bob not invited")
                .unwrap(),
        )
        .unwrap();
    alice.push(&v, vec![pn.clone()]).await.unwrap();
    assert!(bring.decrypt(Purpose::Update, "n1", &pn.payload).is_err());
    // Content under the old document key is rejected now.
    let old = br
        .local_change("n1", br.notes.get("n1").unwrap().set_text("x").unwrap())
        .unwrap();
    let resp = alice
        .push(
            &v,
            vec![PushUpdate {
                object_id: "n1".into(),
                client_update_id: random_id(),
                payload: old.payload,
                signature: None,
            }],
        )
        .await
        .unwrap();
    assert_eq!(resp.rejected[0].code, REJECT_STALE_KEY);
    // Carol still reads everything through the key chain.
    let ckeys: ObjectKeysResponse = carol.api.get(&format!("/vaults/{v}/objects/n1/keys")).await;
    let cring =
        engine::object_keyring_for_recipient(&carol.account, &carol.id, &v, &ckeys, None).unwrap();
    assert!(
        !cring
            .decrypt(Purpose::Update, "n1", &pn.payload)
            .unwrap()
            .is_empty()
    );
    let _ = &mut bob;

    // Renaming updates the title recipients see.
    let (_, cur) = {
        let r = &ar.doc_keys["n1"];
        (0, r.clone())
    };
    let meta = engine::ObjectMeta::titled("Coast trip")
        .encrypt(&cur, "n1")
        .unwrap();
    let _: () = alice
        .api
        .put(
            &format!("/vaults/{v}/objects/n1/meta"),
            &ObjectMetaUpdate {
                encrypted_meta: meta,
            },
        )
        .await;
    let mine: Vec<SharedNote> = carol.api.get("/shares").await;
    assert_eq!(
        engine::ObjectMeta::decrypt(&cring, "n1", mine[0].encrypted_meta.as_ref().unwrap())
            .unwrap()
            .title,
        "Coast trip"
    );

    // Members get the note keys through the vault keys response (also after rotation).
    let vkeys: VaultKeysResponse = alice.api.get(&format!("/vaults/{v}/keys")).await;
    assert_eq!(vkeys.objects.len(), 2);
    let rings = engine::object_keyrings(alice.ring(&v), &vkeys);
    assert!(
        rings["n1"]
            .decrypt(Purpose::Update, "n1", &pn.payload)
            .is_ok()
    );

    // Purging the note removes every share.
    assert_eq!(
        alice
            .api
            .status::<()>(Method::DELETE, &format!("/vaults/{v}/objects/n1"), None)
            .await,
        204
    );
    let mine: Vec<SharedNote> = carol.api.get("/shares").await;
    assert!(mine.is_empty());
}

#[tokio::test]
async fn removed_members_lose_per_note_subscriptions() {
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let mut bob = new_user(&srv.base, "bob").await;
    let v = alice.create_vault("Family").await;
    alice.share(&v, &bob.id, Role::Editor).await.unwrap();
    bob.load_keys(&v).await.unwrap();
    let note = NoteDoc::new();
    let u = note.set_text("hello").unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n1", &u)])
        .await
        .unwrap();

    // As a member, Bob subscribes to a single note of the vault.
    let mut wb = Ws::connect(&srv.ws, bob.api.token.as_ref().unwrap()).await;
    wb.send(&ClientMessage::Subscribe {
        vault_id: v.clone(),
        since: None,
        object_id: Some("n1".into()),
    })
    .await;
    wb.until(|m| matches!(m, ServerMessage::Synced { .. }).then_some(()))
        .await;

    // Removing him from the vault ends that subscription as well.
    assert_eq!(
        alice
            .api
            .status::<()>(
                Method::DELETE,
                &format!("/vaults/{v}/members/{}", bob.id),
                None
            )
            .await,
        204
    );
    wb.until(|m| {
        matches!(
            m,
            ServerMessage::ObjectEvent {
                event: ObjectEventKind::AccessRevoked,
                ..
            }
        )
        .then_some(())
    })
    .await;
    let later = note.set_text("hello, written after Bob left").unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n1", &later)])
        .await
        .unwrap();
    let update = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        wb.until(|m| matches!(m, ServerMessage::Update { .. }).then_some(())),
    )
    .await;
    assert!(update.is_err(), "a removed member still received an update");
}

#[tokio::test]
async fn writes_with_a_replaced_vault_key_are_refused() {
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let mut bob = new_user(&srv.base, "bob").await;
    let v = alice.create_vault("Family").await;
    alice.share(&v, &bob.id, Role::Editor).await.unwrap();
    bob.load_keys(&v).await.unwrap();
    let note = NoteDoc::new();
    let u = note.set_text("before").unwrap();
    bob.push(&v, vec![bob.encrypt_update(&v, "n1", &u)])
        .await
        .unwrap();

    // The owner rotates the key (e.g. after removing someone); Bob's client
    // hasn't noticed yet and still encrypts with the old key.
    alice.rotate(&v).await;
    let u = note.set_text("after").unwrap();
    let stale = bob.encrypt_update(&v, "n1", &u);
    let resp = bob.push(&v, vec![stale]).await.unwrap();
    assert_eq!(resp.rejected[0].code, REJECT_STALE_KEY);
    // Snapshots too.
    let snap = CreateSnapshotRequest {
        snapshot_id: random_id(),
        covers_seq: 1,
        payload: bob
            .ring(&v)
            .encrypt(Purpose::Snapshot, "n1", &note.encode_state())
            .unwrap(),
        signature: None,
        carries: false,
    };
    assert_eq!(
        bob.api
            .status(
                Method::POST,
                &format!("/vaults/{v}/objects/n1/snapshots"),
                Some(&snap)
            )
            .await,
        412
    );
    // After reloading keys the same change goes through.
    bob.load_keys(&v).await.unwrap();
    bob.push(&v, vec![bob.encrypt_update(&v, "n1", &u)])
        .await
        .unwrap();
}

#[tokio::test]
async fn agents_stop_when_their_owner_is_disabled() {
    let srv = start().await;
    let admin = new_user(&srv.base, "admin").await; // first user: administrator
    let bob = new_user(&srv.base, "bob").await;
    let created: CreateAgentResponse = bob
        .api
        .post(
            "/agents",
            &CreateAgentRequest {
                name: "Helper".into(),
            },
        )
        .await;
    let tok: TokenResponse = reqwest::Client::new()
        .post(format!("{}/oauth/token", srv.base))
        .basic_auth(
            &created.credential.client_id,
            Some(&created.credential.client_secret),
        )
        .form(&[("grant_type", "client_credentials")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut agent_api = Api::new(&srv.base);
    agent_api.token = Some(tok.access_token);
    assert_eq!(
        agent_api.status::<()>(Method::GET, "/vaults", None).await,
        200
    );

    let st = admin
        .api
        .status(
            Method::PATCH,
            &format!("/admin/users/{}", bob.id),
            Some(&AdminUpdateUser {
                disabled: Some(true),
                is_admin: None,
            }),
        )
        .await;
    assert_eq!(st, 200);
    assert_eq!(
        agent_api.status::<()>(Method::GET, "/vaults", None).await,
        401
    );
}

#[tokio::test]
async fn large_catch_ups_are_split_into_pages() {
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let v = alice.create_vault("Big").await;
    // Nine updates of ~1.9 MB: more than one 16 MiB page.
    for i in 0..9 {
        let payload = vec![b'x'; 1_900_000];
        let p = alice.encrypt_update(&v, &format!("n{i}"), &payload);
        alice.push(&v, vec![p]).await.unwrap();
    }
    let mut since = 0;
    let mut pages = 0;
    let mut seen = 0;
    loop {
        let ch: ChangesResponse = alice
            .api
            .get(&format!(
                "/vaults/{v}/changes?since={since}&snapshots={}",
                pages == 0
            ))
            .await;
        let bytes: usize = ch.updates.iter().map(|u| u.payload.len()).sum();
        assert!(bytes <= 16 << 20 || ch.updates.len() == 1);
        seen += ch.updates.len();
        pages += 1;
        since = ch.next_since;
        if !ch.has_more {
            break;
        }
    }
    assert_eq!(seen, 9);
    assert!(pages >= 2);
}

#[tokio::test]
async fn snapshots_of_one_note_by_one_person_have_a_cooldown() {
    let srv = start_with(|c| c.snapshot_cooldown_secs = 30).await;
    let mut alice = new_user(&srv.base, "alice").await;
    let v = alice.create_vault("V").await;
    let note = NoteDoc::new();
    let u = note.set_text("hello").unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n1", &u)])
        .await
        .unwrap();
    let snap = || CreateSnapshotRequest {
        snapshot_id: random_id(),
        covers_seq: 1,
        payload: alice
            .ring(&v)
            .encrypt(Purpose::Snapshot, "n1", &note.encode_state())
            .unwrap(),
        signature: None,
        carries: false,
    };
    let path = format!("/vaults/{v}/objects/n1/snapshots");
    assert_eq!(
        alice.api.status(Method::POST, &path, Some(&snap())).await,
        201
    );
    assert_eq!(
        alice.api.status(Method::POST, &path, Some(&snap())).await,
        429
    );
}

#[tokio::test]
async fn one_bad_update_does_not_block_its_batch() {
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let v = alice.create_vault("Home").await;
    let note = NoteDoc::new();
    let good1 = alice.encrypt_update(&v, "n", &note.set_text("one").unwrap());
    let good2 = alice.encrypt_update(&v, "n", &note.set_text("one two").unwrap());
    let big = PushUpdate {
        object_id: "m".into(),
        client_update_id: random_id(),
        payload: vec![1; 3 * 1024 * 1024],
        signature: None,
    };
    let big_id = big.client_update_id.clone();
    let resp = alice
        .push(&v, vec![good1.clone(), big, good2.clone()])
        .await
        .unwrap();
    let stored: Vec<&str> = resp
        .results
        .iter()
        .map(|r| r.client_update_id.as_str())
        .collect();
    assert_eq!(
        stored,
        vec![
            good1.client_update_id.as_str(),
            good2.client_update_id.as_str()
        ]
    );
    assert_eq!(resp.rejected.len(), 1);
    assert_eq!(resp.rejected[0].client_update_id, big_id);
    assert_eq!(resp.rejected[0].object_id, "m");
    assert_eq!(resp.rejected[0].code, REJECT_TOO_LARGE);

    // Over the WebSocket the refusal comes in an acknowledgement.
    let mut ws = Ws::connect(&srv.ws, alice.api.token.as_ref().unwrap()).await;
    ws.send(&ClientMessage::Subscribe {
        vault_id: v.clone(),
        since: Some(resp.head_seq),
        object_id: None,
    })
    .await;
    ws.until(|m| matches!(m, ServerMessage::Synced { .. }).then_some(()))
        .await;
    let good3 = alice.encrypt_update(&v, "n", &note.set_text("one two three").unwrap());
    let big = PushUpdate {
        object_id: "m".into(),
        client_update_id: random_id(),
        payload: vec![1; 3 * 1024 * 1024],
        signature: None,
    };
    ws.send(&ClientMessage::Push {
        vault_id: v.clone(),
        updates: vec![big, good3.clone()],
    })
    .await;
    let mut acked = Vec::new();
    let mut refused = Vec::new();
    while acked.is_empty() || refused.is_empty() {
        let (a, r) = ws
            .until(|m| match m {
                ServerMessage::Ack {
                    results, rejected, ..
                } => Some((results.clone(), rejected.clone())),
                _ => None,
            })
            .await;
        acked.extend(a);
        refused.extend(r);
    }
    assert_eq!(acked[0].client_update_id, good3.client_update_id);
    assert_eq!(refused[0].code, REJECT_TOO_LARGE);
}

async fn put_attachment(u: &User, v: &str, id: &str) -> u16 {
    let blob = u
        .ring(v)
        .encrypt(Purpose::Attachment, id, b"bytes")
        .unwrap();
    u.api
        .raw()
        .put(format!("{}/vaults/{v}/attachments/{id}", u.api.base))
        .bearer_auth(u.api.token.clone().unwrap())
        .body(blob)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// Every write that depends on a role follows role changes and removals.
/// These handlers repeat their checks inside the write transaction (S-L1);
/// the race that closes can't be provoked from here, but this covers the
/// handlers themselves.
#[tokio::test]
async fn role_dependent_writes_follow_role_changes() {
    use coolpsonotes_core::engine;
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let mut bob = new_user(&srv.base, "bob").await;
    let carol = new_user(&srv.base, "carol").await;
    let v = alice.create_vault("Team").await;
    alice.share(&v, &bob.id, Role::Editor).await.unwrap();
    bob.load_keys(&v).await.unwrap();

    // A note shared with carol, so bob can retitle it as an editor.
    let n1 = NoteDoc::new();
    let u1 = n1.set_text("plans").unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n1", &u1)])
        .await
        .unwrap();
    let (doc_ring, req) = engine::start_sharing(
        alice.ring(&v),
        "n1",
        &engine::ObjectMeta::titled("Plans"),
        &n1.encode_state(),
        1,
    )
    .unwrap();
    let _: () = alice
        .api
        .put(&format!("/vaults/{v}/objects/n1/key"), &req)
        .await;
    let carol_pub: PublicUser = alice.api.get(&format!("/users/{}", carol.id)).await;
    let _: ObjectShareInfo = alice
        .api
        .post(
            &format!("/vaults/{v}/objects/n1/shares"),
            &AddObjectShareRequest {
                user_id: carol.id.clone(),
                role: Role::Viewer,
                envelope: engine::object_share_envelope(&doc_ring, "n1", &carol_pub).unwrap(),
                log_entry: None,
            },
        )
        .await;
    let title = |t: &str| ObjectMetaUpdate {
        encrypted_meta: engine::ObjectMeta::titled(t)
            .encrypt(&doc_ring, "n1")
            .unwrap(),
    };
    let meta_path = format!("/vaults/{v}/objects/n1/meta");
    let share_path = format!("/vaults/{v}/objects/n1/shares/{}", carol.id);
    let to_editor = UpdateMemberRequest {
        role: Role::Editor,
        log_entry: None,
    };

    // As an editor, bob writes; only the owner manages shares and the vault.
    assert_eq!(put_attachment(&bob, &v, "a1").await, 201);
    assert_eq!(put_attachment(&bob, &v, "a2").await, 201);
    let a1 = format!("/vaults/{v}/attachments/a1");
    let a2 = format!("/vaults/{v}/attachments/a2");
    assert_eq!(bob.api.status::<()>(Method::DELETE, &a1, None).await, 204);
    assert_eq!(
        bob.api
            .status(Method::PUT, &meta_path, Some(&title("Plan B")))
            .await,
        204
    );
    assert_eq!(
        bob.api
            .status(Method::PATCH, &share_path, Some(&to_editor))
            .await,
        403
    );
    assert_eq!(
        carol
            .api
            .status(Method::PATCH, &share_path, Some(&to_editor))
            .await,
        404
    );
    assert_eq!(
        alice
            .api
            .status(Method::PATCH, &share_path, Some(&to_editor))
            .await,
        204
    );
    let archive = UpdateVaultRequest {
        encrypted_meta: None,
        archived: Some(true),
    };
    assert_eq!(
        bob.api
            .status(Method::PATCH, &format!("/vaults/{v}"), Some(&archive))
            .await,
        403
    );
    let info: VaultInfo = alice
        .api
        .call(Method::PATCH, &format!("/vaults/{v}"), Some(&archive))
        .await
        .unwrap();
    assert!(info.archived);

    // Demoted to viewer, bob can't change anything any more.
    let to_viewer = UpdateMemberRequest {
        role: Role::Viewer,
        log_entry: None,
    };
    assert_eq!(
        alice
            .api
            .status(
                Method::PATCH,
                &format!("/vaults/{v}/members/{}", bob.id),
                Some(&to_viewer)
            )
            .await,
        204
    );
    assert_eq!(put_attachment(&bob, &v, "a3").await, 403);
    assert_eq!(bob.api.status::<()>(Method::DELETE, &a2, None).await, 403);
    assert_eq!(
        bob.api
            .status(Method::PUT, &meta_path, Some(&title("Plan C")))
            .await,
        403
    );
    assert_eq!(
        bob.api
            .status::<()>(Method::DELETE, &format!("/vaults/{v}/objects/n1"), None)
            .await,
        403
    );
    let w = bob.encrypt_update(&v, "n2", &NoteDoc::new().set_text("x").unwrap());
    assert_eq!(bob.push(&v, vec![w]).await.unwrap_err().0, 403);

    // Once removed, bob gets no envelope back, and everything is 404 to him.
    assert_eq!(
        alice
            .api
            .status::<()>(
                Method::DELETE,
                &format!("/vaults/{v}/members/{}", bob.id),
                None
            )
            .await,
        204
    );
    let bob_pub: PublicUser = alice.api.get(&format!("/users/{}", bob.id)).await;
    let (epoch, key) = alice.ring(&v).current().unwrap();
    let grant = GrantKeysRequest {
        envelopes: vec![KeyEnvelope {
            user_id: bob.id.clone(),
            epoch,
            envelope: keys::wrap_vault_key(
                key,
                &v,
                epoch,
                &bob.id,
                &bob_pub.account_key.unwrap().verify().unwrap().enc_public,
            )
            .unwrap(),
            signature: None,
        }],
    };
    assert_eq!(
        alice
            .api
            .status(
                Method::POST,
                &format!("/vaults/{v}/keys/grant"),
                Some(&grant)
            )
            .await,
        400
    );
    assert_eq!(bob.load_keys(&v).await, Err(404));
    assert_eq!(bob.api.status::<()>(Method::DELETE, &a2, None).await, 404);
    assert_eq!(
        bob.api
            .status(Method::PUT, &meta_path, Some(&title("Plan D")))
            .await,
        404
    );
    // The owner still can.
    assert_eq!(alice.api.status::<()>(Method::DELETE, &a2, None).await, 204);
}

async fn put_key(u: &User, v: &str, o: &str, req: &ObjectKeyUpdate) -> Result<(), u16> {
    u.api
        .call::<_, ()>(
            Method::PUT,
            &format!("/vaults/{v}/objects/{o}/key"),
            Some(req),
        )
        .await
        .map_err(|e| e.0)
}

/// R2: every note can have its own key from the start; sharing it later
/// starts with a fresh key, so recipients never get what was written before.
#[tokio::test]
async fn notes_have_own_keys_and_recipients_never_get_the_history_before_sharing() {
    use coolpsonotes_core::engine;
    use coolpsonotes_core::keys::FIRST_DOC_EPOCH;
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let mut bob = new_user(&srv.base, "bob").await;
    let carol = new_user(&srv.base, "carol").await;
    let v = alice.create_vault("Home").await;
    alice.share(&v, &bob.id, Role::Editor).await.unwrap();
    bob.load_keys(&v).await.unwrap();

    // An editor gives a new note its own (private) key: no snapshot needed.
    let (doc, req) = engine::new_document_key(bob.ring(&v), "n", "Plans").unwrap();
    assert!(req.private);
    put_key(&bob, &v, "n", &req).await.unwrap();
    let note = NoteDoc::new();
    let first = note.set_text("private plans").unwrap();
    let under_doc = PushUpdate {
        object_id: "n".into(),
        client_update_id: random_id(),
        payload: doc.encrypt(Purpose::Update, "n", &first).unwrap(),
        signature: None,
    };
    let resp = bob.push(&v, vec![under_doc]).await.unwrap();
    assert_eq!(resp.results.len(), 1);
    // Content under the vault key is refused now.
    let stale = bob.encrypt_update(&v, "n", &NoteDoc::new().set_text("x").unwrap());
    assert_eq!(
        bob.push(&v, vec![stale]).await.unwrap().rejected[0].code,
        REJECT_STALE_KEY
    );
    let vk: VaultKeysResponse = alice.api.get(&format!("/vaults/{v}/keys")).await;
    assert!(vk.objects.iter().any(|e| e.object_id == "n"));
    assert_eq!(vk.shared, Some(vec![]), "a private key is not a share");

    // Nobody can be added before sharing starts with a fresh key.
    let carol_pub: PublicUser = alice.api.get(&format!("/users/{}", carol.id)).await;
    let early = engine::object_share_envelope(&doc, "n", &carol_pub).unwrap();
    let add = |env: KeyEnvelope| AddObjectShareRequest {
        user_id: carol.id.clone(),
        role: Role::Viewer,
        envelope: env,
        log_entry: None,
    };
    let st = alice
        .api
        .status(
            Method::POST,
            &format!("/vaults/{v}/objects/n/shares"),
            Some(&add(early)),
        )
        .await;
    assert_eq!(st, 409);

    // Only the owner starts sharing: a fresh epoch, no chain link, a snapshot.
    let ch: ChangesResponse = alice.api.get(&format!("/vaults/{v}/changes?since=0")).await;
    let covers = ch.head_seq;
    let (shared, start) = engine::start_sharing_keyed(
        alice.ring(&v),
        &doc,
        "n",
        &engine::ObjectMeta::titled("Plans"),
        &note.encode_state(),
        covers,
    )
    .unwrap();
    assert_eq!(start.epoch, FIRST_DOC_EPOCH + 1);
    assert!(start.chain_link.is_none());
    assert_eq!(put_key(&bob, &v, "n", &start).await.unwrap_err(), 403);
    put_key(&alice, &v, "n", &start).await.unwrap();
    let env = engine::object_share_envelope(&shared, "n", &carol_pub).unwrap();
    let _: ObjectShareInfo = alice
        .api
        .post(&format!("/vaults/{v}/objects/n/shares"), &add(env))
        .await;
    let vk: VaultKeysResponse = alice.api.get(&format!("/vaults/{v}/keys")).await;
    assert_eq!(vk.shared, Some(vec!["n".to_string()]));

    // Carol gets the key and content from the share on — never the earlier
    // key, nor anything encrypted with it.
    let keys: ObjectKeysResponse = carol.api.get(&format!("/vaults/{v}/objects/n/keys")).await;
    assert!(
        keys.envelopes
            .iter()
            .all(|e| e.epoch == FIRST_DOC_EPOCH + 1)
    );
    assert!(keys.chain.iter().all(|c| c.epoch > FIRST_DOC_EPOCH + 1));
    let ring =
        engine::object_keyring_for_recipient(&carol.account, &carol.id, &v, &keys, None).unwrap();
    assert!(ring.get(FIRST_DOC_EPOCH).is_none());
    let ch: ChangesResponse = carol
        .api
        .get(&format!("/vaults/{v}/objects/n/changes?since=0"))
        .await;
    let epochs: Vec<u32> = ch
        .updates
        .iter()
        .map(|u| coolpsonotes_core::crypto::sym_epoch(&u.payload).unwrap())
        .chain(
            ch.snapshots
                .iter()
                .map(|s| coolpsonotes_core::crypto::sym_epoch(&s.payload).unwrap()),
        )
        .collect();
    assert!(!epochs.is_empty());
    assert!(epochs.iter().all(|&e| e > FIRST_DOC_EPOCH), "{epochs:x?}");
    let n = NoteDoc::new();
    for s in &ch.snapshots {
        n.apply_update(&ring.decrypt(Purpose::Snapshot, "n", &s.payload).unwrap())
            .unwrap();
    }
    assert_eq!(n.text(), "private plans");

    // An older note under the vault key gets a private key with a snapshot.
    let old = NoteDoc::new();
    let u = old.set_text("old note").unwrap();
    bob.push(&v, vec![bob.encrypt_update(&v, "m", &u)])
        .await
        .unwrap();
    let (_, no_snapshot) = engine::new_document_key(bob.ring(&v), "m", "Old").unwrap();
    assert_eq!(put_key(&bob, &v, "m", &no_snapshot).await.unwrap_err(), 400);
    let ch: ChangesResponse = bob.api.get(&format!("/vaults/{v}/changes?since=0")).await;
    let (_, adopt) =
        engine::adopt_document_key(bob.ring(&v), "m", "Old", &old.encode_state(), ch.head_seq)
            .unwrap();
    put_key(&bob, &v, "m", &adopt).await.unwrap();

    // Rotating the vault key must rotate every note's key with it.
    let st = alice
        .api
        .status(
            Method::POST,
            &format!("/vaults/{v}/keys/rotate"),
            Some(
                &engine::rotate(
                    &mut alice.ring(&v).clone(),
                    &alice
                        .api
                        .get::<Vec<MemberInfo>>(&format!("/vaults/{v}/members"))
                        .await,
                    &VaultMeta::named("Home"),
                )
                .unwrap(),
            ),
        )
        .await;
    assert_eq!(st, 409);
    alice.rotate(&v).await;
    let vk: VaultKeysResponse = alice.api.get(&format!("/vaults/{v}/keys")).await;
    let n_epochs: Vec<u32> = vk
        .objects
        .iter()
        .filter(|e| e.object_id == "n")
        .map(|e| e.epoch)
        .collect();
    assert!(n_epochs.contains(&(FIRST_DOC_EPOCH + 2)));
}

#[tokio::test]
async fn membership_changes_carry_owner_signed_log_entries() {
    use coolpsonotes_core::memberlog::{LogBody, LogEntry, LogMember, LogState, key_commitment};
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let bob = new_user(&srv.base, "bob").await;
    let carol = new_user(&srv.base, "carol").await;
    let v = alice.create_signed_vault("Signed").await;
    let log = alice.member_log(&v).await;
    assert_eq!(log.len(), 1);
    let s = LogState::verify(&v, &alice.account.public(), &log).unwrap();
    assert_eq!(s.check_key(1, alice.ring(&v).get(1).unwrap()), Ok(true));
    // Members only.
    let st = bob
        .api
        .status::<()>(Method::GET, &format!("/vaults/{v}/log"), None)
        .await;
    assert_eq!(st, 404);

    let add = |entry: Option<LogEntry>, role: Role| AddMemberRequest {
        user_id: bob.id.clone(),
        role,
        key_envelope: None,
        log_entry: entry,
    };
    let members = format!("/vaults/{v}/members");
    let bob_member = LogMember::new(&bob.id, Role::Editor, &bob.account.public());
    // Unsigned, mismatching, someone else's or forged entries are refused.
    assert_eq!(
        alice
            .api
            .status(Method::POST, &members, Some(&add(None, Role::Editor)))
            .await,
        409
    );
    let e = alice
        .sign_next(
            &v,
            LogBody::Add {
                member: bob_member.clone(),
            },
        )
        .await;
    assert_eq!(
        alice
            .api
            .status(
                Method::POST,
                &members,
                Some(&add(Some(e.clone()), Role::Viewer))
            )
            .await,
        400,
        "the entry says editor"
    );
    let by_carol = LogEntry::sign(
        &carol.account,
        &alice.id,
        &v,
        log.last(),
        LogBody::Add {
            member: bob_member.clone(),
        },
        1,
    );
    assert_eq!(
        alice
            .api
            .status(
                Method::POST,
                &members,
                Some(&add(Some(by_carol), Role::Editor))
            )
            .await,
        400,
        "not signed by the owner's key"
    );
    let swapped = LogMember::new(&bob.id, Role::Editor, &carol.account.public());
    let e2 = alice.sign_next(&v, LogBody::Add { member: swapped }).await;
    assert_eq!(
        alice
            .api
            .status(Method::POST, &members, Some(&add(Some(e2), Role::Editor)))
            .await,
        400,
        "not bob's key"
    );
    assert_eq!(
        alice
            .api
            .status(
                Method::POST,
                &members,
                Some(&add(Some(e.clone()), Role::Editor))
            )
            .await,
        201
    );
    // An entry that doesn't continue the log (signed against an old head).
    let stale = LogEntry::sign(
        &alice.account,
        &alice.id,
        &v,
        log.last(),
        LogBody::Role {
            user_id: bob.id.clone(),
            role: Role::Viewer,
        },
        1,
    );
    let role_path = format!("/vaults/{v}/members/{}", bob.id);
    assert_eq!(
        alice
            .api
            .status(
                Method::PATCH,
                &role_path,
                Some(&UpdateMemberRequest {
                    role: Role::Viewer,
                    log_entry: Some(stale)
                }),
            )
            .await,
        409
    );
    let e = alice
        .sign_next(
            &v,
            LogBody::Role {
                user_id: bob.id.clone(),
                role: Role::Viewer,
            },
        )
        .await;
    assert_eq!(
        alice
            .api
            .status(
                Method::PATCH,
                &role_path,
                Some(&UpdateMemberRequest {
                    role: Role::Viewer,
                    log_entry: Some(e)
                }),
            )
            .await,
        204
    );

    // Rotation needs its entry with the new key's commitment.
    let mut ring = alice.ring(&v).clone();
    let ms: Vec<MemberInfo> = alice.api.get(&members).await;
    let mut req =
        coolpsonotes_core::engine::rotate(&mut ring, &ms, &VaultMeta::named("S")).unwrap();
    let st = alice
        .api
        .status(
            Method::POST,
            &format!("/vaults/{v}/keys/rotate"),
            Some(&req),
        )
        .await;
    assert_eq!(st, 409, "unsigned rotation");
    req.log_entry = Some(
        alice
            .sign_next(
                &v,
                LogBody::Rotate {
                    epoch: 2,
                    key_commit: key_commitment(&v, 2, ring.get(2).unwrap()),
                },
            )
            .await,
    );
    let st = alice
        .api
        .status(
            Method::POST,
            &format!("/vaults/{v}/keys/rotate"),
            Some(&req),
        )
        .await;
    assert_eq!(st, 204);

    // Bob may leave with or without an entry; the owner removes only with one.
    let leave = bob
        .sign_next(
            &v,
            LogBody::Leave {
                user_id: bob.id.clone(),
            },
        )
        .await;
    let st = bob
        .api
        .status(
            Method::DELETE,
            &role_path,
            Some(&RemoveMemberRequest {
                log_entry: Some(leave),
            }),
        )
        .await;
    assert_eq!(st, 204);
    let log = alice.member_log(&v).await;
    let s = LogState::verify(&v, &alice.account.public(), &log).unwrap();
    assert!(s.member(&bob.id).is_none());
    assert_eq!(s.epoch, 2);
    assert_eq!(s.check_key(2, ring.get(2).unwrap()), Ok(true));

    // Someone added before they have keys is pinned later (`Rekey`).
    let (dana_api, dana) = dev_login(&srv.base, "dana").await;
    let e = alice
        .sign_next(
            &v,
            LogBody::Add {
                member: LogMember::pending(&dana.user.id, Role::Editor),
            },
        )
        .await;
    let req = AddMemberRequest {
        user_id: dana.user.id.clone(),
        role: Role::Editor,
        key_envelope: None,
        log_entry: Some(e),
    };
    assert_eq!(
        alice.api.status(Method::POST, &members, Some(&req)).await,
        201
    );
    drop(dana_api);
}

#[tokio::test]
async fn an_existing_vault_starts_its_log_with_a_checkpoint() {
    use coolpsonotes_core::memberlog::{LogBody, LogEntry, LogMember, LogState, key_commitment};
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let bob = new_user(&srv.base, "bob").await;
    let v = alice.create_vault("Old").await;
    alice.share(&v, &bob.id, Role::Viewer).await.unwrap();
    let log_path = format!("/vaults/{v}/log");
    let checkpoint = |members: Vec<LogMember>| {
        LogEntry::sign(
            &alice.account,
            &alice.id,
            &v,
            None,
            LogBody::Checkpoint {
                members,
                epoch: 1,
                key_commit: key_commitment(&v, 1, alice.ring(&v).get(1).unwrap()),
            },
            1,
        )
    };
    let me = LogMember::new(&alice.id, Role::Owner, &alice.account.public());
    let bob_m = LogMember::new(&bob.id, Role::Viewer, &bob.account.public());
    // It must list exactly the members, with their roles and keys.
    for wrong in [
        vec![me.clone()],
        vec![
            me.clone(),
            LogMember::new(&bob.id, Role::Editor, &bob.account.public()),
        ],
    ] {
        let st = alice
            .api
            .status(
                Method::POST,
                &log_path,
                Some(&AppendLogRequest {
                    entry: checkpoint(wrong),
                }),
            )
            .await;
        assert_eq!(st, 400);
    }
    let entry = checkpoint(vec![me.clone(), bob_m.clone()]);
    let st = bob
        .api
        .status(
            Method::POST,
            &log_path,
            Some(&AppendLogRequest {
                entry: entry.clone(),
            }),
        )
        .await;
    assert_eq!(st, 403, "owner only");
    let st = alice
        .api
        .status(
            Method::POST,
            &log_path,
            Some(&AppendLogRequest {
                entry: entry.clone(),
            }),
        )
        .await;
    assert_eq!(st, 204);
    let st = alice
        .api
        .status(Method::POST, &log_path, Some(&AppendLogRequest { entry }))
        .await;
    assert_eq!(st, 409, "only once");
    let s = LogState::verify(&v, &alice.account.public(), &alice.member_log(&v).await).unwrap();
    assert!(s.vouches_for(&bob.id, Role::Viewer, &bob.account.public()));
    // From now on unsigned changes are refused.
    let st = alice
        .api
        .status::<()>(
            Method::DELETE,
            &format!("/vaults/{v}/members/{}", bob.id),
            None,
        )
        .await;
    assert_eq!(st, 409);
}

#[tokio::test]
async fn signed_content_is_required_once_the_log_says_so() {
    use coolpsonotes_core::memberlog::{self, LogBody};
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let bob = new_user(&srv.base, "bob").await;
    let v = alice.create_signed_vault("Signed").await;
    let note = NoteDoc::new();
    let mut u = alice.encrypt_update(&v, "n1", &note.set_text("before").unwrap());
    // Before the requirement unsigned content is stored.
    assert!(
        alice
            .push(&v, vec![u.clone()])
            .await
            .unwrap()
            .rejected
            .is_empty()
    );

    let log_path = format!("/vaults/{v}/log");
    let too_far = alice
        .sign_next(&v, LogBody::RequireSigned { from_seq: 99 })
        .await;
    let st = alice
        .api
        .status(
            Method::POST,
            &log_path,
            Some(&AppendLogRequest { entry: too_far }),
        )
        .await;
    assert_eq!(st, 400, "from_seq can't be in the future");
    let entry = alice
        .sign_next(&v, LogBody::RequireSigned { from_seq: 1 })
        .await;
    let st = alice
        .api
        .status(Method::POST, &log_path, Some(&AppendLogRequest { entry }))
        .await;
    assert_eq!(st, 204);

    // Unsigned, or signed with someone else's key: refused alone.
    u = alice.encrypt_update(&v, "n1", &note.set_text("after").unwrap());
    let mut forged = u.clone();
    forged.client_update_id = random_id();
    forged.signature = Some(memberlog::sign_update(
        &bob.account,
        &v,
        "n1",
        &forged.payload,
    ));
    let mut good = u.clone();
    good.client_update_id = random_id();
    good.signature = Some(memberlog::sign_update(
        &alice.account,
        &v,
        "n1",
        &good.payload,
    ));
    let r = alice.push(&v, vec![u, forged, good.clone()]).await.unwrap();
    let codes: Vec<&str> = r.rejected.iter().map(|x| x.code.as_str()).collect();
    assert_eq!(codes, vec![REJECT_UNSIGNED, REJECT_BAD_SIGNATURE]);
    assert_eq!(r.results.len(), 1);
    // The signature is handed out with the update.
    let ch: ChangesResponse = alice.api.get(&format!("/vaults/{v}/changes?since=0")).await;
    let last = ch.updates.last().unwrap();
    assert_eq!(last.signature.as_deref(), good.signature.as_deref());

    // Snapshots too, over what they cover.
    let ring = alice.ring(&v).clone();
    let payload = ring
        .encrypt(Purpose::Snapshot, "n1", &note.encode_state())
        .unwrap();
    let mut snap = CreateSnapshotRequest {
        snapshot_id: random_id(),
        covers_seq: ch.head_seq,
        payload: payload.clone(),
        signature: None,
        carries: false,
    };
    let path = format!("/vaults/{v}/objects/n1/snapshots");
    assert_eq!(
        alice.api.status(Method::POST, &path, Some(&snap)).await,
        409
    );
    snap.signature = Some(memberlog::sign_snapshot(
        &alice.account,
        &v,
        "n1",
        ch.head_seq - 1,
        &payload,
    ));
    assert_eq!(
        alice.api.status(Method::POST, &path, Some(&snap)).await,
        400,
        "covers something else"
    );
    snap.signature = Some(memberlog::sign_snapshot(
        &alice.account,
        &v,
        "n1",
        ch.head_seq,
        &payload,
    ));
    assert_eq!(
        alice.api.status(Method::POST, &path, Some(&snap)).await,
        201
    );
}

/// R3: the owner signs each note key given to someone a note is shared
/// with; the server keeps the signature with that key and hands it out.
#[tokio::test]
async fn note_key_grants_carry_the_owners_signature() {
    use coolpsonotes_core::{engine, memberlog};
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let carol = new_user(&srv.base, "carol").await;
    let rita = new_user(&srv.base, "rita").await;
    let v = alice.create_vault("Home").await;
    let note = NoteDoc::new();
    let first = note.set_text("plans").unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n", &first)])
        .await
        .unwrap();
    let head: ChangesResponse = alice.api.get(&format!("/vaults/{v}/changes?since=0")).await;
    let (doc, start) = engine::start_sharing(
        alice.ring(&v),
        "n",
        &engine::ObjectMeta::titled("Plans"),
        &note.encode_state(),
        head.head_seq,
    )
    .unwrap();
    put_key(&alice, &v, "n", &start).await.unwrap();
    let carol_pub: PublicUser = alice.api.get(&format!("/users/{}", carol.id)).await;
    let rita_pub: PublicUser = alice.api.get(&format!("/users/{}", rita.id)).await;
    let share = |who: &PublicUser, signed: bool| {
        let mut env = engine::object_share_envelope(&doc, "n", who).unwrap();
        if signed {
            let (epoch, key) = doc.current().unwrap();
            env.signature = Some(memberlog::sign_note_grant(
                &alice.account,
                &v,
                "n",
                &who.id,
                epoch,
                key,
            ));
        }
        AddObjectShareRequest {
            user_id: who.id.clone(),
            role: Role::Editor,
            envelope: env,
            log_entry: None,
        }
    };
    let path = format!("/vaults/{v}/objects/n/shares");
    let _: ObjectShareInfo = alice.api.post(&path, &share(&carol_pub, true)).await;
    let _: ObjectShareInfo = alice.api.post(&path, &share(&rita_pub, false)).await;
    let list: Vec<ObjectShareInfo> = alice.api.get(&path).await;
    let signed = |id: &str| list.iter().find(|s| s.user.id == id).unwrap().key_signed;
    assert!(signed(&carol.id) && !signed(&rita.id));

    // Carol checks her key against the owner's signature.
    let keys: ObjectKeysResponse = carol.api.get(&format!("/vaults/{v}/objects/n/keys")).await;
    let owner = alice.account.public();
    engine::object_keyring_for_recipient(&carol.account, &carol.id, &v, &keys, Some(&owner))
        .unwrap();
    // Rita's has none yet: not used, until the owner signs it.
    let keys: ObjectKeysResponse = rita.api.get(&format!("/vaults/{v}/objects/n/keys")).await;
    assert!(matches!(
        engine::object_keyring_for_recipient(&rita.account, &rita.id, &v, &keys, Some(&owner)),
        Err(engine::EngineError::UnconfirmedKey)
    ));
    let grants = unsigned_grants(&alice, &v, &doc, &list);
    let sig_path = format!("/vaults/{v}/objects/n/keys/signatures");
    let req = SignGrantsRequest { grants };
    assert_eq!(
        rita.api.status(Method::PUT, &sig_path, Some(&req)).await,
        404,
        "owner only"
    );
    assert_eq!(
        alice.api.status(Method::PUT, &sig_path, Some(&req)).await,
        204
    );
    let keys: ObjectKeysResponse = rita.api.get(&format!("/vaults/{v}/objects/n/keys")).await;
    engine::object_keyring_for_recipient(&rita.account, &rita.id, &v, &keys, Some(&owner)).unwrap();
    let list: Vec<ObjectShareInfo> = alice.api.get(&path).await;
    assert!(list.iter().all(|s| s.key_signed));
}

/// The grant signatures an owner adds for keys given before they were signed.
fn unsigned_grants(
    owner: &User,
    v: &str,
    doc: &keys::VaultKeyring,
    shares: &[ObjectShareInfo],
) -> Vec<GrantSignature> {
    let (epoch, key) = doc.current().unwrap();
    shares
        .iter()
        .filter(|s| !s.key_signed)
        .map(|s| GrantSignature {
            user_id: s.user.id.clone(),
            epoch,
            signature: coolpsonotes_core::memberlog::sign_note_grant(
                &owner.account,
                v,
                "n",
                &s.user.id,
                epoch,
                key,
            ),
        })
        .collect()
}

/// R2: a vault member can reload one note's keys alone (after it got or
/// rotated its key) instead of every note's; recipients get only theirs.
#[tokio::test]
async fn members_reload_one_notes_keys() {
    use coolpsonotes_core::engine;
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let mut bob = new_user(&srv.base, "bob").await;
    let v = alice.create_vault("Home").await;
    alice.share(&v, &bob.id, Role::Editor).await.unwrap();
    bob.load_keys(&v).await.unwrap();
    let (doc, req) = engine::new_document_key(bob.ring(&v), "n", "Plans").unwrap();
    put_key(&bob, &v, "n", &req).await.unwrap();

    let r: ObjectKeysResponse = alice.api.get(&format!("/vaults/{v}/objects/n/keys")).await;
    assert_eq!(r.shared, Some(false));
    assert!(
        r.envelopes.is_empty(),
        "members hold no envelope of their own"
    );
    let ring = engine::object_keyring_for_member(alice.ring(&v), &r).unwrap();
    assert_eq!(ring.current().unwrap().1.0, doc.current().unwrap().1.0);

    // Once shared, a recipient gets no member keys.
    let carol = new_user(&srv.base, "carol").await;
    let note = NoteDoc::new();
    let state = note.encode_state();
    let head: ChangesResponse = alice.api.get(&format!("/vaults/{v}/changes?since=0")).await;
    let (doc2, start) = engine::start_sharing_keyed(
        alice.ring(&v),
        &ring,
        "n",
        &engine::ObjectMeta::titled("Plans"),
        &state,
        head.head_seq,
    )
    .unwrap();
    put_key(&alice, &v, "n", &start).await.unwrap();
    let carol_pub: PublicUser = alice.api.get(&format!("/users/{}", carol.id)).await;
    let _: ObjectShareInfo = alice
        .api
        .post(
            &format!("/vaults/{v}/objects/n/shares"),
            &AddObjectShareRequest {
                user_id: carol.id.clone(),
                role: Role::Viewer,
                envelope: engine::object_share_envelope(&doc2, "n", &carol_pub).unwrap(),
                log_entry: None,
            },
        )
        .await;
    let r: ObjectKeysResponse = alice.api.get(&format!("/vaults/{v}/objects/n/keys")).await;
    assert_eq!(r.shared, Some(true));
    assert_eq!(
        engine::object_keyring_for_member(alice.ring(&v), &r)
            .unwrap()
            .current_epoch(),
        doc2.current_epoch()
    );
    let r: ObjectKeysResponse = carol.api.get(&format!("/vaults/{v}/objects/n/keys")).await;
    assert!(r.member_envelopes.is_empty() && r.shared.is_none());
}

#[tokio::test]
async fn a_snapshot_carrying_changes_the_log_lacks_reaches_everyone() {
    let srv = start_with(|c| c.snapshot_retention = 2).await;
    let mut alice = new_user(&srv.base, "alice").await;
    let mut bob = new_user(&srv.base, "bob").await;
    let v = alice.create_vault("Plans").await;
    alice.share(&v, &bob.id, Role::Editor).await.unwrap();
    bob.load_keys(&v).await.unwrap();
    let note = NoteDoc::new();
    let u = note.set_text("short").unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n", &u)])
        .await
        .unwrap();
    let ch: ChangesResponse = bob.api.get(&format!("/vaults/{v}/changes?since=0")).await;
    let head = ch.head_seq;
    let mut wb = Ws::connect(&srv.ws, bob.api.token.as_ref().unwrap()).await;
    wb.send(&ClientMessage::Subscribe {
        vault_id: v.clone(),
        since: Some(head),
        object_id: None,
    })
    .await;
    wb.until(|m| matches!(m, ServerMessage::Synced { .. }).then_some(()))
        .await;

    // Alice's change too large for one update goes out in a snapshot of
    // the whole note, marked as carrying it.
    note.set_text(&"a long line ".repeat(500)).unwrap();
    let snapshot = |carries: bool, covers_seq: i64| CreateSnapshotRequest {
        snapshot_id: random_id(),
        covers_seq,
        payload: alice
            .ring(&v)
            .encrypt(Purpose::Snapshot, "n", &note.encode_state())
            .unwrap(),
        signature: None,
        carries,
    };
    let carrier = snapshot(true, head);
    let _: SnapshotInfo = alice
        .api
        .post(&format!("/vaults/{v}/objects/n/snapshots"), &carrier)
        .await;
    // Live, to everyone subscribed...
    let live = wb
        .until(|m| match m {
            ServerMessage::Snapshot { snapshot, .. } => Some(snapshot.clone()),
            _ => None,
        })
        .await;
    assert_eq!(live.id, carrier.snapshot_id);
    let got = NoteDoc::new();
    let plain = bob
        .ring(&v)
        .decrypt(Purpose::Snapshot, "n", &live.payload)
        .unwrap();
    got.apply_update(&plain).unwrap();
    assert_eq!(got.text(), note.text());
    // ...and with every catch-up from before it, also on later pages (which
    // carry no snapshots otherwise).
    let carried_in =
        |ch: &ChangesResponse| ch.snapshots.iter().any(|s| s.id == carrier.snapshot_id);
    let ch: ChangesResponse = bob
        .api
        .get(&format!("/vaults/{v}/changes?since={head}&snapshots=false"))
        .await;
    assert!(carried_in(&ch));
    // Ordinary snapshots never come with a catch-up; nor does it after it.
    let u2 = note.set_text("short again").unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n", &u2)])
        .await
        .unwrap();
    let ordinary = snapshot(false, head + 1);
    let _: SnapshotInfo = alice
        .api
        .post(&format!("/vaults/{v}/objects/n/snapshots"), &ordinary)
        .await;
    let ch: ChangesResponse = bob
        .api
        .get(&format!("/vaults/{v}/changes?since={head}&snapshots=false"))
        .await;
    assert!(!ch.snapshots.iter().any(|s| s.id == ordinary.snapshot_id));
    let later: ChangesResponse = bob
        .api
        .get(&format!(
            "/vaults/{v}/changes?since={}&snapshots=false",
            head + 1
        ))
        .await;
    assert!(!carried_in(&later));
    // Revision retention keeps the newest one carrying changes.
    for _ in 0..3 {
        let _: SnapshotInfo = alice
            .api
            .post(
                &format!("/vaults/{v}/objects/n/snapshots"),
                &snapshot(false, head + 1),
            )
            .await;
    }
    let ch: ChangesResponse = bob
        .api
        .get(&format!("/vaults/{v}/changes?since={head}&snapshots=false"))
        .await;
    assert!(carried_in(&ch));
}

#[tokio::test]
async fn a_rotation_can_leave_out_people_whose_key_changed_on_purpose() {
    use coolpsonotes_core::engine;
    let srv = start().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let bob = new_user(&srv.base, "bob").await;
    let mut carol = new_user(&srv.base, "carol").await;
    let dave = new_user(&srv.base, "dave").await;
    let v = alice.create_vault("Home").await;
    alice.share(&v, &bob.id, Role::Editor).await.unwrap();
    alice.share(&v, &carol.id, Role::Editor).await.unwrap();
    // Dave isn't a member: one note is shared with him.
    let note = NoteDoc::new();
    let u = note.set_text("plans").unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n1", &u)])
        .await
        .unwrap();
    let (ring, req) = engine::start_sharing(
        alice.ring(&v),
        "n1",
        &engine::ObjectMeta::titled("Plans"),
        &note.encode_state(),
        1,
    )
    .unwrap();
    let _: () = alice
        .api
        .put(&format!("/vaults/{v}/objects/n1/key"), &req)
        .await;
    let dave_pub: PublicUser = alice.api.get(&format!("/users/{}", dave.id)).await;
    let share_dave = |ring: &keys::VaultKeyring| AddObjectShareRequest {
        user_id: dave.id.clone(),
        role: Role::Viewer,
        envelope: engine::object_share_envelope(ring, "n1", &dave_pub).unwrap(),
        log_entry: None,
    };
    let _: ObjectShareInfo = alice
        .api
        .post(
            &format!("/vaults/{v}/objects/n1/shares"),
            &share_dave(&ring),
        )
        .await;

    // Carol's and Dave's keys changed: leaving them out unannounced is refused.
    let out = [carol.id.as_str(), dave.id.as_str()];
    assert_eq!(alice.rotate_leaving_out(&v, &out, false).await, 409);
    // Said on purpose, the key is renewed without them.
    let ok = alice.rotate_leaving_out(&v, &out, true).await;
    assert!((200..300).contains(&ok), "{ok}");
    let members: Vec<MemberInfo> = alice.api.get(&format!("/vaults/{v}/members")).await;
    let has_key = |id: &str| {
        members
            .iter()
            .find(|m| m.user.id == id)
            .unwrap()
            .has_current_key
    };
    assert!(has_key(&alice.id) && has_key(&bob.id) && !has_key(&carol.id));
    let recipients = |shares: &[ObjectShareInfo]| {
        shares
            .iter()
            .find(|s| s.user.id == dave.id)
            .unwrap()
            .has_current_key
    };
    let shares: Vec<ObjectShareInfo> = alice
        .api
        .get(&format!("/vaults/{v}/objects/n1/shares"))
        .await;
    assert!(!recipients(&shares));
    assert_eq!(
        carol.load_keys(&v).await,
        Ok(()),
        "Carol keeps the older key"
    );

    // Once verified, both get the key again: Carol as members do...
    let carol_pub: PublicUser = alice.api.get(&format!("/users/{}", carol.id)).await;
    let (epoch, key) = alice.ring(&v).current().unwrap();
    let pk = carol_pub.account_key.as_ref().unwrap().verify().unwrap();
    let _: () = alice
        .api
        .post(
            &format!("/vaults/{v}/keys/grant"),
            &GrantKeysRequest {
                envelopes: vec![KeyEnvelope {
                    user_id: carol.id.clone(),
                    epoch,
                    envelope: keys::wrap_vault_key(key, &v, epoch, &carol.id, &pk.enc_public)
                        .unwrap(),
                    signature: None,
                }],
            },
        )
        .await;
    let members: Vec<MemberInfo> = alice.api.get(&format!("/vaults/{v}/members")).await;
    assert!(
        members
            .iter()
            .find(|m| m.user.id == carol.id)
            .unwrap()
            .has_current_key
    );
    // ...and Dave by sharing the note with him again.
    let vk: VaultKeysResponse = alice.api.get(&format!("/vaults/{v}/keys")).await;
    let rings = engine::object_keyrings(alice.ring(&v), &vk);
    let _: ObjectShareInfo = alice
        .api
        .post(
            &format!("/vaults/{v}/objects/n1/shares"),
            &share_dave(&rings["n1"]),
        )
        .await;
    let shares: Vec<ObjectShareInfo> = alice
        .api
        .get(&format!("/vaults/{v}/objects/n1/shares"))
        .await;
    assert!(recipients(&shares));
    // Someone who has the key can't be added twice.
    assert_eq!(
        alice
            .api
            .status(
                Method::POST,
                &format!("/vaults/{v}/objects/n1/shares"),
                Some(&share_dave(&rings["n1"]))
            )
            .await,
        409
    );
}
