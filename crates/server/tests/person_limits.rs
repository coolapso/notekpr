//! Limits per person (protocol.md §9, "Limits per person"): a request rate
//! and a daily write allowance per person — from every session, with their
//! agents —, suspensions by administrators and by the server, and what
//! administrators see of them.

use futures::StreamExt;
use notekpr_core::doc::NoteDoc;
use notekpr_core::protocol::*;
use notekpr_server::admin_clients;
use reqwest::Method;
use tokio_tungstenite::tungstenite::Message;

use crate::common::*;

const TOKEN: &str = "notekpr_a_operator-token-for-the-limits-0123456789";

async fn start_with_operator() -> TestServer {
    let srv = start().await;
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

fn operator(srv: &TestServer, m: Method, path: &str) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .request(m, format!("{}{path}", srv.base))
        .bearer_auth(TOKEN)
}

async fn patch_user(srv: &TestServer, id: &str, change: serde_json::Value) -> u16 {
    operator(srv, Method::PATCH, &format!("/admin/users/{id}"))
        .json(&change)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

async fn admin_users(srv: &TestServer, query: &str) -> Vec<AdminUser> {
    operator(srv, Method::GET, &format!("/admin/users{query}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn admin_user(srv: &TestServer, id: &str) -> AdminUser {
    admin_users(srv, "")
        .await
        .into_iter()
        .find(|u| u.id == id)
        .unwrap()
}

/// The status, error code and `Retry-After` of an answer.
async fn refusal(r: reqwest::Response) -> (u16, String, Option<u64>) {
    let status = r.status().as_u16();
    let after = r
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .map(|v| v.to_str().unwrap().parse().unwrap());
    let code = r
        .json::<ApiError>()
        .await
        .map(|e| e.error)
        .unwrap_or_default();
    (status, code, after)
}

async fn get_me(api: &Api) -> u16 {
    api.status::<()>(Method::GET, "/me", None).await
}

/// An agent of `owner`'s, signed in.
async fn agent_of(srv: &TestServer, owner: &Api) -> Api {
    let created: CreateAgentResponse = owner
        .post(
            "/agents",
            &CreateAgentRequest {
                name: "Assistant".into(),
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
    let mut api = Api::new(&srv.base);
    api.token = Some(tok.access_token);
    api
}

#[tokio::test]
async fn a_persons_requests_count_from_every_session_and_their_agents() {
    let srv = start_with_operator().await;
    let (laptop, alice) = dev_login(&srv.base, "alice").await;
    let (phone, _) = dev_login(&srv.base, "alice").await;
    let agent = agent_of(&srv, &laptop).await;
    let (bob, _) = dev_login(&srv.base, "bob").await;
    srv.set(InstanceSettingsUpdate {
        person_rate_limit_rps: Some(1),
        auto_suspend_minutes: Some(0),
        ..Default::default()
    })
    .await;

    // A burst of 4 seconds' worth from the laptop; then her phone and her
    // agent share what refills (at most one more in the moment).
    for _ in 0..4 {
        assert_eq!(get_me(&laptop).await, 200);
    }
    let mut refused = 0;
    for api in [&phone, &phone, &agent, &agent] {
        let (status, code, after) =
            refusal(api.req(Method::GET, "/me").send().await.unwrap()).await;
        if status == 429 {
            assert_eq!(code, ERROR_RATE_LIMITED);
            assert_eq!(after, Some(1));
            refused += 1;
        }
    }
    assert!(refused >= 3, "{refused}");
    // Bob has his own.
    assert_eq!(get_me(&bob).await, 200);

    // Her own rate, given by an administrator, applies at once.
    assert_eq!(
        patch_user(
            &srv,
            &alice.user.id,
            serde_json::json!({"rate_allowance": "unlimited"})
        )
        .await,
        200
    );
    for _ in 0..20 {
        assert_eq!(get_me(&phone).await, 200);
        assert_eq!(get_me(&agent).await, 200);
    }
    let me = admin_user(&srv, &alice.user.id).await;
    assert_eq!(me.rate_allowance, Allowance::Unlimited);
    assert!(me.refused.requests >= 3);
    // At least one a second; agents have none of their own.
    assert_eq!(
        patch_user(
            &srv,
            &alice.user.id,
            serde_json::json!({"rate_allowance": 0})
        )
        .await,
        400
    );
    let agent_id = admin_users(&srv, "")
        .await
        .into_iter()
        .find(|u| u.kind == PrincipalKind::Agent)
        .unwrap()
        .id;
    assert_eq!(
        patch_user(&srv, &agent_id, serde_json::json!({"rate_allowance": 5})).await,
        400
    );
}

#[tokio::test]
async fn changes_count_against_the_daily_write_allowance() {
    let srv = start_with_operator().await;
    let mut alice = new_user(&srv.base, "alice").await;
    let bob = new_user(&srv.base, "bob").await;
    let v = alice.create_vault("Big").await;
    srv.set(InstanceSettingsUpdate {
        writes_per_day_mb: Some(Limit::Max(1)),
        ..Default::default()
    })
    .await;
    // About 930 KiB each, as base64 in JSON.
    let big = |alice: &User, object: &str| {
        let note = NoteDoc::new();
        let u = note.set_text(&"x".repeat(700 * 1024)).unwrap();
        PushRequest {
            updates: vec![alice.encrypt_update(&v, object, &u)],
        }
    };
    let push = |alice: &User, req: PushRequest| {
        alice
            .api
            .req(Method::POST, &format!("/vaults/{v}/updates"))
            .json(&req)
            .send()
    };
    // Accepted while any is left: the second goes past it.
    assert_eq!(push(&alice, big(&alice, "a")).await.unwrap().status(), 200);
    assert_eq!(push(&alice, big(&alice, "b")).await.unwrap().status(), 200);
    let head = |alice: &User| {
        let api = alice.api.clone();
        let path = format!("/vaults/{v}");
        async move { api.get::<VaultInfo>(&path).await.head_seq }
    };
    let before = head(&alice).await;
    let (status, code, after) = refusal(push(&alice, big(&alice, "c")).await.unwrap()).await;
    assert_eq!((status, code.as_str()), (429, ERROR_WRITE_LIMIT));
    // About 840 KiB past it, at a MiB a day: some 20 hours.
    let after = after.unwrap();
    assert!((18 * 3600..22 * 3600).contains(&after), "{after}");
    assert_eq!(head(&alice).await, before, "nothing was stored");

    // Small changes too (a vault), but not reading, her settings, or
    // deleting something for good.
    assert_eq!(
        refusal(
            alice
                .api
                .req(Method::POST, "/vaults")
                .json(&serde_json::json!({}))
                .send()
                .await
                .unwrap()
        )
        .await
        .1,
        ERROR_WRITE_LIMIT
    );
    let _: ChangesResponse = alice.api.get(&format!("/vaults/{v}/changes?since=0")).await;
    assert_eq!(
        alice
            .api
            .status(
                Method::PUT,
                "/me/prefs",
                Some(&PutPrefsRequest {
                    ciphertext: vec![1; 64],
                    expected_version: 0,
                }),
            )
            .await,
        200
    );
    assert_eq!(
        alice
            .api
            .status::<()>(Method::DELETE, &format!("/vaults/{v}/objects/a"), None)
            .await,
        204
    );

    // Over the socket: refused with when to come back.
    let mut ws = Ws::connect(&srv.ws, alice.api.token.as_deref().unwrap()).await;
    ws.send(&ClientMessage::Subscribe {
        vault_id: v.clone(),
        since: None,
        object_id: None,
    })
    .await;
    ws.until(|m| matches!(m, ServerMessage::Synced { .. }).then_some(()))
        .await;
    ws.send(&ClientMessage::Push {
        vault_id: v.clone(),
        updates: big(&alice, "d").updates,
    })
    .await;
    let (code, retry_after) = ws
        .until(|m| match m {
            ServerMessage::Error {
                code, retry_after, ..
            } => Some((code.clone(), *retry_after)),
            _ => None,
        })
        .await;
    assert_eq!(code, ERROR_WRITE_LIMIT);
    assert!(retry_after.is_some_and(|s| s > 3600));

    // Bob writes on.
    let mut bob = bob;
    let w = bob.create_vault("Small").await;
    let note = NoteDoc::new();
    let u = note.set_text("hi").unwrap();
    bob.push(&w, vec![bob.encrypt_update(&w, "n", &u)])
        .await
        .unwrap();

    // Administrators see who ran into it, and give her more.
    let limited = admin_users(&srv, "?limited=true").await;
    assert_eq!(
        limited.iter().map(|u| u.id.as_str()).collect::<Vec<_>>(),
        [alice.id.as_str()]
    );
    assert!(limited[0].refused.writes >= 3);
    assert_eq!(
        patch_user(&srv, &alice.id, serde_json::json!({"write_allowance": 100})).await,
        200
    );
    assert_eq!(
        admin_user(&srv, &alice.id).await.write_allowance,
        Allowance::Max(100)
    );
    assert_eq!(push(&alice, big(&alice, "e")).await.unwrap().status(), 200);
    // At once on her open connection too.
    ws.send(&ClientMessage::Push {
        vault_id: v.clone(),
        updates: big(&alice, "f").updates,
    })
    .await;
    ws.until(|m| match m {
        ServerMessage::Ack { .. } => Some(()),
        ServerMessage::Error { code, .. } => panic!("refused: {code}"),
        _ => None,
    })
    .await;
}

#[tokio::test]
async fn a_suspended_person_and_their_agents_are_refused_until_it_ends() {
    let srv = start_with_operator().await;
    let alice = new_user(&srv.base, "alice").await;
    let agent = agent_of(&srv, &alice.api).await;
    let mut ws = Ws::connect(&srv.ws, alice.api.token.as_deref().unwrap()).await;

    let until = notekpr_core::now_ms() + 3_600_000;
    assert_eq!(
        patch_user(
            &srv,
            &alice.id,
            serde_json::json!({"suspended_until": until})
        )
        .await,
        200
    );
    // Her connection ends, saying until when.
    let retry_after = ws
        .until(|m| match m {
            ServerMessage::Error {
                code, retry_after, ..
            } if code == ERROR_SUSPENDED => Some(*retry_after),
            _ => None,
        })
        .await;
    assert!(retry_after.is_some_and(|s| (3500..=3600).contains(&s)));
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(5), ws.stream.next()).await,
        Ok(None | Some(Ok(Message::Close(_))) | Some(Err(_)))
    ));
    // Every request, hers and her agent's, and connecting again.
    for api in [&alice.api, &agent] {
        let (status, code, after) =
            refusal(api.req(Method::GET, "/me").send().await.unwrap()).await;
        assert_eq!((status, code.as_str()), (429, ERROR_SUSPENDED));
        assert!(after.is_some_and(|s| s > 3500));
    }
    let (mut stream, _) = tokio_tungstenite::connect_async(&srv.ws).await.unwrap();
    use futures::SinkExt;
    let auth = serde_json::to_string(&ClientMessage::Auth {
        token: alice.api.token.clone().unwrap(),
        app: None,
    })
    .unwrap();
    stream.send(Message::Text(auth.into())).await.unwrap();
    let Some(Ok(Message::Text(t))) = stream.next().await else {
        panic!("no answer");
    };
    assert!(matches!(
        serde_json::from_str(&t).unwrap(),
        ServerMessage::Error { code, .. } if code == ERROR_SUSPENDED
    ));
    let me = admin_user(&srv, &alice.id).await;
    assert_eq!(me.suspended_until, Some(until));
    assert_eq!(
        admin_users(&srv, "?limited=true")
            .await
            .iter()
            .map(|u| u.id.as_str())
            .collect::<Vec<_>>(),
        [alice.id.as_str()]
    );
    // Not an agent on its own.
    let agent_id = admin_users(&srv, "")
        .await
        .into_iter()
        .find(|u| u.kind == PrincipalKind::Agent)
        .unwrap()
        .id;
    assert_eq!(
        patch_user(
            &srv,
            &agent_id,
            serde_json::json!({"suspended_until": until})
        )
        .await,
        400
    );

    // Lifted: everything works again, and both are in her audit log.
    assert_eq!(
        patch_user(&srv, &alice.id, serde_json::json!({"suspended_until": 0})).await,
        200
    );
    assert_eq!(get_me(&alice.api).await, 200);
    assert_eq!(get_me(&agent).await, 200);
    assert_eq!(admin_user(&srv, &alice.id).await.suspended_until, None);
    let audit: Vec<AuditEntry> = alice.api.get("/me/audit").await;
    let actions: Vec<&str> = audit.iter().map(|a| a.action.as_str()).collect();
    assert!(actions.contains(&"account.suspended"), "{actions:?}");
    assert!(actions.contains(&"account.unsuspended"), "{actions:?}");
}

#[tokio::test]
async fn administrators_dont_suspend_themselves_and_are_held_to_their_limits() {
    let srv = start_with(|c| c.first_user_admin = true).await;
    let (admin, me) = dev_login(&srv.base, "root").await;
    let (_, other) = dev_login(&srv.base, "eve").await;
    let until = notekpr_core::now_ms() + 3_600_000;
    let patch = |id: &str, change: serde_json::Value| {
        admin
            .req(Method::PATCH, &format!("/admin/users/{id}"))
            .json(&change)
            .send()
    };
    let r = patch(&me.user.id, serde_json::json!({"suspended_until": until}))
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    // Their rate counts on administration too.
    srv.set(InstanceSettingsUpdate {
        person_rate_limit_rps: Some(1),
        auto_suspend_minutes: Some(0),
        ..Default::default()
    })
    .await;
    let mut refused = false;
    for _ in 0..10 {
        let r = admin.req(Method::GET, "/admin/users").send().await.unwrap();
        refused |= r.status() == 429;
    }
    assert!(refused);
    let _ = other;
}

#[tokio::test]
async fn a_session_that_keeps_going_past_its_rate_gets_its_person_suspended() {
    let srv = start_with_operator().await;
    let (script, alice) = dev_login(&srv.base, "alice").await;
    let (bob_api, bob) = dev_login(&srv.base, "bob").await;
    srv.set(InstanceSettingsUpdate {
        person_rate_limit_rps: Some(1),
        ..Default::default()
    })
    .await;
    // Not waiting as told, many at once: 12 000 refusals and a few more
    // (each second drains 20).
    let mut suspended = false;
    for _ in 0..80 {
        let answers =
            futures::future::join_all((0..200).map(|_| script.req(Method::GET, "/me").send()))
                .await;
        for a in answers {
            let (_, code, _) = refusal(a.unwrap()).await;
            suspended |= code == ERROR_SUSPENDED;
        }
        if suspended {
            break;
        }
    }
    assert!(suspended, "never suspended");
    let me = admin_user(&srv, &alice.user.id).await;
    let minutes = (me.suspended_until.unwrap() - notekpr_core::now_ms()) / 60_000;
    assert!((58..=60).contains(&minutes), "{minutes}");
    assert!(me.refused.requests >= 12_000);
    let audit_by_server = sqlx::query_scalar::<_, String>(
        "SELECT details FROM audit_log WHERE action = 'account.suspended' AND actor_id IS NULL",
    )
    .fetch_all(&srv.state.db)
    .await
    .unwrap();
    assert_eq!(audit_by_server.len(), 1);
    assert!(audit_by_server[0].contains("\"by\":\"server\""));

    // Turned off: never.
    srv.set(InstanceSettingsUpdate {
        auto_suspend_minutes: Some(0),
        ..Default::default()
    })
    .await;
    for _ in 0..70 {
        let answers =
            futures::future::join_all((0..200).map(|_| bob_api.req(Method::GET, "/me").send()))
                .await;
        for a in answers {
            assert_ne!(refusal(a.unwrap()).await.1, ERROR_SUSPENDED);
        }
    }
    assert_eq!(admin_user(&srv, &bob.user.id).await.suspended_until, None);
}

/// An agent's tokens count against its owner: none while the owner is
/// suspended, at the sign-in rate per person from however many addresses,
/// and at most 20 that work at once.
#[tokio::test]
async fn agents_tokens_count_against_their_owner() {
    let srv = start_with(|c| c.trust_proxy = true).await;
    admin_clients::ensure(
        &srv.state.db,
        "operator",
        &AdminScope::ALL,
        &[TOKEN.to_string()],
    )
    .await
    .unwrap();
    let alice = new_user(&srv.base, "alice").await;
    let created: CreateAgentResponse = alice
        .api
        .post(
            "/agents",
            &CreateAgentRequest {
                name: "Assistant".into(),
            },
        )
        .await;
    let mut n = 0u32;
    let mut mint = || {
        n += 1;
        reqwest::Client::new()
            .post(format!("{}/oauth/token", srv.base))
            // Every time from another address.
            .header("x-forwarded-for", format!("203.0.113.{}", n % 250 + 1))
            .basic_auth(
                &created.credential.client_id,
                Some(&created.credential.client_secret),
            )
            .form(&[("grant_type", "client_credentials")])
            .send()
    };
    for _ in 0..25 {
        assert_eq!(mint().await.unwrap().status(), 200);
    }
    let live: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sessions WHERE user_id = ? AND revoked_at IS NULL",
    )
    .bind(&created.agent.id)
    .fetch_one(&srv.state.db)
    .await
    .unwrap();
    assert_eq!(live, 20, "an agent keeps 20 tokens at most");

    // Its owner suspended: no token.
    let until = notekpr_core::now_ms() + 3_600_000;
    assert_eq!(
        patch_user(
            &srv,
            &alice.id,
            serde_json::json!({"suspended_until": until})
        )
        .await,
        200
    );
    let r = mint().await.unwrap();
    assert_eq!(r.status(), 429);
    assert!(r.headers().contains_key(reqwest::header::RETRY_AFTER));
    assert_eq!(r.json::<OAuthError>().await.unwrap().error, ERROR_SUSPENDED);
    assert_eq!(
        patch_user(&srv, &alice.id, serde_json::json!({"suspended_until": 0})).await,
        200
    );

    // At the sign-in rate per person, whatever the address.
    srv.set(InstanceSettingsUpdate {
        auth_rate_limit_per_min: Some(2),
        ..Default::default()
    })
    .await;
    let mut refused = None;
    for _ in 0..5 {
        let r = mint().await.unwrap();
        if r.status() == 429 {
            refused = Some(r.json::<OAuthError>().await.unwrap().error);
            break;
        }
    }
    assert_eq!(refused.as_deref(), Some(ERROR_RATE_LIMITED));
}

#[tokio::test]
async fn a_person_has_at_most_fifty_connections() {
    let srv = start().await;
    let alice = new_user(&srv.base, "alice").await;
    let token = alice.api.token.clone().unwrap();
    let mut open = Vec::new();
    for _ in 0..50 {
        open.push(Ws::connect(&srv.ws, &token).await);
    }
    let (mut stream, _) = tokio_tungstenite::connect_async(&srv.ws).await.unwrap();
    use futures::SinkExt;
    let auth = serde_json::to_string(&ClientMessage::Auth {
        token: token.clone(),
        app: None,
    })
    .unwrap();
    stream.send(Message::Text(auth.into())).await.unwrap();
    let Some(Ok(Message::Text(t))) = stream.next().await else {
        panic!("no answer");
    };
    assert!(matches!(
        serde_json::from_str(&t).unwrap(),
        ServerMessage::Error { code, .. } if code == ERROR_RATE_LIMITED
    ));
    // One closes: another may come.
    drop(open.pop());
    let mut again = None;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let (mut s, _) = tokio_tungstenite::connect_async(&srv.ws).await.unwrap();
        s.send(Message::Text(
            serde_json::to_string(&ClientMessage::Auth {
                token: token.clone(),
                app: None,
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
        if let Some(Ok(Message::Text(t))) = s.next().await
            && matches!(
                serde_json::from_str(&t).unwrap(),
                ServerMessage::Ready { .. }
            )
        {
            again = Some(s);
            break;
        }
    }
    assert!(again.is_some());
}
