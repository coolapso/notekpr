//! The signed membership log against a server that lies: members the owner
//! never added, vault keys the owner never made, a rolled-back log, and the
//! checkpoint that starts the log of an older vault.

mod common;

use common::*;
use coolpsonotes_client::{manage, ops};
use coolpsonotes_core::engine::{self, VaultMeta};
use coolpsonotes_core::keys::{self, VaultKey};
use coolpsonotes_core::memberlog::LogBody;
use coolpsonotes_core::protocol::*;

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
    let err = alice.rotate_keys(&v).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("with a key you never added"),
        "{err:#}"
    );
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
        "DELETE FROM member_log WHERE vault_id = ? AND seq = 2",
        &[&vid],
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
    // A vault from before the log, shared the old way.
    let (mut req, ring) =
        engine::new_vault(alice.account(), &alice.user_id, &VaultMeta::named("Old"), 1).unwrap();
    req.log_entry = None;
    let info: VaultInfo = alice.api.post("/vaults", &req).await.unwrap();
    let bob_user = alice.api.user(&bob.user_id).await.unwrap();
    let _: MemberInfo = alice
        .api
        .post(
            &format!("/vaults/{}/members", info.id),
            &AddMemberRequest {
                user_id: bob.user_id.clone(),
                role: Role::Editor,
                key_envelope: Some(engine::share_envelope(&ring, &bob_user).unwrap()),
                log_entry: None,
            },
        )
        .await
        .unwrap();

    // The owner's client starts the log when it opens the vault.
    let ah = hub(&alice).await;
    let v = ah.resolve("Old").await.unwrap();
    let log: MemberLogResponse = alice
        .api
        .get(&format!("/vaults/{}/log", info.id))
        .await
        .unwrap();
    assert_eq!(log.entries.len(), 1);
    match &log.entries[0].body {
        LogBody::Checkpoint { members, epoch, .. } => {
            assert_eq!(*epoch, 1);
            assert_eq!(members.len(), 2);
        }
        other => panic!("expected a checkpoint, got {other:?}"),
    }
    // Bob verifies it; from now on changes are signed.
    let binfo = bob.api.vault(&info.id).await.unwrap();
    let bv = bob.open_vault(&binfo).await.unwrap();
    assert!(bv.log.has_log());
    let st = alice
        .api
        .delete(&format!("/vaults/{}/members/{}", info.id, bob.user_id))
        .await
        .unwrap_err();
    assert_eq!(st.status(), Some(409), "unsigned removal");
    let (_, epoch) = manage::unshare(&alice, &v, "bob").await.unwrap();
    assert_eq!(epoch, 2);
}
