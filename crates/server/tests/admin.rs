//! Instance settings (limits, read-only mode) and admin clients.

mod common;

use common::*;
use notekpr_core::doc::NoteDoc;
use notekpr_core::protocol::*;
use notekpr_core::random_id;
use reqwest::Method;

use notekpr_server::admin_clients;

/// The token of the "operator" admin client, which has every scope.
const TOKEN: &str = "notekpr_a_operator-token-for-the-tests-0123456789";

/// A server with the "operator" admin client, set up as
/// `notekpr-server admin-client ensure` does.
async fn start_with_operator() -> TestServer {
    with_operator(start().await).await
}

async fn with_operator(srv: TestServer) -> TestServer {
    admin_clients::ensure(
        &srv.state.db,
        "operator",
        &AdminScope::ALL,
        &[TOKEN.to_string()],
    )
    .await
    .unwrap();
    srv
}

/// Calls as an admin client.
fn as_client(srv: &TestServer, token: &str, m: Method, path: &str) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .request(m, format!("{}{path}", srv.base))
        .bearer_auth(token)
}

/// Calls as the "operator" admin client.
fn operator(srv: &TestServer, m: Method, path: &str) -> reqwest::RequestBuilder {
    as_client(srv, TOKEN, m, path)
}

async fn status(r: reqwest::RequestBuilder) -> u16 {
    r.send().await.unwrap().status().as_u16()
}

async fn settings(srv: &TestServer, change: serde_json::Value) -> AdminSettings {
    let r = operator(srv, Method::PATCH, "/admin/settings")
        .json(&change)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200, "{}", r.text().await.unwrap());
    r.json().await.unwrap()
}

async fn error_code(r: reqwest::Response) -> (u16, String) {
    let st = r.status().as_u16();
    let body: ApiError = r.json().await.unwrap();
    (st, body.error)
}

async fn create_agent(u: &User) -> Result<CreateAgentResponse, (u16, String)> {
    u.api
        .call(
            Method::POST,
            "/agents",
            Some(&CreateAgentRequest {
                name: "Helper".into(),
            }),
        )
        .await
}

async fn try_create_vault(u: &User) -> u16 {
    // The server refuses before looking at the key material.
    let req = CreateVaultRequest {
        id: random_id(),
        encrypted_meta: vec![1; 64],
        key_envelope: KeyEnvelope {
            user_id: u.id.clone(),
            epoch: 1,
            envelope: vec![1; 64],
            signature: None,
        },
        log_entry: None,
        kind: Default::default(),
    };
    u.api.status(Method::POST, "/vaults", Some(&req)).await
}

#[tokio::test]
async fn an_admin_client_manages_the_instance_but_never_reaches_a_vault() {
    let srv = start_with_operator().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let bob = new_user(&srv.base, "bob").await;
    let v = alice.create_vault("Private").await;
    let note = NoteDoc::new();
    let u = note.set_text("not for the operator").unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n", &u)])
        .await
        .unwrap();

    // Settings, accounts and usage.
    let s: AdminSettings = operator(&srv, Method::GET, "/admin/settings")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s.settings, InstanceSettings::default());
    assert_eq!(s.read_only, None);
    assert!(s.storage_bytes > 0);
    let users: Vec<AdminUser> = operator(&srv, Method::GET, "/admin/users")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(users.len(), 2);
    let found: Vec<AdminUser> = operator(&srv, Method::GET, "/admin/users?identity=dev:bob")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, bob.id);
    let alices: Vec<AdminUser> = operator(
        &srv,
        Method::GET,
        "/admin/users?email=&identity=dev%3Aalice",
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(alices[0].owned_vault_count, 1);
    assert_eq!(alices[0].vault_allowance, Allowance::Default);
    let stats: AdminStats = operator(&srv, Method::GET, "/admin/stats")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stats.vaults, 1);
    // The version the server was built as, as in `/info`.
    assert_eq!(stats.version, notekpr_core::VERSION);
    let info: ServerInfo = Api::new(&srv.base).get("/info").await;
    assert_eq!(info.version, notekpr_core::VERSION);

    // It manages accounts: disabling ends the person's sessions.
    let r = operator(&srv, Method::PATCH, &format!("/admin/users/{}", bob.id))
        .json(&AdminUpdateUser {
            disabled: Some(true),
            ..Default::default()
        })
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(bob.api.status::<()>(Method::GET, "/me", None).await, 401);

    // No vault, note, key or account route accepts its token.
    for (m, path) in [
        (Method::GET, "/me".to_string()),
        (Method::GET, "/vaults".to_string()),
        (Method::GET, format!("/vaults/{v}")),
        (Method::GET, format!("/vaults/{v}/keys")),
        (Method::GET, format!("/vaults/{v}/members")),
        (Method::GET, format!("/vaults/{v}/log")),
        (Method::GET, format!("/vaults/{v}/changes?since=0")),
        (Method::GET, format!("/vaults/{v}/objects/n")),
        (Method::GET, format!("/vaults/{v}/objects/n/snapshots")),
        (Method::GET, format!("/vaults/{v}/objects/n/keys")),
        (Method::GET, format!("/vaults/{v}/audit")),
        (Method::GET, format!("/vaults/{v}/attachments/a")),
        (Method::POST, format!("/vaults/{v}/updates")),
        (Method::DELETE, format!("/vaults/{v}")),
        (Method::GET, "/shares".to_string()),
        (Method::GET, format!("/users/{}", alice.id)),
        (Method::GET, "/users/search?q=al".to_string()),
        (Method::GET, "/me/export".to_string()),
        (Method::GET, "/me/recovery".to_string()),
        (Method::GET, "/devices".to_string()),
        (Method::GET, "/agents".to_string()),
    ] {
        assert_eq!(
            status(operator(&srv, m.clone(), &path)).await,
            401,
            "{m} {path}"
        );
    }
    // Nor the WebSocket.
    let (mut ws, _) = tokio_tungstenite::connect_async(&srv.ws).await.unwrap();
    use futures::{SinkExt, StreamExt};
    let auth = serde_json::to_string(&ClientMessage::Auth {
        token: TOKEN.into(),
        app: None,
    })
    .unwrap();
    ws.send(tokio_tungstenite::tungstenite::Message::Text(auth.into()))
        .await
        .unwrap();
    let first = ws.next().await.unwrap().unwrap();
    let m: ServerMessage = serde_json::from_str(first.to_text().unwrap()).unwrap();
    assert!(matches!(m, ServerMessage::Error { code, .. } if code == "unauthorized"));

    // A token nobody has gets nothing; a session token is no admin token.
    let unknown = format!("{ADMIN_TOKEN_PREFIX}{}", "0".repeat(40));
    assert_eq!(
        status(as_client(&srv, &unknown, Method::GET, "/admin/settings")).await,
        401
    );
    let carol = new_user(&srv.base, "carol").await;
    assert_eq!(
        carol
            .api
            .status::<()>(Method::GET, "/admin/settings", None)
            .await,
        403
    );
    // The administrators (people) have the settings by default.
    let s: AdminSettings = alice.api.get("/admin/settings").await;
    assert!(!s.settings.read_only);
}

#[tokio::test]
async fn admin_clients_have_their_scopes_and_hand_out_no_more() {
    let srv = start().await;
    let alice = new_user(&srv.base, "alice").await;
    let bob = new_user(&srv.base, "bob").await;
    let create = |name: &str, scopes: &[AdminScope]| {
        let req = CreateAdminClient {
            name: name.into(),
            scopes: scopes.to_vec(),
        };
        let api = alice.api.clone();
        async move {
            api.call::<_, CreatedAdminToken>(Method::POST, "/admin/clients", Some(&req))
                .await
        }
    };

    // A billing system: settings and accounts, nothing else.
    let billing = create("billing", &[AdminScope::Settings, AdminScope::Accounts])
        .await
        .unwrap();
    assert!(billing.token.starts_with(ADMIN_TOKEN_PREFIX));
    assert_eq!(
        billing.client.scopes,
        vec![AdminScope::Read, AdminScope::Settings, AdminScope::Accounts]
    );
    let t = billing.token.clone();
    let r = as_client(&srv, &t, Method::PATCH, "/admin/settings")
        .json(&serde_json::json!({ "vaults_per_user": 1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let r = as_client(&srv, &t, Method::PATCH, &format!("/admin/users/{}", bob.id))
        .json(&serde_json::json!({ "vault_allowance": 3 }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let r = as_client(&srv, &t, Method::PATCH, &format!("/admin/users/{}", bob.id))
        .json(&serde_json::json!({ "is_admin": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status().as_u16(),
        403,
        "the admin role needs the admins scope"
    );
    let r = as_client(&srv, &t, Method::POST, "/admin/clients")
        .json(&CreateAdminClient {
            name: "more".into(),
            scopes: vec![],
        })
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status().as_u16(),
        403,
        "making clients needs the clients scope"
    );

    // A dashboard only reads.
    let monitor = create("monitor", &[]).await.unwrap();
    assert_eq!(monitor.client.scopes, vec![AdminScope::Read]);
    assert_eq!(
        status(as_client(&srv, &monitor.token, Method::GET, "/admin/users")).await,
        200
    );
    let r = as_client(&srv, &monitor.token, Method::PATCH, "/admin/settings")
        .json(&serde_json::json!({ "read_only": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 403);

    // Delegation: a client with the clients scope hands out what it has.
    let delegator = create("delegator", &[AdminScope::Accounts, AdminScope::Clients])
        .await
        .unwrap();
    let d = delegator.token.clone();
    let made = |name: &'static str, scopes: Vec<AdminScope>| {
        as_client(&srv, &d, Method::POST, "/admin/clients")
            .json(&CreateAdminClient {
                name: name.into(),
                scopes,
            })
            .send()
    };
    let r = made("sub", vec![AdminScope::Accounts]).await.unwrap();
    assert_eq!(r.status().as_u16(), 201);
    let sub: CreatedAdminToken = r.json().await.unwrap();
    let r = made("sneaky", vec![AdminScope::Settings]).await.unwrap();
    assert_eq!(r.status().as_u16(), 403, "no scopes it doesn't have");
    // It can't touch a client with more than it has, nor raise one.
    let r = as_client(
        &srv,
        &d,
        Method::DELETE,
        &format!("/admin/clients/{}", billing.client.id),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(r.status().as_u16(), 403);
    let r = as_client(
        &srv,
        &d,
        Method::PATCH,
        &format!("/admin/clients/{}", sub.client.id),
    )
    .json(&UpdateAdminClient {
        scopes: Some(vec![AdminScope::Admins]),
    })
    .send()
    .await
    .unwrap();
    assert_eq!(r.status().as_u16(), 403);
    // Rotating: a second token, then the first one goes.
    let r = as_client(
        &srv,
        &d,
        Method::POST,
        &format!("/admin/clients/{}/tokens", sub.client.id),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(r.status().as_u16(), 201);
    let rotated: CreatedAdminToken = r.json().await.unwrap();
    assert_eq!(rotated.client.tokens.len(), 2);
    let old = &sub.client.tokens[0];
    let r = as_client(
        &srv,
        &d,
        Method::DELETE,
        &format!("/admin/clients/{}/tokens/{}", sub.client.id, old.id),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(r.status().as_u16(), 204);
    assert_eq!(
        status(as_client(&srv, &sub.token, Method::GET, "/admin/users")).await,
        401
    );
    assert_eq!(
        status(as_client(&srv, &rotated.token, Method::GET, "/admin/users")).await,
        200
    );

    // The list shows hints, never tokens; deleting a client ends its tokens.
    let r = alice
        .api
        .req(Method::GET, "/admin/clients")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!r.contains(&billing.token[ADMIN_TOKEN_PREFIX.len()..]));
    let clients: Vec<AdminClientInfo> = serde_json::from_str(&r).unwrap();
    assert_eq!(clients.len(), 4);
    let hint = &clients.iter().find(|c| c.name == "billing").unwrap().tokens[0].hint;
    assert!(billing.token.ends_with(hint.trim_start_matches('…')));
    assert_eq!(
        alice
            .api
            .status::<()>(
                Method::DELETE,
                &format!("/admin/clients/{}", billing.client.id),
                None
            )
            .await,
        204
    );
    assert_eq!(
        status(as_client(&srv, &t, Method::GET, "/admin/users")).await,
        401
    );
    // Names are unique.
    assert_eq!(create("monitor", &[]).await.unwrap_err().0, 409);
}

#[tokio::test]
async fn an_adminless_instance_has_no_administrators_among_its_people() {
    let srv = with_operator(
        start_with(|c| {
            c.adminless = true;
            // Ignored on an admin-less instance, like being the first.
            c.admins = vec!["dev:bob".into()];
        })
        .await,
    )
    .await;
    let alice = new_user(&srv.base, "alice").await;
    let bob = new_user(&srv.base, "bob").await;
    for u in [&alice, &bob] {
        let me: MeResponse = u.api.get("/me").await;
        assert!(!me.user.is_admin, "{} isn't an administrator", u.name);
        // Nothing of the administration: accounts, settings, admin clients.
        for path in [
            "/admin/users",
            "/admin/stats",
            "/admin/settings",
            "/admin/clients",
        ] {
            assert_eq!(
                u.api.status::<()>(Method::GET, path, None).await,
                403,
                "{path}"
            );
        }
        assert_eq!(
            u.api
                .status(
                    Method::POST,
                    "/admin/clients",
                    Some(&CreateAdminClient {
                        name: "mine".into(),
                        scopes: vec![],
                    })
                )
                .await,
            403
        );
    }
    // Their vaults are theirs as anywhere.
    let mut alice = alice;
    alice.create_vault("Mine").await;
    // Only admin clients administer, and the admin role can't be given.
    assert_eq!(
        status(operator(&srv, Method::GET, "/admin/settings")).await,
        200
    );
    let r = operator(&srv, Method::PATCH, &format!("/admin/users/{}", alice.id))
        .json(&serde_json::json!({ "is_admin": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);
    let users: Vec<AdminUser> = operator(&srv, Method::GET, "/admin/users")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(users.iter().all(|u| !u.is_admin));
}

#[tokio::test]
async fn ensure_keeps_a_client_as_configured() {
    let srv = start().await;
    let db = &srv.state.db;
    let a = format!("{ADMIN_TOKEN_PREFIX}{}", "a".repeat(40));
    let b = format!("{ADMIN_TOKEN_PREFIX}{}", "b".repeat(40));
    let works = |t: String| {
        let url = format!("{}/admin/users", srv.base);
        async move {
            reqwest::Client::new()
                .get(url)
                .bearer_auth(t)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };
    let r = admin_clients::ensure(
        db,
        "billing",
        &[AdminScope::Accounts],
        std::slice::from_ref(&a),
    )
    .await
    .unwrap();
    assert_eq!(
        r,
        admin_clients::Ensured {
            created: true,
            added: 1,
            removed: 0
        }
    );
    assert_eq!(works(a.clone()).await, 200);
    // Again: nothing changes.
    let r = admin_clients::ensure(
        db,
        "billing",
        &[AdminScope::Accounts],
        std::slice::from_ref(&a),
    )
    .await
    .unwrap();
    assert_eq!(r, admin_clients::Ensured::default());
    // Rotating: both for a while, then only the new one.
    let r = admin_clients::ensure(
        db,
        "billing",
        &[AdminScope::Accounts],
        &[a.clone(), b.clone()],
    )
    .await
    .unwrap();
    assert_eq!(r.added, 1);
    let r = admin_clients::ensure(
        db,
        "billing",
        &[AdminScope::Settings],
        std::slice::from_ref(&b),
    )
    .await
    .unwrap();
    assert_eq!(r.removed, 1);
    assert_eq!(works(a.clone()).await, 401);
    assert_eq!(works(b.clone()).await, 200);
    let list = notekpr_server::repo::admin_clients::list(db).await.unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].managed);
    assert_eq!(list[0].scopes, vec![AdminScope::Read, AdminScope::Settings]);
    // What isn't a token, or is another client's, is refused.
    assert!(
        admin_clients::ensure(db, "billing", &[], &["short".into()])
            .await
            .is_err()
    );
    assert!(
        admin_clients::ensure(db, "billing", &[], &[])
            .await
            .is_err()
    );
    assert!(
        admin_clients::ensure(db, "other", &[], std::slice::from_ref(&b))
            .await
            .is_err()
    );
    // `create` prints a token of its own.
    let made = admin_clients::create(db, "cli", &[AdminScope::Read], None)
        .await
        .unwrap();
    assert_eq!(works(made.token).await, 200);
    assert!(!made.client.managed);
}

#[tokio::test]
async fn limits_stop_new_accounts_vaults_and_agents() {
    let srv = start_with_operator().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let mut bob = new_user(&srv.base, "bob").await;

    // Accounts: disabled ones don't count, enabling one counts like a new one.
    settings(&srv, serde_json::json!({ "max_users": 2 })).await;
    let r = reqwest::Client::new()
        .post(format!("{}/auth/dev-login", srv.base))
        .json(&DevLoginRequest {
            username: "carol".into(),
            display_name: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(error_code(r).await, (403, ERROR_LIMIT_REACHED.to_string()));
    // Those who have an account still sign in.
    dev_login(&srv.base, "bob").await;
    let _: serde_json::Value = alice
        .api
        .call(
            Method::PATCH,
            &format!("/admin/users/{}", bob.id),
            Some(&AdminUpdateUser {
                disabled: Some(true),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    let carol = new_user(&srv.base, "carol").await;
    let enable = AdminUpdateUser {
        disabled: Some(false),
        ..Default::default()
    };
    let err = alice
        .api
        .call::<_, serde_json::Value>(
            Method::PATCH,
            &format!("/admin/users/{}", bob.id),
            Some(&enable),
        )
        .await
        .unwrap_err();
    assert_eq!(err.0, 403);
    settings(&srv, serde_json::json!({ "max_users": "unlimited" })).await;
    let _: serde_json::Value = alice
        .api
        .call(
            Method::PATCH,
            &format!("/admin/users/{}", bob.id),
            Some(&enable),
        )
        .await
        .unwrap();
    // Disabling ended bob's sessions.
    bob.api = dev_login(&srv.base, "bob").await.0;

    // Vaults a person owns, by default and by their own allowance.
    settings(&srv, serde_json::json!({ "vaults_per_user": 1 })).await;
    let mine = alice.create_vault("One").await;
    assert_eq!(try_create_vault(&alice).await, 403);
    let r = operator(&srv, Method::PATCH, &format!("/admin/users/{}", alice.id))
        .json(&serde_json::json!({ "vault_allowance": 2 }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    alice.create_vault("Two").await;
    assert_eq!(try_create_vault(&alice).await, 403);
    // Vaults shared with someone don't count for them.
    let mut carol = carol;
    let carols = carol.create_vault("Carol's").await;
    carol.share(&carols, &alice.id, Role::Editor).await.unwrap();
    assert_eq!(try_create_vault(&alice).await, 403);
    let r = operator(&srv, Method::PATCH, &format!("/admin/users/{}", alice.id))
        .json(&serde_json::json!({ "vault_allowance": "unlimited" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    alice.create_vault("Three").await;
    // Below a lowered limit nothing is taken away.
    settings(&srv, serde_json::json!({ "vaults_per_user": 0 })).await;
    let r = operator(&srv, Method::PATCH, &format!("/admin/users/{}", alice.id))
        .json(&serde_json::json!({ "vault_allowance": "default" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let note = NoteDoc::new();
    let u = note.set_text("still mine").unwrap();
    alice
        .push(&mine, vec![alice.encrypt_update(&mine, "n", &u)])
        .await
        .unwrap();
    let err = alice
        .api
        .call::<_, serde_json::Value>(
            Method::POST,
            "/vaults",
            Some(&CreateVaultRequest {
                id: random_id(),
                encrypted_meta: vec![1; 64],
                key_envelope: KeyEnvelope {
                    user_id: alice.id.clone(),
                    epoch: 1,
                    envelope: vec![1; 64],
                    signature: None,
                },
                log_entry: None,
                kind: Default::default(),
            }),
        )
        .await
        .unwrap_err();
    assert!(err.1.contains("can't create vaults"), "{}", err.1);

    // The server's total.
    settings(
        &srv,
        serde_json::json!({ "vaults_per_user": "unlimited", "max_vaults": 4 }),
    )
    .await;
    assert_eq!(try_create_vault(&bob).await, 403);

    // Agents: the person's allowance, then the server's total.
    let r = operator(&srv, Method::PATCH, &format!("/admin/users/{}", bob.id))
        .json(&serde_json::json!({ "agent_allowance": 0 }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(create_agent(&bob).await.unwrap_err().0, 403);
    settings(&srv, serde_json::json!({ "agents_per_user": 1 })).await;
    create_agent(&alice).await.unwrap();
    assert_eq!(create_agent(&alice).await.unwrap_err().0, 403);
    settings(
        &srv,
        serde_json::json!({ "agents_per_user": "unlimited", "max_agents": 1 }),
    )
    .await;
    assert_eq!(create_agent(&carol).await.unwrap_err().0, 403);
    // Agents have no allowance of their own.
    let agents: Vec<AdminUser> = alice.api.get("/admin/users").await;
    let agent = agents
        .iter()
        .find(|u| u.kind == PrincipalKind::Agent)
        .unwrap();
    assert_eq!(
        alice
            .api
            .status(
                Method::PATCH,
                &format!("/admin/users/{}", agent.id),
                Some(&serde_json::json!({ "vault_allowance": 1 }))
            )
            .await,
        400
    );
    let me = agents.iter().find(|u| u.id == alice.id).unwrap();
    assert_eq!(me.agent_count, 1);
    assert_eq!(me.owned_vault_count, 3);
}

#[tokio::test]
async fn read_only_mode_keeps_everything_but_refuses_changes() {
    let srv = start_with_operator().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let v = alice.create_vault("Notes").await;
    let token = alice.api.token.clone().unwrap();
    let mut ws = Ws::connect(&srv.ws, &token).await;
    assert_eq!(ws.read_only, None);
    ws.send(&ClientMessage::Subscribe {
        vault_id: v.clone(),
        since: Some(0),
        object_id: None,
    })
    .await;
    ws.until(|m| matches!(m, ServerMessage::Synced { .. }).then_some(()))
        .await;

    let s = settings(&srv, serde_json::json!({ "read_only": true })).await;
    assert_eq!(s.read_only, Some(ReadOnly::Manual));
    // Live connections are told at once.
    let mode = ws
        .until(|m| match m {
            ServerMessage::Mode { read_only } => Some(*read_only),
            _ => None,
        })
        .await;
    assert_eq!(mode, Some(ReadOnly::Manual));
    let info: ServerInfo = Api::new(&srv.base).get("/info").await;
    assert_eq!(info.read_only, Some(ReadOnly::Manual));

    // Changes are refused, over REST and the WebSocket.
    let note = NoteDoc::new();
    let u = note.set_text("written while read-only").unwrap();
    let update = alice.encrypt_update(&v, "n", &u);
    let r = alice
        .api
        .req(Method::POST, &format!("/vaults/{v}/updates"))
        .json(&PushRequest {
            updates: vec![update.clone()],
        })
        .send()
        .await
        .unwrap();
    assert_eq!(error_code(r).await, (503, ERROR_READ_ONLY.to_string()));
    ws.send(&ClientMessage::Push {
        vault_id: v.clone(),
        updates: vec![update.clone()],
    })
    .await;
    let code = ws
        .until(|m| match m {
            ServerMessage::Error { code, .. } => Some(code.clone()),
            _ => None,
        })
        .await;
    assert_eq!(code, ERROR_READ_ONLY);
    assert_eq!(try_create_vault(&alice).await, 503);
    assert_eq!(create_agent(&alice).await.unwrap_err().0, 503);
    // Administration keeps working (it is how read-only mode ends).
    let r = operator(&srv, Method::POST, "/admin/clients")
        .json(&CreateAdminClient {
            name: "during-maintenance".into(),
            scopes: vec![],
        })
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 201);
    assert_eq!(
        alice
            .api
            .status::<()>(Method::DELETE, &format!("/vaults/{v}"), None)
            .await,
        503
    );
    // No new accounts; reading, signing in and out, revoking still work.
    let r = reqwest::Client::new()
        .post(format!("{}/auth/dev-login", srv.base))
        .json(&DevLoginRequest {
            username: "dave".into(),
            display_name: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(error_code(r).await, (503, ERROR_READ_ONLY.to_string()));
    let (second, _) = dev_login(&srv.base, "alice").await;
    let s: MeResponse = second.get("/me").await;
    let _: ChangesResponse = alice.api.get(&format!("/vaults/{v}/changes?since=0")).await;
    assert_eq!(
        alice
            .api
            .status::<()>(
                Method::DELETE,
                &format!("/me/sessions/{}", s.session.id),
                None
            )
            .await,
        204
    );
    assert_eq!(second.status::<()>(Method::GET, "/me", None).await, 401);
    // A connection made now hears it first thing.
    let late = Ws::connect(&srv.ws, &token).await;
    assert_eq!(late.read_only, Some(ReadOnly::Manual));

    // Writable again: what was kept goes through.
    let s = settings(&srv, serde_json::json!({ "read_only": false })).await;
    assert_eq!(s.read_only, None);
    let mode = ws
        .until(|m| match m {
            ServerMessage::Mode { read_only } => Some(*read_only),
            _ => None,
        })
        .await;
    assert_eq!(mode, None);
    alice.push(&v, vec![update]).await.unwrap();
    let (_, notes) = alice.read_vault(&v).await;
    assert_eq!(notes["n"].text(), "written while read-only");
}

#[tokio::test]
async fn a_full_database_fills_every_vault_until_there_is_room() {
    let srv = start_with_operator().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let v = alice.create_vault("Notes").await;
    let work = alice.create_vault("Work").await;
    let text = |t: &str| NoteDoc::new().set_text(t).unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "old", &text("old"))])
        .await
        .unwrap();
    let mut ws = subscribed(&srv, &alice, &v, None).await;
    let server_full = |m: &ServerMessage| match m {
        ServerMessage::VaultFull {
            vault_id,
            full,
            server,
        } if *vault_id == v => Some((*full, *server)),
        _ => None,
    };

    // Every vault is full, because the server is: its subscribers hear so.
    let s = settings(&srv, serde_json::json!({ "max_storage_mb": 0 })).await;
    assert!(s.storage_full);
    assert_eq!(s.read_only, None);
    assert_eq!(ws.until(server_full).await, (true, true));
    for vault in [&v, &work] {
        let u = alice.encrypt_update(vault, "n", &text("no room"));
        let err = alice.push(vault, vec![u]).await.unwrap_err();
        assert_eq!(err.0, 507, "{}", err.1);
    }
    // All but new content goes on: deleting for good (which makes room),
    // keys.
    assert_eq!(
        alice
            .api
            .status::<()>(Method::DELETE, &format!("/vaults/{v}/objects/old"), None)
            .await,
        204
    );
    alice.rotate(&v).await;

    // It is separate from read-only mode: switching that on and off leaves
    // the server full.
    let s = settings(&srv, serde_json::json!({ "read_only": true })).await;
    assert_eq!(s.read_only, Some(ReadOnly::Manual));
    let s = settings(&srv, serde_json::json!({ "read_only": false })).await;
    assert_eq!(s.read_only, None);
    assert!(s.storage_full);

    let s = settings(&srv, serde_json::json!({ "max_storage_mb": 1024 })).await;
    assert!(!s.storage_full);
    assert_eq!(ws.until(server_full).await, (false, false));
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n", &text("room again"))])
        .await
        .unwrap();
    // Settings outlive a restart (they are in the database).
    let stored = notekpr_server::repo::settings::load(&srv.state.db)
        .await
        .unwrap();
    assert_eq!(stored.max_storage_mb, Limit::Max(1024));
}

/// A quick note made by pushing its first content (no key of its own).
fn quick_note(u: &User, v: &str, id: &str) -> PushUpdate {
    let text = NoteDoc::new().set_text(id).unwrap();
    u.encrypt_update(v, id, &text)
}

/// Quick notes hold as many notes as the instance's `quick_notes_per_user`
/// or the person's own allowance say; both through the admin API. Lowering
/// takes nothing away, and quick notes count as no vault.
#[tokio::test]
async fn quick_notes_hold_as_many_notes_as_allowed() {
    use notekpr_core::engine;
    let srv = start_with_operator().await;
    let mut alice = new_user(&srv.base, "alice").await;
    settings(
        &srv,
        serde_json::json!({ "quick_notes_per_user": 2, "vaults_per_user": 0 }),
    )
    .await;
    // No vault may be made, quick notes still can.
    assert_eq!(try_create_vault(&alice).await, 403);
    let q = alice.create_quick_notes().await.unwrap();
    assert_eq!(q.note_limit, Limit::Max(2));
    let q = q.id;
    let mut ws = Ws::connect(&srv.ws, alice.api.token.as_ref().unwrap()).await;

    // Two notes fit; the third waits. Edits to the two and the index don't.
    let r = alice
        .push(
            &q,
            vec![
                quick_note(&alice, &q, "n1"),
                quick_note(&alice, &q, "n2"),
                quick_note(&alice, &q, "n3"),
                quick_note(&alice, &q, "n1"),
                alice.encrypt_update(&q, INDEX_OBJECT_ID, b"\0"),
            ],
        )
        .await
        .unwrap();
    assert_eq!(r.results.len(), 4);
    assert_eq!(r.rejected.len(), 1);
    assert_eq!(r.rejected[0].object_id, "n3");
    assert_eq!(r.rejected[0].code, REJECT_QUICK_NOTES_FULL);
    // A new note's own key is refused the same way.
    let (_, key) = engine::new_document_key(alice.ring(&q), "n4", "Later").unwrap();
    let err = alice
        .api
        .call::<_, ()>(
            Method::PUT,
            &format!("/vaults/{q}/objects/n4/key"),
            Some(&key),
        )
        .await
        .unwrap_err();
    assert_eq!(err.0, 403);
    assert!(err.1.contains(ERROR_LIMIT_REACHED), "{}", err.1);

    // Deleting one for good makes room.
    let purge = format!("/vaults/{q}/objects/n1");
    assert_eq!(
        alice.api.status::<()>(Method::DELETE, &purge, None).await,
        204
    );
    let r = alice
        .push(&q, vec![quick_note(&alice, &q, "n3")])
        .await
        .unwrap();
    assert!(r.rejected.is_empty());

    // A person's own allowance, told to their apps.
    let path = format!("/admin/users/{}", alice.id);
    let r = operator(&srv, Method::PATCH, &path)
        .json(&serde_json::json!({ "quick_note_allowance": 3 }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    ws.until(|m| match m {
        ServerMessage::UserEvent {
            event: UserEventKind::VaultsChanged,
            ..
        } => Some(()),
        _ => None,
    })
    .await;
    let info: VaultInfo = alice.api.get(&format!("/vaults/{q}")).await;
    assert_eq!(info.note_limit, Limit::Max(3));
    let r = alice
        .push(&q, vec![quick_note(&alice, &q, "n5")])
        .await
        .unwrap();
    assert!(r.rejected.is_empty());
    let users: Vec<AdminUser> = operator(&srv, Method::GET, "/admin/users")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let a = users.iter().find(|u| u.id == alice.id).unwrap();
    assert_eq!(a.quick_note_allowance, Allowance::Max(3));
    assert_eq!(a.quick_note_count, 3);
    assert_eq!((a.vault_count, a.owned_vault_count), (0, 0));
    let stats: AdminStats = operator(&srv, Method::GET, "/admin/stats")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stats.vaults, 0, "quick notes are no vault");

    // Back to the instance's default, lowered: nothing is taken away.
    let r = operator(&srv, Method::PATCH, &path)
        .json(&serde_json::json!({ "quick_note_allowance": "default" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let s = settings(&srv, serde_json::json!({ "quick_notes_per_user": 1 })).await;
    assert_eq!(s.settings.quick_notes_per_user, Limit::Max(1));
    let info: VaultInfo = alice.api.get(&format!("/vaults/{q}")).await;
    assert_eq!(info.note_limit, Limit::Max(1));
    let r = alice
        .push(
            &q,
            vec![quick_note(&alice, &q, "n2"), quick_note(&alice, &q, "n6")],
        )
        .await
        .unwrap();
    assert_eq!(r.results.len(), 1, "the old one is still edited");
    assert_eq!(r.rejected[0].object_id, "n6");
    // Unlimited for everyone.
    settings(
        &srv,
        serde_json::json!({ "quick_notes_per_user": "unlimited" }),
    )
    .await;
    let r = alice
        .push(&q, vec![quick_note(&alice, &q, "n6")])
        .await
        .unwrap();
    assert!(r.rejected.is_empty());
    // Agents have no allowance.
    let agent = create_agent(&alice).await.unwrap();
    let r = operator(
        &srv,
        Method::PATCH,
        &format!("/admin/users/{}", agent.agent.id),
    )
    .json(&serde_json::json!({ "quick_note_allowance": 1 }))
    .send()
    .await
    .unwrap();
    assert_eq!(r.status().as_u16(), 400);
}

/// Apps older than the oldest version the server accepts (its build's, or
/// higher as administrators set it) are told to update, over REST and the
/// WebSocket. Development builds, and requests that don't say which app
/// they are, never are.
#[tokio::test]
async fn apps_older_than_the_server_accepts_are_told_to_update() {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let srv = start_with_operator().await;
    let alice = new_user(&srv.base, "alice").await;
    let token = alice.api.token.clone().unwrap();
    let s: AdminSettings = operator(&srv, Method::GET, "/admin/settings")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(s.build_oldest_app_version, "0.0.0");
    let get = |path: &str| {
        reqwest::Client::new()
            .get(format!("{}{path}", srv.base))
            .bearer_auth(&token)
    };
    assert_eq!(
        status(get("/vaults").header(APP_HEADER, "cli/0.1.0")).await,
        200
    );

    let r = operator(&srv, Method::PATCH, "/admin/settings")
        .json(&serde_json::json!({ "oldest_app_version": "soon" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);
    let s = settings(&srv, serde_json::json!({ "oldest_app_version": "0.6.0" })).await;
    assert_eq!(s.settings.oldest_app_version, "0.6.0");

    let r = get("/vaults")
        .header(APP_HEADER, "cli/0.5.9")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 426);
    let body: ApiError = r.json().await.unwrap();
    assert_eq!(body.error, ERROR_UPDATE_REQUIRED);
    assert_eq!(body.oldest_version.as_deref(), Some("0.6.0"));
    for fine in ["cli/0.6.0", "web/0.7.1-2-gabc1234", "cli/dev"] {
        assert_eq!(
            status(get("/vaults").header(APP_HEADER, fine)).await,
            200,
            "{fine}"
        );
    }
    assert_eq!(status(get("/vaults")).await, 200);
    // The server's version is there for every app to read.
    assert_eq!(
        status(get("/info").header(APP_HEADER, "cli/0.1.0")).await,
        200
    );
    // Signing in is refused too.
    let r = reqwest::Client::new()
        .post(format!("{}/auth/dev-login", srv.base))
        .header(APP_HEADER, "web/0.1.0")
        .json(&serde_json::json!({ "name": "bob" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 426);

    let (mut ws, _) = tokio_tungstenite::connect_async(&srv.ws).await.unwrap();
    let auth = ClientMessage::Auth {
        token: token.clone(),
        app: Some("cli/0.5.0".into()),
    };
    ws.send(Message::Text(serde_json::to_string(&auth).unwrap().into()))
        .await
        .unwrap();
    let first = ws.next().await.unwrap().unwrap();
    let m: ServerMessage = serde_json::from_str(first.to_text().unwrap()).unwrap();
    assert!(
        matches!(&m, ServerMessage::Error { code, .. } if code == ERROR_UPDATE_REQUIRED),
        "{m:?}"
    );

    // Back to only what the build needs.
    let s = settings(&srv, serde_json::json!({ "oldest_app_version": "" })).await;
    assert_eq!(s.settings.oldest_app_version, "");
    assert_eq!(
        status(get("/vaults").header(APP_HEADER, "cli/0.5.9")).await,
        200
    );
}

/// Upload `size` bytes (before encryption) as an attachment.
async fn put_attachment(u: &User, v: &str, id: &str, size: usize) -> u16 {
    let blob = u
        .ring(v)
        .encrypt(
            notekpr_core::crypto::Purpose::Attachment,
            id,
            &vec![7; size],
        )
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

async fn vault_info(u: &User, v: &str) -> VaultInfo {
    u.api.get(&format!("/vaults/{v}")).await
}

/// What a `vault_full` message says of vault `v`.
fn vault_full(m: &ServerMessage, v: &str) -> Option<bool> {
    match m {
        ServerMessage::VaultFull { vault_id, full, .. } if vault_id == v => Some(*full),
        _ => None,
    }
}

async fn subscribed(srv: &TestServer, u: &User, v: &str, object_id: Option<&str>) -> Ws {
    let mut ws = Ws::connect(&srv.ws, u.api.token.as_deref().unwrap()).await;
    ws.send(&ClientMessage::Subscribe {
        vault_id: v.into(),
        since: None,
        object_id: object_id.map(Into::into),
    })
    .await;
    ws.until(|m| matches!(m, ServerMessage::Synced { .. }).then_some(()))
        .await;
    ws
}

#[tokio::test]
async fn a_full_vault_takes_no_new_content_until_it_has_room() {
    let srv = start_with_operator().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let bob = new_user(&srv.base, "bob").await;
    let v = alice.create_vault("Photos").await;
    let work = alice.create_vault("Work").await;
    let text = |t: &str| NoteDoc::new().set_text(t).unwrap();
    share_note(&alice, &v, "shared", &bob).await;
    let plain = NoteDoc::new();
    let first = alice.encrypt_update(&v, "plain", &plain.set_text("plain").unwrap());
    let head = alice.push(&v, vec![first.clone()]).await.unwrap().head_seq;
    // Deleted for good, which can still be undone.
    alice
        .push(&v, vec![alice.encrypt_update(&v, "old", &text("old"))])
        .await
        .unwrap();
    let object = |o: &str| format!("/vaults/{v}/objects/{o}");
    assert_eq!(
        alice
            .api
            .status::<()>(Method::DELETE, &object("old"), None)
            .await,
        204
    );

    let s = settings(&srv, serde_json::json!({ "storage_per_vault_mb": 1 })).await;
    assert_eq!(s.settings.storage_per_vault_mb, Limit::Max(1));
    let info = vault_info(&alice, &v).await;
    assert_eq!(info.storage_limit_mb, Limit::Max(1));
    assert!(info.storage_bytes > 0 && !info.is_full());
    let mut wa = subscribed(&srv, &alice, &v, None).await;
    let mut wb = subscribed(&srv, &bob, &v, Some("shared")).await;

    // A file fills it: everyone writing to it hears so, also someone a
    // single note of it is shared with.
    assert_eq!(put_attachment(&alice, &v, "photo", MIB as usize).await, 201);
    assert!(wa.until(|m| vault_full(m, &v)).await);
    assert!(wb.until(|m| vault_full(m, &v)).await);
    let info = vault_info(&alice, &v).await;
    assert!(info.is_full() && info.storage_bytes > MIB);

    // Nothing new goes in: edits (over REST and the WebSocket), history,
    // files, undoing a deletion.
    let edit = alice.encrypt_update(&v, "plain", &plain.set_text("no room").unwrap());
    let r = alice
        .api
        .req(Method::POST, &format!("/vaults/{v}/updates"))
        .json(&PushRequest {
            updates: vec![edit.clone()],
        })
        .send()
        .await
        .unwrap();
    assert_eq!(error_code(r).await, (507, ERROR_VAULT_FULL.to_string()));
    wa.send(&ClientMessage::Push {
        vault_id: v.clone(),
        updates: vec![edit.clone()],
    })
    .await;
    let refused = wa
        .until(|m| match m {
            ServerMessage::Error { code, vault_id, .. } => Some((code.clone(), vault_id.clone())),
            _ => None,
        })
        .await;
    assert_eq!(refused, (ERROR_VAULT_FULL.to_string(), Some(v.clone())));
    let snapshot = CreateSnapshotRequest {
        snapshot_id: random_id(),
        covers_seq: head,
        payload: alice
            .ring(&v)
            .encrypt(
                notekpr_core::crypto::Purpose::Snapshot,
                "plain",
                &plain.encode_state(),
            )
            .unwrap(),
        signature: None,
        carries: false,
    };
    let snapshots = format!("{}/snapshots", object("plain"));
    assert_eq!(
        alice
            .api
            .status(Method::POST, &snapshots, Some(&snapshot))
            .await,
        507
    );
    assert_eq!(put_attachment(&alice, &v, "more", 10).await, 507);
    assert_eq!(
        alice
            .api
            .status::<()>(Method::POST, &format!("{}/restore", object("old")), None)
            .await,
        507
    );
    // A retry of what it already has is answered as before.
    let again = alice.push(&v, vec![first]).await.unwrap();
    assert_eq!(again.results.len(), 1);
    // Other vaults go on, and so does all but new content (keys here).
    alice
        .push(&work, vec![alice.encrypt_update(&work, "n", &text("work"))])
        .await
        .unwrap();
    alice.rotate(&v).await;
    // A connection that subscribes now hears it before `synced`.
    let mut late = Ws::connect(&srv.ws, alice.api.token.as_deref().unwrap()).await;
    late.send(&ClientMessage::Subscribe {
        vault_id: v.clone(),
        since: None,
        object_id: None,
    })
    .await;
    assert!(late.until(|m| vault_full(m, &v)).await);
    late.until(|m| matches!(m, ServerMessage::Synced { .. }).then_some(()))
        .await;

    // Deleting the file for good makes room at once (it is kept for the
    // undo period, but no longer counts), and the edits go in.
    assert_eq!(
        alice
            .api
            .status::<()>(
                Method::DELETE,
                &format!("/vaults/{v}/attachments/photo"),
                None
            )
            .await,
        204
    );
    assert!(!wa.until(|m| vault_full(m, &v)).await);
    assert!(!wb.until(|m| vault_full(m, &v)).await);
    assert!(!vault_info(&alice, &v).await.is_full());
    let edit = alice.encrypt_update(&v, "plain", &plain.set_text("room again").unwrap());
    alice.push(&v, vec![edit]).await.unwrap();
    // Undoing that fills it again.
    assert_eq!(
        alice
            .api
            .status::<()>(
                Method::POST,
                &format!("/vaults/{v}/attachments/photo/restore"),
                None
            )
            .await,
        204
    );
    assert!(wa.until(|m| vault_full(m, &v)).await);
    // So does a lower limit for every vault, and a higher one makes room.
    let s = settings(&srv, serde_json::json!({ "storage_per_vault_mb": 2 })).await;
    assert_eq!(s.settings.storage_per_vault_mb, Limit::Max(2));
    assert!(!wa.until(|m| vault_full(m, &v)).await);
    settings(&srv, serde_json::json!({ "storage_per_vault_mb": 1 })).await;
    assert!(wa.until(|m| vault_full(m, &v)).await);
    settings(
        &srv,
        serde_json::json!({ "storage_per_vault_mb": "unlimited" }),
    )
    .await;
    assert!(!wa.until(|m| vault_full(m, &v)).await);
}

#[tokio::test]
async fn administrators_see_vaults_by_size_and_give_one_more_room() {
    let srv = start_with_operator().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let mut bob = new_user(&srv.base, "bob").await;
    let diary = alice.create_vault("Diary").await;
    let photos = alice.create_vault("Photos").await;
    let recipes = bob.create_vault("Recipes").await;
    assert_eq!(put_attachment(&alice, &photos, "a", 50_000).await, 201);
    let note = NoteDoc::new().set_text("dear diary").unwrap();
    alice
        .push(&diary, vec![alice.encrypt_update(&diary, "n", &note)])
        .await
        .unwrap();
    let list = |query: String| {
        let r = operator(&srv, Method::GET, &format!("/admin/vaults{query}"));
        async move {
            r.send()
                .await
                .unwrap()
                .json::<Vec<AdminVault>>()
                .await
                .unwrap()
        }
    };

    // The largest first, with its owner and size: nothing of what's in it.
    let all = list(String::new()).await;
    assert_eq!(
        all.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
        [photos.as_str(), diary.as_str(), recipes.as_str()]
    );
    let big = &all[0];
    assert_eq!(
        (big.owner_id.as_str(), big.owner_name.as_str()),
        (alice.id.as_str(), "Alice")
    );
    assert_eq!(
        big.storage_bytes,
        vault_info(&alice, &photos).await.storage_bytes
    );
    assert!(big.storage_bytes > 50_000);
    assert_eq!(big.storage_allowance, Allowance::Default);
    let raw: Vec<serde_json::Value> = operator(&srv, Method::GET, "/admin/vaults")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut fields: Vec<&str> = raw[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort();
    assert_eq!(
        fields,
        [
            "created_at",
            "drawing_allowance",
            "drawing_count",
            "id",
            "owner_id",
            "owner_name",
            "storage_allowance",
            "storage_bytes"
        ]
    );
    // Found by the id people read in its settings, or by owner.
    let one = list(format!("?id={diary}")).await;
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].id, diary);
    let bobs = list(format!("?owner={}", bob.id)).await;
    assert_eq!(bobs.len(), 1);
    assert_eq!(bobs[0].id, recipes);
    assert!(list("?id=nothing-like-it".into()).await.is_empty());
    // A person's total is what their vaults take.
    let users: Vec<AdminUser> = operator(&srv, Method::GET, "/admin/users")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let theirs = users.iter().find(|u| u.id == alice.id).unwrap();
    assert_eq!(
        theirs.storage_bytes,
        all[0].storage_bytes + all[1].storage_bytes
    );

    // Its own limit: members see it, other vaults keep the instance's.
    settings(&srv, serde_json::json!({ "storage_per_vault_mb": 100 })).await;
    let set = |vault: &str, a: serde_json::Value| {
        let r = operator(&srv, Method::PATCH, &format!("/admin/vaults/{vault}"))
            .json(&serde_json::json!({ "storage_allowance": a }));
        async move { r.send().await.unwrap().status().as_u16() }
    };
    let mut ws = subscribed(&srv, &alice, &photos, None).await;
    assert_eq!(set(&photos, serde_json::json!(0)).await, 200);
    assert!(ws.until(|m| vault_full(m, &photos)).await);
    let info = vault_info(&alice, &photos).await;
    assert_eq!(info.storage_limit_mb, Limit::Max(0));
    assert!(info.is_full());
    assert_eq!(
        vault_info(&alice, &diary).await.storage_limit_mb,
        Limit::Max(100)
    );
    assert_eq!(
        list(format!("?id={photos}")).await[0].storage_allowance,
        Allowance::Max(0)
    );
    assert_eq!(set(&photos, serde_json::json!("unlimited")).await, 200);
    assert!(!ws.until(|m| vault_full(m, &photos)).await);
    assert_eq!(
        vault_info(&alice, &photos).await.storage_limit_mb,
        Limit::Unlimited
    );
    assert_eq!(set(&photos, serde_json::json!("default")).await, 200);
    assert_eq!(
        vault_info(&alice, &photos).await.storage_limit_mb,
        Limit::Max(100)
    );
    assert_eq!(set("nothing-like-it", serde_json::json!(5)).await, 404);
    // While the server is read-only too: it's administration.
    settings(&srv, serde_json::json!({ "read_only": true })).await;
    assert_eq!(set(&photos, serde_json::json!(200)).await, 200);
    settings(&srv, serde_json::json!({ "read_only": false })).await;

    // Reading takes `read`, limits `accounts`; people need the admin role.
    let reader: CreatedAdminToken = operator(&srv, Method::POST, "/admin/clients")
        .json(&CreateAdminClient {
            name: "reader".into(),
            scopes: vec![],
        })
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        status(as_client(&srv, &reader.token, Method::GET, "/admin/vaults")).await,
        200
    );
    let patch = as_client(
        &srv,
        &reader.token,
        Method::PATCH,
        &format!("/admin/vaults/{photos}"),
    )
    .json(&serde_json::json!({ "storage_allowance": 1 }));
    assert_eq!(status(patch).await, 403);
    assert_eq!(
        bob.api
            .status::<()>(Method::GET, "/admin/vaults", None)
            .await,
        403
    );
}

/// Someone's quick notes have a storage limit of their own, like how many
/// they may keep: the instance's for quick notes, else the person's — never
/// the limit per vault, and not in the vaults administrators list.
#[tokio::test]
async fn quick_notes_have_a_storage_limit_of_their_own_per_person() {
    let srv = start_with_operator().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let q = alice.create_quick_notes().await.unwrap().id;
    alice
        .push(&q, vec![quick_note(&alice, &q, "n1")])
        .await
        .unwrap();
    assert!(vault_info(&alice, &q).await.storage_bytes > 0);
    let mut ws = subscribed(&srv, &alice, &q, None).await;

    // The limit per vault isn't theirs.
    settings(&srv, serde_json::json!({ "storage_per_vault_mb": 0 })).await;
    let info = vault_info(&alice, &q).await;
    assert_eq!(info.storage_limit_mb, Limit::Unlimited);
    assert!(!info.is_full());

    // The instance's for quick notes is: they turn full, and hear so.
    let s = settings(&srv, serde_json::json!({ "quick_notes_storage_mb": 0 })).await;
    assert_eq!(s.settings.quick_notes_storage_mb, Limit::Max(0));
    assert!(ws.until(|m| vault_full(m, &q)).await);
    assert_eq!(vault_info(&alice, &q).await.storage_limit_mb, Limit::Max(0));
    let refused = alice
        .push(&q, vec![quick_note(&alice, &q, "n2")])
        .await
        .unwrap_err();
    assert_eq!(refused.0, 507);

    // Administrators see them with the person, not among the vaults.
    let vaults: Vec<AdminVault> = operator(&srv, Method::GET, "/admin/vaults")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(vaults.iter().all(|v| v.id != q));
    let patch_vault = operator(&srv, Method::PATCH, &format!("/admin/vaults/{q}"))
        .json(&serde_json::json!({ "storage_allowance": 5 }));
    assert_eq!(status(patch_vault).await, 404);
    let person = || async {
        let users: Vec<AdminUser> = operator(&srv, Method::GET, "/admin/users")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        users.into_iter().find(|u| u.id == alice.id).unwrap()
    };
    let p = person().await;
    assert_eq!(
        p.quick_notes_storage_bytes,
        vault_info(&alice, &q).await.storage_bytes
    );
    assert_eq!(
        p.storage_bytes, 0,
        "their vaults' size leaves quick notes apart"
    );
    assert_eq!(p.quick_notes_storage_allowance, Allowance::Default);

    // The person's own allowance gives them room again, at once.
    let set = |a: serde_json::Value| {
        let r = operator(&srv, Method::PATCH, &format!("/admin/users/{}", alice.id))
            .json(&serde_json::json!({ "quick_notes_storage_allowance": a }));
        async move { status(r).await }
    };
    assert_eq!(set(serde_json::json!(5)).await, 200);
    assert!(!ws.until(|m| vault_full(m, &q)).await);
    assert_eq!(vault_info(&alice, &q).await.storage_limit_mb, Limit::Max(5));
    assert_eq!(
        person().await.quick_notes_storage_allowance,
        Allowance::Max(5)
    );
    alice
        .push(&q, vec![quick_note(&alice, &q, "n2")])
        .await
        .unwrap();
    // Back to the instance's: full again.
    assert_eq!(set(serde_json::json!("default")).await, 200);
    assert!(ws.until(|m| vault_full(m, &q)).await);
}

/// How many drawings a vault may hold is the instance's
/// `drawings_per_vault` (unlimited by default) or the vault's own allowance,
/// whoever draws in it. Members' apps learn when it changes; quick notes
/// have none of their own.
#[tokio::test]
async fn administrators_set_how_many_drawings_a_vault_may_hold() {
    let srv = start_with_operator().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let mut bob = new_user(&srv.base, "bob").await;
    let v = alice.create_vault("Garden").await;
    alice.share(&v, &bob.id, Role::Editor).await.unwrap();
    bob.load_keys(&v).await.unwrap();
    let other = alice.create_vault("Other").await;
    let mut ws = subscribed(&srv, &bob, &v, None).await;
    let s = settings(&srv, serde_json::json!({ "drawings_per_vault": 1 })).await;
    assert_eq!(s.settings.drawings_per_vault, Limit::Max(1));
    let changed = |m: &ServerMessage| match m {
        ServerMessage::UserEvent {
            event: UserEventKind::VaultsChanged,
        } => Some(()),
        _ => None,
    };
    // Members' apps reload their vaults' limits.
    ws.until(changed).await;
    assert_eq!(vault_info(&bob, &v).await.drawing_limit, Limit::Max(1));
    alice.make_drawing(&v, "d1").await;
    let (_, key) = bob.new_key(&v, "d2", ObjectKind::Drawing);
    assert_eq!(bob.put_key(&v, "d2", &key).await.unwrap_err().0, 403);

    // The vault's own allowance, told to its members' apps; other vaults
    // keep the instance's.
    let path = format!("/admin/vaults/{v}");
    let patch = |body: serde_json::Value| {
        let r = operator(&srv, Method::PATCH, &path).json(&body);
        async move { status(r).await }
    };
    assert_eq!(
        patch(serde_json::json!({ "drawing_allowance": 2 })).await,
        200
    );
    ws.until(changed).await;
    assert_eq!(vault_info(&bob, &v).await.drawing_limit, Limit::Max(2));
    assert_eq!(
        vault_info(&alice, &other).await.drawing_limit,
        Limit::Max(1)
    );
    bob.put_key(&v, "d2", &key).await.unwrap();
    let vaults: Vec<AdminVault> = operator(&srv, Method::GET, "/admin/vaults")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let garden = vaults.iter().find(|a| a.id == v).unwrap();
    assert_eq!(
        (garden.drawing_count, garden.drawing_allowance),
        (2, Allowance::Max(2))
    );
    assert_eq!(garden.storage_allowance, Allowance::Default, "unchanged");

    // Back to the instance's limit, lowered: nothing is taken away.
    assert_eq!(
        patch(serde_json::json!({ "drawing_allowance": "default" })).await,
        200
    );
    assert_eq!(vault_info(&alice, &v).await.drawing_limit, Limit::Max(1));
    let (_, key) = alice.new_key(&v, "d3", ObjectKind::Drawing);
    assert_eq!(alice.put_key(&v, "d3", &key).await.unwrap_err().0, 403);
    settings(
        &srv,
        serde_json::json!({ "drawings_per_vault": "unlimited" }),
    )
    .await;
    alice.put_key(&v, "d3", &key).await.unwrap();
    // Quick notes have no limit of their own.
    let q = alice.create_quick_notes().await.unwrap().id;
    let r = operator(&srv, Method::PATCH, &format!("/admin/vaults/{q}"))
        .json(&serde_json::json!({ "drawing_allowance": 1 }));
    assert_eq!(status(r).await, 404);
}
