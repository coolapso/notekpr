//! Repository tests against a real, migrated SQLite database (a temporary
//! file with the production settings: WAL, foreign keys).

use coolpsonotes_core::protocol::{PublicAccountKey, Role};

use super::*;
use crate::access;
use crate::db::{self, Db};
use crate::error::AppError;

struct TestDb {
    db: Db,
    _dir: tempfile::TempDir,
}

async fn test_db() -> TestDb {
    let dir = tempfile::tempdir().unwrap();
    let db = db::connect(&dir.path().join("test.db")).await.unwrap();
    db::MIGRATOR.run(&db).await.unwrap();
    TestDb { db, _dir: dir }
}

const NOW: i64 = 1_000_000;

async fn human(db: &Db, id: &str) {
    let email = format!("{id}@example.org");
    users::insert_human(db, id, id, Some(email.as_str()), false, NOW)
        .await
        .unwrap();
}

async fn agent(db: &Db, id: &str, owner: &str) {
    agents::insert(db, id, id, owner, NOW).await.unwrap();
}

async fn account_key(db: &Db, user: &str) {
    let key = PublicAccountKey {
        user_id: user.into(),
        key_version: 1,
        enc_public: vec![1; 32],
        sign_public: vec![2; 32],
        signature: vec![3; 64],
    };
    users::insert_account_key(db, &key, Some(&b"recovery"[..]), NOW)
        .await
        .unwrap();
}

/// A vault owned by `owner` with `members` as `(user, role)`.
async fn vault(db: &Db, id: &str, owner: &str, members: &[(&str, Role)]) {
    assert!(vaults::insert(db, id, owner, b"meta", NOW).await.unwrap());
    assert!(
        vaults::insert_member(db, id, owner, Role::Owner, owner, NOW)
            .await
            .unwrap()
    );
    for (u, r) in members {
        assert!(
            vaults::insert_member(db, id, u, *r, owner, NOW)
                .await
                .unwrap()
        );
    }
}

/// Share note `o` of vault `v` with `user`.
async fn share(db: &Db, v: &str, o: &str, user: &str, role: Role) {
    shares::ensure_object(db, v, o, "owner", NOW).await.unwrap();
    assert!(
        shares::insert(db, v, o, user, role, "owner", NOW)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn member_roles_and_owner_only_rules() {
    let t = test_db().await;
    let db = &t.db;
    for u in ["owner", "ed", "view", "stranger"] {
        human(db, u).await;
    }
    vault(
        db,
        "v",
        "owner",
        &[("ed", Role::Editor), ("view", Role::Viewer)],
    )
    .await;

    assert_eq!(
        vaults::member_role(db, "v", "owner").await.unwrap(),
        Some(Role::Owner)
    );
    assert_eq!(
        vaults::member_role(db, "v", "ed").await.unwrap(),
        Some(Role::Editor)
    );
    assert_eq!(
        vaults::member_role(db, "v", "stranger").await.unwrap(),
        None
    );
    assert_eq!(
        vaults::member_role(db, "nope", "owner").await.unwrap(),
        None
    );

    // Non-members get 404, members without the right role 403.
    assert!(matches!(
        access::member_role(db, "v", "stranger").await,
        Err(AppError::NotFound)
    ));
    assert!(access::require_owner(db, "v", "owner").await.is_ok());
    assert!(matches!(
        access::require_owner(db, "v", "ed").await,
        Err(AppError::Forbidden(_))
    ));
    assert!(matches!(
        access::require_owner(db, "v", "stranger").await,
        Err(AppError::NotFound)
    ));
    assert_eq!(
        access::require_writer(db, "v", "ed").await.unwrap(),
        Role::Editor
    );
    assert!(matches!(
        access::require_writer(db, "v", "view").await,
        Err(AppError::Forbidden(_))
    ));

    // Inserting a member twice is reported, not an error.
    assert!(
        !vaults::insert_member(db, "v", "ed", Role::Viewer, "owner", NOW)
            .await
            .unwrap()
    );
    assert!(!vaults::insert(db, "v", "ed", b"m", NOW).await.unwrap());

    assert_eq!(
        vaults::set_member_role(db, "v", "ed", Role::Viewer)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        vaults::member_role(db, "v", "ed").await.unwrap(),
        Some(Role::Viewer)
    );
    assert_eq!(
        vaults::set_member_role(db, "v", "stranger", Role::Editor)
            .await
            .unwrap(),
        0
    );

    // The owner can never be removed; others can.
    assert_eq!(vaults::delete_member(db, "v", "owner").await.unwrap(), 0);
    assert_eq!(vaults::delete_member(db, "v", "view").await.unwrap(), 1);
    assert_eq!(vaults::member_role(db, "v", "view").await.unwrap(), None);

    let mut ids = vaults::member_ids(db, "v").await.unwrap();
    ids.sort();
    assert_eq!(ids, ["ed", "owner"]);
    assert_eq!(vaults::list_for_member(db, "ed").await.unwrap().len(), 1);
    assert!(
        vaults::list_for_member(db, "view")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        vaults::get_for_member(db, "view", "v")
            .await
            .unwrap()
            .is_none()
    );
    let info = vaults::get_for_member(db, "ed", "v")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((info.role, info.member_count), (Role::Viewer, 2));

    // Deleting the vault removes every membership.
    vaults::delete(db, "v").await.unwrap();
    assert_eq!(vaults::member_role(db, "v", "owner").await.unwrap(), None);
}

#[tokio::test]
async fn object_access_through_membership_or_a_share() {
    let t = test_db().await;
    let db = &t.db;
    for u in ["owner", "ed", "bob", "carol"] {
        human(db, u).await;
    }
    vault(db, "v", "owner", &[("ed", Role::Editor)]).await;
    share(db, "v", "note", "bob", Role::Viewer).await;

    assert_eq!(
        access::object_access(db, "v", "note", "ed").await.unwrap(),
        (Role::Editor, true)
    );
    assert_eq!(
        access::object_access(db, "v", "note", "bob").await.unwrap(),
        (Role::Viewer, false)
    );
    // A share reaches only its own note, never another one or the vault.
    assert!(matches!(
        access::object_access(db, "v", "other", "bob").await,
        Err(AppError::NotFound)
    ));
    assert!(matches!(
        access::member_role(db, "v", "bob").await,
        Err(AppError::NotFound)
    ));
    assert!(matches!(
        access::object_access(db, "v", "note", "carol").await,
        Err(AppError::NotFound)
    ));

    assert!(
        !shares::insert(db, "v", "note", "bob", Role::Editor, "owner", NOW)
            .await
            .unwrap()
    );
    assert_eq!(
        shares::set_role(db, "v", "note", "bob", Role::Editor)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        shares::role(db, "v", "note", "bob").await.unwrap(),
        Some(Role::Editor)
    );
    assert_eq!(
        shares::set_role(db, "v", "note", "carol", Role::Editor)
            .await
            .unwrap(),
        0
    );

    // Purged notes grant nothing, even while the share row still exists.
    sqlx::query("UPDATE objects SET purged_at = 1 WHERE vault_id = 'v' AND id = 'note'")
        .execute(db)
        .await
        .unwrap();
    assert_eq!(shares::role(db, "v", "note", "bob").await.unwrap(), None);
    assert!(shares::shared_with(db, "bob").await.unwrap().is_empty());
    assert!(shares::exportable(db, "bob").await.unwrap().is_empty());
    sqlx::query("UPDATE objects SET purged_at = NULL WHERE vault_id = 'v' AND id = 'note'")
        .execute(db)
        .await
        .unwrap();

    let listed = shares::shared_with(db, "bob").await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (
            listed[0].object_id.as_str(),
            listed[0].vault_owner_id.as_str()
        ),
        ("note", "owner")
    );
    assert_eq!(
        shares::of_user(db, "bob").await.unwrap(),
        [("v".to_string(), "note".to_string())]
    );
    assert_eq!(
        shares::in_vault(db, "v").await.unwrap(),
        [("bob".to_string(), "note".to_string())]
    );

    // Membership supersedes shares; exports don't list the note twice.
    vaults::insert_member(db, "v", "bob", Role::Viewer, "owner", NOW)
        .await
        .unwrap();
    assert!(shares::exportable(db, "bob").await.unwrap().is_empty());
    shares::delete_in_vault_for(db, "v", "bob").await.unwrap();
    assert_eq!(shares::role(db, "v", "note", "bob").await.unwrap(), None);
    assert_eq!(shares::delete(db, "v", "note", "bob").await.unwrap(), 0);
}

#[tokio::test]
async fn people_connected_through_vaults_or_notes() {
    let t = test_db().await;
    let db = &t.db;
    for u in ["owner", "ed", "bob", "carol"] {
        human(db, u).await;
    }
    vault(db, "v", "owner", &[("ed", Role::Editor)]).await;
    share(db, "v", "note", "bob", Role::Viewer).await;

    assert!(users::share_a_vault(db, "owner", "ed").await.unwrap());
    assert!(!users::share_a_vault(db, "owner", "bob").await.unwrap());
    // Both directions: recipient ↔ any member of the note's vault.
    assert!(users::share_a_note(db, "bob", "ed").await.unwrap());
    assert!(users::share_a_note(db, "ed", "bob").await.unwrap());
    assert!(!users::share_a_note(db, "bob", "carol").await.unwrap());
    assert!(!users::share_a_note(db, "owner", "ed").await.unwrap());
}

#[tokio::test]
async fn key_envelopes_are_per_member_and_per_note() {
    let t = test_db().await;
    let db = &t.db;
    for u in ["owner", "ed", "bob"] {
        human(db, u).await;
    }
    account_key(db, "owner").await;
    account_key(db, "bob").await;
    vault(db, "v", "owner", &[("ed", Role::Editor)]).await;
    keys::insert_vault_key(db, "v", 1, "owner", b"o1", "owner", NOW)
        .await
        .unwrap();
    keys::insert_vault_key(db, "v", 1, "ed", b"e1", "owner", NOW)
        .await
        .unwrap();
    keys::put_vault_key(db, "v", 1, "ed", b"e1b", "owner", NOW)
        .await
        .unwrap();

    let k = keys::vault_keys(db, "v", "ed").await.unwrap();
    assert_eq!(k.current_epoch, 1);
    assert_eq!(k.envelopes.len(), 1);
    assert_eq!(k.envelopes[0].envelope, b"e1b");
    assert!(k.objects.is_empty());

    // Only members with account keys must receive vault keys.
    assert_eq!(
        vaults::member_ids_with_account_keys(db, "v").await.unwrap(),
        ["owner"]
    );

    keys::delete_vault_keys_of(db, "v", "ed").await.unwrap();
    assert!(
        keys::vault_keys(db, "v", "ed")
            .await
            .unwrap()
            .envelopes
            .is_empty()
    );

    // A note that isn't individually shared has no document keys.
    assert!(matches!(
        keys::document_keys(&t.db, "v", "note", "bob", Role::Viewer).await,
        Err(AppError::NotFound)
    ));
    share(db, "v", "note", "bob", Role::Viewer).await;
    let doc = 0x8000_0001_i64;
    shares::set_document_epoch(db, "v", "note", doc, b"title", NOW)
        .await
        .unwrap();
    keys::insert_document_key(db, "v", "note", doc, "", b"wrapped", "owner", NOW)
        .await
        .unwrap();
    keys::insert_document_key(db, "v", "note", doc, "bob", b"for-bob", "owner", NOW)
        .await
        .unwrap();
    let dk = keys::document_keys(db, "v", "note", "bob", Role::Viewer)
        .await
        .unwrap();
    assert_eq!(dk.current_epoch, doc as u32);
    assert_eq!(dk.envelopes.len(), 1);
    assert_eq!(dk.envelopes[0].envelope, b"for-bob");
    assert_eq!(dk.encrypted_meta.as_deref(), Some(&b"title"[..]));
    // Members get the copy wrapped under the vault key.
    let vk = keys::vault_keys(db, "v", "owner").await.unwrap();
    assert_eq!(vk.objects.len(), 1);
    assert_eq!(vk.objects[0].envelope, b"wrapped");
    assert_eq!(
        shares::recipient_ids_with_account_keys(db, "v", "note")
            .await
            .unwrap(),
        ["bob"]
    );
    let r = shares::recipients(db, "v", "note", doc).await.unwrap();
    assert!(r[0].has_current_key);
    let r = shares::recipients(db, "v", "note", doc + 1).await.unwrap();
    assert!(!r[0].has_current_key);

    keys::delete_document_keys_of(db, "v", "note", "bob")
        .await
        .unwrap();
    let dk = keys::document_keys(db, "v", "note", "bob", Role::Viewer)
        .await
        .unwrap();
    assert!(dk.envelopes.is_empty());
}

#[tokio::test]
async fn devices_belong_to_one_account() {
    let t = test_db().await;
    let db = &t.db;
    human(db, "alice").await;
    human(db, "mallory").await;

    assert!(
        devices::insert(db, "d1", "alice", "laptop", &[7; 32], None, NOW)
            .await
            .unwrap()
    );
    assert!(
        !devices::insert(db, "d1", "mallory", "x", &[8; 32], None, NOW)
            .await
            .unwrap()
    );
    assert_eq!(devices::count_pending(db, "alice").await.unwrap(), 1);
    assert_eq!(
        devices::owner_unless_revoked(db, "d1").await.unwrap(),
        Some("alice".to_string())
    );

    // Another account can't see, rename, approve or revoke it.
    assert!(
        devices::envelope(db, "d1", "mallory")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        devices::public_key(db, "d1", "mallory")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        devices::rename(db, "d1", "mallory", "mine").await.unwrap(),
        0
    );
    assert_eq!(devices::revoke(db, "d1", "mallory", NOW).await.unwrap(), 0);
    assert!(devices::list(db, "mallory", None).await.unwrap().is_empty());

    let (status, key) = devices::public_key(db, "d1", "alice")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((status.as_str(), key), ("pending", vec![7; 32]));
    devices::activate(db, "d1", b"env", NOW).await.unwrap();
    assert_eq!(
        devices::envelope(db, "d1", "alice").await.unwrap(),
        Some(("active".to_string(), Some(b"env".to_vec())))
    );
    let list = devices::list(db, "alice", Some("d1")).await.unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].current);

    // Revoking drops the envelope and hides the device everywhere.
    assert_eq!(devices::revoke(db, "d1", "alice", NOW).await.unwrap(), 1);
    assert_eq!(devices::revoke(db, "d1", "alice", NOW).await.unwrap(), 0);
    assert_eq!(devices::owner_unless_revoked(db, "d1").await.unwrap(), None);
    assert_eq!(
        devices::envelope(db, "d1", "alice").await.unwrap(),
        Some(("revoked".to_string(), None))
    );
    assert!(devices::list(db, "alice", None).await.unwrap().is_empty());
    assert_eq!(devices::rename(db, "d1", "alice", "x").await.unwrap(), 0);
}

#[tokio::test]
async fn sessions_are_found_by_token_hash_and_revoked_per_account() {
    let t = test_db().await;
    let db = &t.db;
    human(db, "alice").await;
    human(db, "bob").await;
    agent(db, "bot", "alice").await;
    devices::insert(db, "d1", "alice", "laptop", &[7; 32], Some(&b"e"[..]), NOW)
        .await
        .unwrap();

    let far = NOW + 1_000_000;
    sessions::insert(
        db,
        "s1",
        b"hash1",
        "alice",
        "web",
        Some("browser"),
        NOW,
        far,
    )
    .await
    .unwrap();
    sessions::insert(db, "s2", b"hash2", "alice", "cli", None, NOW, far)
        .await
        .unwrap();
    sessions::insert(
        db,
        "s3",
        b"hash3",
        "bot",
        "agent",
        Some("agent_x"),
        NOW,
        far,
    )
    .await
    .unwrap();
    sessions::insert(db, "old", b"hash4", "alice", "web", None, NOW, NOW + 1)
        .await
        .unwrap();
    sessions::bind_device(db, "s1", "d1").await.unwrap();

    assert!(
        sessions::by_token_hash(db, b"unknown")
            .await
            .unwrap()
            .is_none()
    );
    let s = sessions::by_token_hash(db, b"hash1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (s.id.as_str(), s.user_id.as_str(), s.device_id.as_deref()),
        ("s1", "alice", Some("d1"))
    );
    assert_eq!((s.user_kind.as_str(), s.kind.as_str()), ("human", "web"));
    assert!(!s.disabled && !s.owner_disabled && s.revoked_at.is_none());

    // An agent's session reflects its owner's account state.
    let a = sessions::by_token_hash(db, b"hash3")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (a.user_kind.as_str(), a.owner_id.as_deref()),
        ("agent", Some("alice"))
    );
    assert!(!a.owner_disabled);
    users::set_disabled(db, "alice", true).await.unwrap();
    let a = sessions::by_token_hash(db, b"hash3")
        .await
        .unwrap()
        .unwrap();
    assert!(a.owner_disabled && !a.disabled);
    assert_eq!(
        sessions::unrevoked_of_agents(db, "alice").await.unwrap(),
        [("bot".to_string(), "s3".to_string())]
    );

    // Only the session's own account can revoke it.
    assert_eq!(sessions::revoke(db, "s2", "bob", NOW).await.unwrap(), 0);
    assert_eq!(sessions::revoke(db, "s2", "alice", NOW).await.unwrap(), 1);
    assert_eq!(sessions::revoke(db, "s2", "alice", NOW).await.unwrap(), 0);
    let s2 = sessions::by_token_hash(db, b"hash2")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s2.revoked_at, Some(NOW));

    // Live sessions only: not revoked, not expired; the caller's is marked.
    let live = sessions::list_active(db, "alice", "s1", NOW + 10)
        .await
        .unwrap();
    assert_eq!(live.len(), 1);
    assert!(live[0].current && live[0].id == "s1");

    assert_eq!(
        sessions::revoke_for_device(db, "d1", NOW).await.unwrap(),
        ["s1"]
    );
    assert_eq!(
        sessions::revoke_for_credential(db, "bot", "other", NOW)
            .await
            .unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        sessions::revoke_for_credential(db, "bot", "agent_x", NOW)
            .await
            .unwrap(),
        ["s3"]
    );
    let mut left = sessions::unrevoked_ids(db, "alice").await.unwrap();
    left.sort();
    assert_eq!(left, ["old"]);
    assert_eq!(
        sessions::revoke_for_user(db, "alice", NOW).await.unwrap(),
        ["old"]
    );

    // Old sessions are forgotten eventually.
    sessions::delete_ended_before(db, NOW + 1).await.unwrap();
    assert!(
        sessions::by_token_hash(db, b"hash1")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn dev_sessions_end_by_issuer() {
    let t = test_db().await;
    let db = &t.db;
    human(db, "dev").await;
    human(db, "real").await;
    users::insert_identity(db, "dev-issuer", "dev", "dev", NOW)
        .await
        .unwrap();
    users::insert_identity(db, "https://idp", "real", "real", NOW)
        .await
        .unwrap();
    assert_eq!(
        users::identity_user(db, "dev-issuer", "dev").await.unwrap(),
        Some("dev".to_string())
    );
    assert_eq!(
        users::identity_user(db, "https://idp", "dev")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        users::count_humans(db, "dev-issuer", true).await.unwrap(),
        1
    );
    assert_eq!(
        users::count_humans(db, "dev-issuer", false).await.unwrap(),
        1
    );
    sessions::insert(db, "a", b"a", "dev", "web", None, NOW, NOW + 10)
        .await
        .unwrap();
    sessions::insert(db, "b", b"b", "real", "web", None, NOW, NOW + 10)
        .await
        .unwrap();
    assert_eq!(
        sessions::revoke_for_issuer(db, "dev-issuer", NOW)
            .await
            .unwrap(),
        1
    );
    assert!(
        sessions::by_token_hash(db, b"a")
            .await
            .unwrap()
            .unwrap()
            .revoked_at
            .is_some()
    );
    assert!(
        sessions::by_token_hash(db, b"b")
            .await
            .unwrap()
            .unwrap()
            .revoked_at
            .is_none()
    );
}

#[tokio::test]
async fn agents_are_visible_to_their_owner_only() {
    let t = test_db().await;
    let db = &t.db;
    human(db, "alice").await;
    human(db, "bob").await;
    agent(db, "bot", "alice").await;

    assert!(agents::get(db, "alice", "bot").await.unwrap().is_some());
    assert!(agents::get(db, "bob", "bot").await.unwrap().is_none());
    assert_eq!(agents::ids(db, "alice").await.unwrap(), ["bot"]);
    assert_eq!(agents::count(db, "bob").await.unwrap(), 0);
    let p = users::principal(db, "bot").await.unwrap().unwrap();
    assert!(p.is_agent() && p.owner_id.as_deref() == Some("alice"));

    agents::insert_credential(db, "c1", "bot", b"secret-hash", NOW)
        .await
        .unwrap();
    let c = agents::credential_login(db, "c1").await.unwrap().unwrap();
    assert_eq!(c.agent_id, "bot");
    assert_eq!(c.owner_disabled, Some(false));
    assert!(!c.agent_disabled && c.revoked_at.is_none());
    assert_eq!(
        agents::revoke_credential(db, "c1", "other", NOW)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        agents::revoke_credential(db, "c1", "bot", NOW)
            .await
            .unwrap(),
        1
    );
    assert!(
        agents::credential_login(db, "c1")
            .await
            .unwrap()
            .unwrap()
            .revoked_at
            .is_some()
    );

    // Someone else can't delete the agent; its owner can, and its
    // credentials go with it.
    agents::delete(db, "bot", "bob").await.unwrap();
    assert!(users::exists(db, "bot").await.unwrap());
    agents::delete(db, "bot", "alice").await.unwrap();
    assert!(!users::exists(db, "bot").await.unwrap());
    assert!(agents::credential_login(db, "c1").await.unwrap().is_none());
}

#[tokio::test]
async fn directory_search_respects_its_mode() {
    let t = test_db().await;
    let db = &t.db;
    human(db, "alice").await;
    human(db, "albert").await;
    agent(db, "al-bot", "alice").await;
    agent(db, "al-other", "albert").await;
    users::set_disabled(db, "albert", true).await.unwrap();

    // Disabled people and other people's agents never show up.
    assert_eq!(
        users::search(db, "alice", "al", "al%").await.unwrap(),
        ["al-bot"]
    );
    human(db, "alfred").await;
    assert_eq!(
        users::search(db, "alice", "al", "al%").await.unwrap(),
        ["al-bot", "alfred"]
    );
    // Exact e-mail mode doesn't match names.
    assert_eq!(
        users::search_exact_email(db, "alice", "al", "al%")
            .await
            .unwrap(),
        ["al-bot"]
    );
    assert_eq!(
        users::search_exact_email(db, "alice", "alfred@example.org", "x%")
            .await
            .unwrap(),
        ["alfred"]
    );
    assert_eq!(
        users::find(db, "ALFRED@example.org").await.unwrap(),
        ["alfred"]
    );
}

#[tokio::test]
async fn audit_entries_by_vault_and_user() {
    let t = test_db().await;
    let db = &t.db;
    human(db, "alice").await;
    audit::record(
        db,
        Some("alice"),
        None,
        Some("v"),
        "vault.create",
        None,
        serde_json::json!({}),
    )
    .await;
    audit::record(
        db,
        Some("alice"),
        Some("alice"),
        None,
        "login",
        None,
        serde_json::json!({ "x": 1 }),
    )
    .await;
    let v = audit::for_vault(db, "v").await.unwrap();
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].action, "vault.create");
    assert_eq!(v[0].actor_name.as_deref(), Some("alice"));
    let u = audit::for_user(db, "alice").await.unwrap();
    assert_eq!(u.len(), 1);
    assert_eq!(u[0].details, serde_json::json!({ "x": 1 }));
}

#[tokio::test]
async fn single_use_login_records() {
    let t = test_db().await;
    let db = &t.db;
    human(db, "alice").await;

    sessions::insert_login_code(db, b"code", "alice", NOW)
        .await
        .unwrap();
    assert_eq!(
        sessions::take_login_code(db, b"code").await.unwrap(),
        Some(("alice".to_string(), NOW))
    );
    assert_eq!(sessions::take_login_code(db, b"code").await.unwrap(), None);

    let st = sessions::OidcState {
        pkce_verifier: "v".into(),
        nonce: "n".into(),
        return_to: "/".into(),
        expires_at: NOW,
        browser_hash: Some(vec![1]),
    };
    sessions::insert_oidc_state(db, "state", &st).await.unwrap();
    let got = sessions::take_oidc_state(db, "state")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.browser_hash, Some(vec![1]));
    assert!(
        sessions::take_oidc_state(db, "state")
            .await
            .unwrap()
            .is_none()
    );

    assert!(
        sessions::insert_device_grant(db, b"dc", "ABCD-EFGH", "cli", NOW, NOW + 100)
            .await
            .unwrap()
    );
    assert!(
        !sessions::insert_device_grant(db, b"dc2", "ABCD-EFGH", "cli", NOW, NOW + 100)
            .await
            .unwrap()
    );
    assert!(
        sessions::pending_device_grant(db, "ABCD-EFGH", NOW + 100)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        sessions::decide_device_grant(db, "ABCD-EFGH", true, "alice", NOW)
            .await
            .unwrap(),
        1
    );
    // Decided once only.
    assert_eq!(
        sessions::decide_device_grant(db, "ABCD-EFGH", false, "alice", NOW)
            .await
            .unwrap(),
        0
    );
    let g = sessions::device_grant(db, b"dc").await.unwrap().unwrap();
    assert_eq!(
        (g.status.as_str(), g.user_id.as_deref()),
        ("approved", Some("alice"))
    );
}
