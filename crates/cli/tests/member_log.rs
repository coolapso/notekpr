//! The signed membership log against a server that lies: members the owner
//! never added, vault keys the owner never made, a rolled-back log, and the
//! checkpoint that starts the log of an older vault.

mod common;

use common::*;
use notekpr_cli::account::{self, LoginOptions};
use notekpr_cli::{manage, note_shares, ops, prefs};
use notekpr_core::doc::EntryKind;
use notekpr_core::engine::{self, VaultMeta, VaultReplica};
use notekpr_core::keys::{self, VaultKey};
use notekpr_core::memberlog::LogBody;
use notekpr_core::protocol::*;

/// Sign `user` in for the first time; returns their recovery key too.
async fn first_device(
    srv: &TestServer,
    user: &str,
) -> (std::sync::Arc<notekpr_cli::session::Session>, String) {
    let out = account::login(&srv.dirs(user), user, dev_login(srv, user))
        .await
        .unwrap();
    (reopen(srv, user, user).await, out.recovery_key.unwrap())
}

/// Another device of `user` (in `dirs`), unlocked with their recovery key.
async fn second_device(
    srv: &TestServer,
    dirs: &str,
    user: &str,
    recovery_key: String,
) -> std::sync::Arc<notekpr_cli::session::Session> {
    let opts = LoginOptions {
        recovery_key: Some(recovery_key),
        approval_timeout: Some(std::time::Duration::from_secs(1)),
        ..dev_login(srv, user)
    };
    account::login(&srv.dirs(dirs), user, opts).await.unwrap();
    reopen(srv, dirs, user).await
}

fn dev_login(srv: &TestServer, user: &str) -> LoginOptions {
    LoginOptions {
        server: srv.url.clone(),
        dev_user: Some(user.to_string()),
        device_name: Some(format!("{user}'s test device")),
        ..Default::default()
    }
}

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

/// The same key handed to a vault that is open already (`notekpr sync`
/// fetches keys again on a key event or on content under an unknown key):
/// it is never put in place, so nothing is written under it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_open_vault_never_takes_a_key_the_owner_never_made() {
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
    let bh = hub(&bob).await;
    let bv = bh.resolve("Family").await.unwrap();
    assert_eq!(bv.lock().await.replica.keyring.current_epoch(), Some(1));

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

    // A key event.
    let Err(err) = bob.refresh_keys(&bv).await else {
        panic!("took the key")
    };
    assert!(
        format!("{err:#}").contains("never announced by the owner"),
        "{err:#}"
    );
    assert_eq!(bv.lock().await.replica.keyring.current_epoch(), Some(1));
    // The vault list says the vault moved on.
    let info = bob.api.vault(&vid).await.unwrap();
    assert!(bob.apply_info(&mut *bv.lock().await, &info).await.is_err());
    assert_eq!(bv.lock().await.replica.keyring.current_epoch(), Some(1));
    // Nor was it kept on disk.
    let stored = bob.load_local(&vid).unwrap().unwrap();
    assert_eq!(stored.replica.keyring.current_epoch(), Some(1));
}

/// A device that never opened the vault is shown it without its log (as
/// a vault from before logs): it knows better from where the person's other
/// apps saw the log (the synced preferences), so it refuses it, and the
/// owner's new device signs no new history over it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_device_knows_the_vault_had_a_log() {
    let srv = start().await;
    let (alice, alice_rk) = first_device(&srv, "alice").await;
    let (bob, bob_rk) = first_device(&srv, "bob").await;
    manage::create_vault(&alice, "Family").await.unwrap();
    let ah = hub(&alice).await;
    let v = ah.resolve("Family").await.unwrap();
    let vid = v.lock().await.id().to_string();
    manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .unwrap();
    // Bob's app opens it: where its log stands is now in his preferences.
    let info = bob.api.vault(&vid).await.unwrap();
    bob.open_vault(&info).await.unwrap();
    let heads = notekpr_cli::prefs::log_heads(&bob).await.unwrap();
    assert!(heads.contains_key(&vid), "{heads:?}");

    // The server hides the log.
    sql(&srv, "DELETE FROM member_log WHERE vault_id = ?", &[&vid]).await;
    // Bob's new device.
    let bob2 = second_device(&srv, "bob-2", "bob", bob_rk).await;
    let info = bob2.api.vault(&vid).await.unwrap();
    let Err(err) = bob2.open_vault(&info).await else {
        panic!("opened without its log")
    };
    assert!(
        format!("{err:#}").contains("no longer has what your apps saw"),
        "{err:#}"
    );
    // Alice's new device: no checkpoint over what the server lists.
    let alice2 = second_device(&srv, "alice-2", "alice", alice_rk).await;
    let info = alice2.api.vault(&vid).await.unwrap();
    assert!(alice2.open_vault(&info).await.is_err());
    let logged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM member_log WHERE vault_id = ?")
        .bind(&vid)
        .fetch_one(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(logged, 0, "nothing was signed");
}

/// A new vault requires signed changes from its start: the entry after
/// `create` sets the cut-off at 0, so no position the server gives content
/// makes it exempt. Quick notes too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_vault_is_signed_from_its_start() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Family").await.unwrap();
    manage::create_quick_notes(&alice).await.unwrap().unwrap();
    // A server that doesn't store the requirement with the vault (or an
    // older one), and then says the vault has got further than it has: the
    // app requires signatures from the start itself, never from there.
    let meta = VaultMeta::named("Trips");
    let (mut req, ring) = engine::new_vault(alice.account(), &alice.user_id, &meta, 1).unwrap();
    req.signed_from_start = None;
    let trips = manage::create_vault_with(&alice, &req, ring, meta)
        .await
        .unwrap();
    sqlx::query("UPDATE vaults SET head_seq = 7 WHERE id = ?")
        .bind(trips.id())
        .execute(&srv.state.db)
        .await
        .unwrap();
    let ah = hub(&alice).await;
    assert_eq!(ah.vaults().len(), 3);
    for v in ah.vaults() {
        let g = v.lock().await;
        let entries = &g.log.entries;
        assert!(
            matches!(entries[0].body, LogBody::Create { .. }),
            "{entries:?}"
        );
        assert_eq!(entries[1].body, LogBody::RequireSigned { from_seq: 0 });
        assert_eq!(g.log.state.as_ref().unwrap().signed_from, Some(0));
    }
}

/// The server shares a note with Eve on its own: renewing the note's key,
/// or the vault's (which renews every note's), gives her nothing. Only the
/// people the log has the note shared with get it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_note_recipient_the_owner_never_added_gets_no_key() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    let eve = login(&srv, "eve").await;
    manage::create_vault(&alice, "Family").await.unwrap();
    let ah = hub(&alice).await;
    let v = ah.resolve("Family").await.unwrap();
    let vid = v.lock().await.id().to_string();
    ops::put_document(&ah, "Family", "Plan.md", "secret\n", None)
        .await
        .unwrap();
    ah.sync_vault(&vid).await.unwrap();
    note_shares::share_note(&alice, &v, "Plan", "bob", Role::Viewer, false)
        .await
        .unwrap();
    let oid = v.lock().await.find_note("Plan").unwrap().1.id;
    sql(
        &srv,
        "INSERT INTO object_shares (vault_id, object_id, user_id, role, added_at) VALUES (?, ?, ?, 'viewer', 0)",
        &[&vid, &oid, &eve.user_id],
    )
    .await;
    let keys_of = |user: String| {
        let (db, vid, oid) = (srv.state.db.clone(), vid.clone(), oid.clone());
        async move {
            let n: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM object_keys WHERE vault_id = ? AND object_id = ? AND user_id = ?",
            )
            .bind(&vid)
            .bind(&oid)
            .bind(&user)
            .fetch_one(&db)
            .await
            .unwrap();
            n
        }
    };
    let bob_id = bob.user_id.clone();
    let before = keys_of(bob_id.clone()).await;

    // A new note key (as after someone left it): Bob's, never Eve's.
    note_shares::rotate_note_key(&alice, &v, &vid, &oid)
        .await
        .unwrap();
    assert_eq!(keys_of(bob_id.clone()).await, before + 1);
    assert_eq!(keys_of(eve.user_id.clone()).await, 0);
    // A new vault key renews the note's too.
    alice.rotate_keys(&v).await.unwrap();
    assert_eq!(keys_of(bob_id).await, before + 2);
    assert_eq!(keys_of(eve.user_id.clone()).await, 0);
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
    req.signed_from_start = None;
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

/// The server hands Rita a new key for a note shared with her, signed by
/// an account of its own (Eve): she doesn't see the vault's log, but the
/// server can't show Eve an owner from where the vault's history starts, so
/// the key isn't used. Nor by Sam, who never opened the note (nothing
/// pinned yet): without a proof, no key counts. The vault's real other
/// owner (Bob) renewing it is taken: the server shows how Alice made him
/// one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_note_key_counts_only_from_an_owner_the_log_shows() {
    use notekpr_core::memberlog;
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    let rita = login(&srv, "rita").await;
    let eve = login(&srv, "eve").await;
    let sam = login(&srv, "sam").await;
    manage::create_vault(&alice, "Family").await.unwrap();
    let ah = hub(&alice).await;
    let v = ah.resolve("Family").await.unwrap();
    let vid = v.lock().await.id().to_string();
    manage::share(&alice, &v, "bob", Role::Owner, false)
        .await
        .unwrap();
    ops::put_document(&ah, "Family", "Trip.md", "coast\n", None)
        .await
        .unwrap();
    ah.sync_vault(&vid).await.unwrap();
    for who in ["rita", "sam"] {
        note_shares::share_note(&alice, &v, "Trip", who, Role::Editor, false)
            .await
            .unwrap();
    }
    let (_, text) = note_shares::read_shared(&rita, "Trip").await.unwrap();
    assert_eq!(text, "coast\n");
    let oid = v.lock().await.find_note("Trip").unwrap().1.id;

    // Bob, an owner too, renews the note's key: Rita takes it.
    let bh = hub(&bob).await;
    let bv = bh.resolve("Family").await.unwrap();
    note_shares::rotate_note_key(&bob, &bv, &vid, &oid)
        .await
        .unwrap();
    let keys: ObjectKeysResponse = rita
        .api
        .get(&format!("/vaults/{vid}/objects/{oid}/keys"))
        .await
        .unwrap();
    assert!(
        !keys.signer_proof.is_empty(),
        "the server shows Bob an owner"
    );
    note_shares::write_shared(&rita, &oid, "coast and hills\n", true)
        .await
        .unwrap();

    // The server makes up a newer key for Rita and Sam, signed by Eve, so
    // that it could read what they write.
    let epoch: i64 =
        sqlx::query_scalar("SELECT key_epoch FROM objects WHERE vault_id = ? AND id = ?")
            .bind(&vid)
            .bind(&oid)
            .fetch_one(&srv.state.db)
            .await
            .unwrap();
    let next = (epoch + 1) as u32;
    let fake = VaultKey::generate();
    for who in [&rita, &sam] {
        let env = keys::wrap_vault_key(
            &fake,
            &keys::object_scope(&vid, &oid),
            next,
            &who.user_id,
            &who.account().public().enc_public,
        )
        .unwrap();
        let grant =
            memberlog::sign_note_grant(eve.account(), &vid, &oid, &who.user_id, next, &fake);
        sqlx::query("INSERT INTO object_keys (vault_id, object_id, epoch, user_id, envelope, created_at, signature, signed_by) VALUES (?, ?, ?, ?, ?, 0, ?, ?)")
            .bind(&vid)
            .bind(&oid)
            .bind(epoch + 1)
            .bind(&who.user_id)
            .bind(env)
            .bind(grant)
            .bind(&eve.user_id)
            .execute(&srv.state.db)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE objects SET key_epoch = ? WHERE vault_id = ? AND id = ?")
        .bind(epoch + 1)
        .bind(&vid)
        .bind(&oid)
        .execute(&srv.state.db)
        .await
        .unwrap();
    for who in [&rita, &sam] {
        let err = note_shares::write_shared(who, &oid, "secret plans\n", true)
            .await
            .err()
            .expect("refused");
        assert!(
            format!("{err:#}").contains("didn't show that whoever gave you its key owns"),
            "{err:#}"
        );
    }
}

/// An owner who renewed a note's key stops being one: an owner who still is
/// signs the key again, so the person the note is shared with keeps using
/// it (their app takes a key only from an owner).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_note_key_from_an_owner_since_demoted_is_signed_again() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    let rita = login(&srv, "rita").await;
    manage::create_vault(&alice, "Family").await.unwrap();
    let ah = hub(&alice).await;
    let v = ah.resolve("Family").await.unwrap();
    let vid = v.lock().await.id().to_string();
    manage::share(&alice, &v, "bob", Role::Owner, false)
        .await
        .unwrap();
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
    let bh = hub(&bob).await;
    let bv = bh.resolve("Family").await.unwrap();
    note_shares::rotate_note_key(&bob, &bv, &vid, &oid)
        .await
        .unwrap();
    let signer = || async {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT signed_by FROM object_keys WHERE vault_id = ? AND object_id = ? AND user_id = ? ORDER BY epoch DESC LIMIT 1",
        )
        .bind(&vid)
        .bind(&oid)
        .bind(&rita.user_id)
        .fetch_one(&srv.state.db)
        .await
        .unwrap()
    };
    assert_eq!(signer().await, Some(bob.user_id.clone()));
    // (Alice's running app does this when told the key was renewed.)
    alice.refresh_note_keys(&v, &oid).await.unwrap();

    // Bob is an editor now: Alice's app signs Rita's key again.
    manage::share(&alice, &v, "bob", Role::Editor, false)
        .await
        .unwrap();
    assert_eq!(signer().await, Some(alice.user_id.clone()));
    note_shares::write_shared(&rita, &oid, "hills\n", true)
        .await
        .unwrap();
    let (_, text) = note_shares::read_shared(&rita, "Trip").await.unwrap();
    assert_eq!(text, "coast\nhills\n");
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
    // (Its title can't be read either: refer to it by id.) No owner signed
    // it, so the server has no owner to show.
    let err = note_shares::write_shared(&rita, &oid, "secret plans\n", true)
        .await
        .err()
        .expect("refused");
    assert!(
        format!("{err:#}").contains("didn't show that whoever gave you its key owns"),
        "{err:#}"
    );
}

/// The server hands the CLI an older copy of the person's settings, from
/// before the web app pinned Bob's key: what the CLI saves again still has
/// the pin, counted newer than any copy before, so the web app (which takes
/// the newest) never forgets it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_older_settings_copy_is_never_saved_again_without_whom_we_trust() {
    use serde_json::{Map, Value, json};
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let pin = |who: &'static str, fp: &'static str| {
        move |p: &mut Map<String, Value>| {
            let mut pins = p.get("pins").cloned().unwrap_or_else(|| json!({}));
            pins[who] = fp.into();
            p.insert("pins".into(), pins);
            true
        }
    };
    let stored = || async {
        let ct: Vec<u8> = sqlx::query_scalar("SELECT ciphertext FROM user_prefs WHERE user_id = ?")
            .bind(&alice.user_id)
            .fetch_one(&srv.state.db)
            .await
            .unwrap();
        ct
    };
    let open = |ct: &[u8]| -> Value {
        serde_json::from_slice(&alice.account().open_private(&alice.user_id, ct).unwrap()).unwrap()
    };
    // As the web app would: Carol's key pinned, later Bob's.
    prefs::update(&alice, pin("carol", "fp-carol"))
        .await
        .unwrap();
    let older = stored().await;
    prefs::update(&alice, pin("bob", "fp-bob")).await.unwrap();
    let newest = open(&stored().await)["counter"].as_u64().unwrap();

    sqlx::query("UPDATE user_prefs SET ciphertext = ? WHERE user_id = ?")
        .bind(&older)
        .bind(&alice.user_id)
        .execute(&srv.state.db)
        .await
        .unwrap();
    prefs::save_log_head(&alice, "v", (3, vec![7; 32]))
        .await
        .unwrap();
    let saved = open(&stored().await);
    assert_eq!(saved["pins"]["bob"], "fp-bob", "{saved}");
    assert_eq!(saved["pins"]["carol"], "fp-carol");
    assert!(saved["counter"].as_u64().unwrap() > newest, "{saved}");
    assert_eq!(saved["log_heads"]["v"]["seq"], 3);
}
