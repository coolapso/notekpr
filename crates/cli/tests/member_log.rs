//! The signed membership log against a server that lies: members the owner
//! never added, vault keys the owner never made, a rolled-back log, and the
//! checkpoint that starts the log of an older vault.

mod common;

use common::*;
use notekpr_cli::{manage, note_shares, ops};
use notekpr_core::doc::EntryKind;
use notekpr_core::engine::{self, VaultMeta, VaultReplica};
use notekpr_core::keys::{self, VaultKey};
use notekpr_core::memberlog::LogBody;
use notekpr_core::protocol::*;

async fn sql(srv: &TestServer, q: &'static str, binds: &[&str]) {
    let mut query = sqlx::query(q);
    for b in binds {
        query = query.bind(*b);
    }
    query.execute(&srv.state.db).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_the_owner_never_added_gets_no_keys() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let eve = login(&srv, "eve").await;
    manage::create_vault(&alice, "Family").await.unwrap();
    let ah = hub(&alice).await;
    let v = ah.resolve("Family").await.unwrap();
    let vid = v.lock().await.id().to_string();
    ops::put_document(&ah, "Family", "Plans.md", "secret\n", None)
        .await
        .unwrap();

    // The server adds Eve on its own.
    sql(
        &srv,
        "INSERT INTO vault_members (vault_id, user_id, role, added_at) VALUES (?, ?, 'editor', 0)",
        &[&vid, &eve.user_id],
    )
    .await;
    let granted = alice.grant_missing_keys(&v).await.unwrap();
    assert!(granted.is_empty(), "no key for eve: {granted:?}");
    let list = manage::members(&alice, &v).await.unwrap();
    let e = list
        .iter()
        .find(|m| m.member.user.id == eve.user_id)
        .unwrap();
    assert!(!e.vouched);
    assert!(
        list.iter()
            .filter(|m| m.member.user.id != eve.user_id)
            .all(|m| m.vouched)
    );
    // A new key goes to everyone else, never to her.
    alice.rotate_keys(&v).await.unwrap();
    assert!(eve.api.vault_keys(&vid).await.unwrap().envelopes.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vault_key_the_owner_never_made_is_refused() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    manage::create_vault(&alice, "Family").await.unwrap();
    let ah = hub(&alice).await;
    let v = ah.resolve("Family").await.unwrap();
    let vid = v.lock().await.id().to_string();
    manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .unwrap();
    let info = bob.api.vault(&vid).await.unwrap();
    bob.open_vault(&info).await.unwrap();

    // The server makes up epoch 2 (a key it knows) and hands it to Bob, so
    // that what Bob writes next would be readable to it.
    let fake = VaultKey::generate();
    let env = keys::wrap_vault_key(
        &fake,
        &vid,
        2,
        &bob.user_id,
        &bob.account().public().enc_public,
    )
    .unwrap();
    sqlx::query("INSERT INTO vault_keys (vault_id, epoch, user_id, envelope, created_at) VALUES (?, 2, ?, ?, 0)")
        .bind(&vid)
        .bind(&bob.user_id)
        .bind(env)
        .execute(&srv.state.db)
        .await
        .unwrap();
    sql(
        &srv,
        "UPDATE vaults SET key_epoch = 2 WHERE id = ?",
        &[&vid],
    )
    .await;
    let info = bob.api.vault(&vid).await.unwrap();
    let Err(err) = bob.open_vault(&info).await else {
        panic!("opened")
    };
    assert!(
        format!("{err:#}").contains("never announced by the owner"),
        "{err:#}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rolled_back_membership_log_is_noticed() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    let carol = login(&srv, "carol").await;
    manage::create_vault(&alice, "Family").await.unwrap();
    let ah = hub(&alice).await;
    let v = ah.resolve("Family").await.unwrap();
    let vid = v.lock().await.id().to_string();
    manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .unwrap();
    manage::share(&alice, &v, "carol", Role::Viewer, false)
        .await
        .unwrap();
    let info = bob.api.vault(&vid).await.unwrap();
    bob.open_vault(&info).await.unwrap();
    drop(carol);
    // The server forgets the last entry (e.g. to hide that Carol was added).
    sql(
        &srv,
        "DELETE FROM member_log WHERE vault_id = ? AND seq = (SELECT MAX(seq) FROM member_log WHERE vault_id = ?)",
        &[&vid, &vid],
    )
    .await;
    let Err(err) = bob.open_vault(&info).await else {
        panic!("opened")
    };
    assert!(format!("{err:#}").contains("membership history"), "{err:#}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_older_vault_gets_a_signed_checkpoint() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    let carol = login(&srv, "carol").await;
    // A vault from before the log, shared the old way: Bob is a member, and
    // one note is shared with Carol.
    let (mut req, ring) =
        engine::new_vault(alice.account(), &alice.user_id, &VaultMeta::named("Old"), 1).unwrap();
    req.log_entry = None;
    let info: VaultInfo = alice.api.post("/vaults", &req).await.unwrap();
    let vid = info.id.clone();
    let bob_user = alice.api.user(&bob.user_id).await.unwrap();
    let _: MemberInfo = alice
        .api
        .post(
            &format!("/vaults/{vid}/members"),
            &AddMemberRequest {
                user_id: bob.user_id.clone(),
                role: Role::Editor,
                key_envelope: Some(engine::share_envelope(&ring, &bob_user).unwrap()),
                log_entry: None,
            },
        )
        .await
        .unwrap();
    let mut r = VaultReplica::new(&vid, Role::Owner, ring.clone(), VaultMeta::named("Old"));
    let idx = r.index.create("n1", "Plan.md", EntryKind::Note, 1).unwrap();
    let p1 = r.local_change(INDEX_OBJECT_ID, idx).unwrap();
    let text = r
        .notes
        .entry("n1".into())
        .or_default()
        .set_text("plan\n")
        .unwrap();
    let p2 = r.local_change("n1", text).unwrap();
    let pushed = alice.api.push(&vid, vec![p1, p2]).await.unwrap();
    let state = r.note("n1").unwrap().encode_state();
    let (doc, key) = engine::start_sharing(
        &ring,
        "n1",
        &engine::ObjectMeta::titled("Plan"),
        &state,
        pushed.head_seq,
    )
    .unwrap();
    alice
        .api
        .put::<_, ()>(&format!("/vaults/{vid}/objects/n1/key"), &key)
        .await
        .unwrap();
    let carol_user = alice.api.user(&carol.user_id).await.unwrap();
    let _: ObjectShareInfo = alice
        .api
        .post(
            &format!("/vaults/{vid}/objects/n1/shares"),
            &AddObjectShareRequest {
                user_id: carol.user_id.clone(),
                role: Role::Editor,
                envelope: engine::object_share_envelope(&doc, "n1", &carol_user).unwrap(),
                log_entry: None,
            },
        )
        .await
        .unwrap();

    // The owner's client starts the log when it opens the vault: the
    // members, the existing share, then signatures are required.
    let ah = hub(&alice).await;
    let v = ah.resolve("Old").await.unwrap();
    let log: MemberLogResponse = alice.api.get(&format!("/vaults/{vid}/log")).await.unwrap();
    let kinds: Vec<&LogBody> = log.entries.iter().map(|e| &e.body).collect();
    assert_eq!(kinds.len(), 3, "{kinds:?}");
    match kinds[0] {
        LogBody::Checkpoint { members, epoch, .. } => {
            assert_eq!(*epoch, 1);
            assert_eq!(members.len(), 2);
        }
        other => panic!("expected a checkpoint, got {other:?}"),
    }
    assert!(
        matches!(kinds[1], LogBody::Share { object_id, member } if object_id == "n1" && member.user_id == carol.user_id)
    );
    assert_eq!(
        *kinds[2],
        LogBody::RequireSigned {
            from_seq: pushed.head_seq
        }
    );

    // Carol's (signed) edits still count for the vault's members.
    note_shares::write_shared(&carol, "Plan", "carol was here\n", true)
        .await
        .unwrap();
    ah.sync_vault(&vid).await.unwrap();
    assert_eq!(
        note(&ah, &vid, "Plan").await.as_deref(),
        Some("plan\ncarol was here\n")
    );

    // Bob verifies the log; from now on changes are signed.
    let binfo = bob.api.vault(&vid).await.unwrap();
    let bv = bob.open_vault(&binfo).await.unwrap();
    assert!(bv.log.has_log());
    let st = alice
        .api
        .delete(&format!("/vaults/{vid}/members/{}", bob.user_id))
        .await
        .unwrap_err();
    assert_eq!(
        st.status(),
        Some(400),
        "a removal without its signed new key"
    );
    let (_, epoch) = manage::unshare(&alice, &v, "bob").await.unwrap();
    assert_eq!(epoch, 2);
}

/// Insert an update the way a compromised server would.
async fn slip_in(
    srv: &TestServer,
    vault_id: &str,
    object_id: &str,
    author: &str,
    payload: Vec<u8>,
    signature: Option<Vec<u8>>,
) {
    let seq: i64 = sqlx::query_scalar(
        "UPDATE vaults SET head_seq = head_seq + 1 WHERE id = ? RETURNING head_seq",
    )
    .bind(vault_id)
    .fetch_one(&srv.state.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO updates (vault_id, seq, object_id, client_update_id, author_id, payload, created_at, signature)
         VALUES (?, ?, ?, ?, ?, ?, 0, ?)",
    )
    .bind(vault_id)
    .bind(seq)
    .bind(object_id)
    .bind(notekpr_core::random_id())
    .bind(author)
    .bind(payload)
    .bind(signature)
    .execute(&srv.state.db)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn content_a_server_slips_in_is_refused() {
    use notekpr_core::crypto::Purpose;
    use notekpr_core::memberlog;
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    let vic = login(&srv, "vic").await;
    manage::create_vault(&alice, "Family").await.unwrap();
    let ah = hub(&alice).await;
    let v = ah.resolve("Family").await.unwrap();
    let vid = v.lock().await.id().to_string();
    manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .unwrap();
    manage::share(&alice, &v, "vic", Role::Viewer, false)
        .await
        .unwrap();
    ops::put_document(&ah, "Family", "Plan.md", "real\n", None)
        .await
        .unwrap();
    ah.sync_vault(&vid).await.unwrap();

    // Vic only views, but holds the keys: with the server's help, Vic
    // writes "forged" into the note.
    let vh = hub(&vic).await;
    let sv = vh.resolve(&vid).await.unwrap();
    let vv = sv.lock().await;
    let oid = vv.find_note("Plan").unwrap().1.id;
    let forged = {
        let n = vv.replica.notes.get(&oid).unwrap();
        let fork = notekpr_cli::vault::fork_note(&n.encode_state()).unwrap();
        fork.set_text("forged\n").unwrap()
    };
    let payload = vv
        .replica
        .encrypt_object(Purpose::Update, &oid, &forged)
        .unwrap();
    drop(vv);
    // Unsigned, claiming to be Alice's.
    slip_in(&srv, &vid, &oid, &alice.user_id, payload.clone(), None).await;
    // Signed by Vic, claiming to be Alice's.
    let vic_sig = memberlog::sign_update(vic.account(), &vid, &oid, &payload);
    slip_in(
        &srv,
        &vid,
        &oid,
        &alice.user_id,
        payload.clone(),
        Some(vic_sig.clone()),
    )
    .await;
    // Signed by Vic as Vic, who was never allowed to write.
    slip_in(&srv, &vid, &oid, &vic.user_id, payload, Some(vic_sig)).await;

    let bh = hub(&bob).await;
    assert_eq!(note(&bh, &vid, "Plan").await.as_deref(), Some("real\n"));
    ah.sync_vault(&vid).await.unwrap();
    assert_eq!(note(&ah, &vid, "Plan").await.as_deref(), Some("real\n"));
    // Real edits keep flowing.
    ops::put_document(&ah, "Family", "Plan.md", "real\nand more\n", None)
        .await
        .unwrap();
    bh.sync_vault(&vid).await.unwrap();
    assert_eq!(
        note(&bh, &vid, "Plan").await.as_deref(),
        Some("real\nand more\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_note_key_the_owner_never_gave_is_refused() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let rita = login(&srv, "rita").await;
    manage::create_vault(&alice, "Family").await.unwrap();
    let ah = hub(&alice).await;
    let v = ah.resolve("Family").await.unwrap();
    let vid = v.lock().await.id().to_string();
    ops::put_document(&ah, "Family", "Trip.md", "coast\n", None)
        .await
        .unwrap();
    ah.sync_vault(&vid).await.unwrap();
    note_shares::share_note(&alice, &v, "Trip", "rita", Role::Editor, false)
        .await
        .unwrap();
    let (_, text) = note_shares::read_shared(&rita, "Trip").await.unwrap();
    assert_eq!(text, "coast\n");
    let oid = v.lock().await.find_note("Trip").unwrap().1.id;

    // The server makes up a newer key for Rita, so it could read her edits.
    let epoch: i64 =
        sqlx::query_scalar("SELECT key_epoch FROM objects WHERE vault_id = ? AND id = ?")
            .bind(&vid)
            .bind(&oid)
            .fetch_one(&srv.state.db)
            .await
            .unwrap();
    let fake = VaultKey::generate();
    let scope = keys::object_scope(&vid, &oid);
    let env = keys::wrap_vault_key(
        &fake,
        &scope,
        (epoch + 1) as u32,
        &rita.user_id,
        &rita.account().public().enc_public,
    )
    .unwrap();
    sqlx::query("INSERT INTO object_keys (vault_id, object_id, epoch, user_id, envelope, created_at) VALUES (?, ?, ?, ?, ?, 0)")
        .bind(&vid)
        .bind(&oid)
        .bind(epoch + 1)
        .bind(&rita.user_id)
        .bind(env)
        .execute(&srv.state.db)
        .await
        .unwrap();
    sqlx::query("UPDATE objects SET key_epoch = ? WHERE vault_id = ? AND id = ?")
        .bind(epoch + 1)
        .bind(&vid)
        .bind(&oid)
        .execute(&srv.state.db)
        .await
        .unwrap();
    // (Its title can't be read either: refer to it by id.)
    let err = note_shares::write_shared(&rita, &oid, "secret plans\n", true)
        .await
        .err()
        .expect("refused");
    assert!(
        format!("{err:#}").contains("confirmed by its owner"),
        "{err:#}"
    );
}
