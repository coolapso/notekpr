//! Sign-in through OpenID Connect against a small mock identity provider:
//! login bound to the browser that started it, client authentication
//! methods, profile from the userinfo endpoint, verified e-mail only.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use notekpr_core::protocol::{ExchangeRequest, SessionResponse};
use openidconnect::core::{CoreJwsSigningAlgorithm, CoreRsaPrivateSigningKey};
use openidconnect::{JsonWebKeyId, PrivateSigningKey};
use serde_json::{Value, json};

/// Throwaway RSA key used only to sign the mock provider's ID tokens in tests.
const TEST_KEY_PEM: &str = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEAqxioZbIfZDcyQC3MGiogJQch171NXKPaaQ+k6QDCqvOHWQHl\nBo4dI15avgJXzacdsm2CO0f7h3J7691dAvPJs6Ag+xnEdp6VNWi7/rAwkaAn7lp+\nVIk/qL6eI8DqytRRQ/xumv2qFzoH51dWsqE6psXiJWa0JvyDK40yFfBhufUN97GI\nLTvLbJ4qr9yfbRlQY4V29heeMsUj34DuXYkrE6FwyLwoja4gDO0lXUufuQxmB8Gs\nhZKHpELTuJArJpP8S6+azgC3yVIf2ZhRSBySG8ICXjC8MFhP79WAAJ/KqmCDjwPn\nXvA9bgNzAzz/jWnz/uoVBCmxe3NJPAag//UITwIDAQABAoIBABpQ0JXTr8BaPQBi\nCuJ7WDZZwnrMQvncudsu7M5pr4dbrZWjFPbNJO4rtQlEkvyul4K6FS3JBIbwtN6y\nTXwGwx89odT9QZp9CbYNRd0GfHHpkDq48HVQJtptdyVIptRLk7RBP0HXgjHQWybZ\n6BvRcJP2G7Jk+ZCFhidMsEdvkZhoOClMOGZGkBkv0ahy6CIaMruWwvExkkEU8Nhe\n6O2FhspNimaduE7yLd6BtTt5aVQI/DOh4qgKNS4U3HPfSsAyI+ywbDVG8qeZOY5D\nVDD5fQFPCXZkuCEhCqB0gcgHccdEM6jcEpsuy6VZm/7XugaozM2SKMt7AvVDEf9e\n6UduigECgYEA26DxI7GI/PtknzofXPYskGi015BTo2vC1MgTwG7y/Yf3N691cxYK\nnK2mytW8M1egxoyeV337OsS43r7OHSw6WMMbmyz8JOFNjAdpzLEQmUBAMBXsz9p1\nwUCyAD33facMfccLRfEoePGboAMJufOiGKzxlvO7NhOaCS1AB+cQIfcCgYEAx241\nYMM+BbZOZkWslaOQYsdfseM2e4XPd2NrhWM/hBou+tJdfeHV+zpOysDj0nBlhY+B\nCgJjW7KV4T/CFu49FvdmqvataebnSnztcJHSumlsau6CIdM2x4j2vm0W/C6n3AB1\n5rrzetTbMXcqg2giYnkBrUK44jR+2zibTir7NmkCgYEAyDYz/MTXyz7koF5cvuLP\nByncSXIRPLV8m3p0vwSlUEeFLuctGyepruCCXv9unCMytiz0H3AB+LXijXPE80Uh\nP09FODVO+dPFXHRZlzaGmh2/Jdam1Wgh1nTCj4PtvEbvFS3A8haAPZiKtNhwImqd\nhv8JEQHI1OgTkHAGoe2agEsCgYAm+AwKGlyWNSdgqKCupMVj5911HxbYUC9X/t9V\nsy0IHS2s6UpHfMeZTa1zxPk/d6L+1Cm80PalRmIvH4xHesoJINK4+XatSOdNENg8\ntOHedxcuVeirv1/m4YaFcz+Yi21VeemP6lwFVm52Xfi6sLB3WXnQZ0dFPEjFgWwn\nfBKWcQKBgQCVCZ2z7sgB/JBjmuuz8Ghna/5VhCXZAyA/sqeNfffQkQ8p6950HhUa\nDEZRXc9eK0ZZ1tCSqOiCjQEXpeM+a3c/oPMbUb2aB9aYTHfit+JYyLimOrW9IZzQ\nCvcuiv7fSSTU7TBCWzIao2Q48TqvV0KePBdJxSbUKLOjWBu5GCyIHg==\n-----END RSA PRIVATE KEY-----";
const CLIENT_ID: &str = "notekpr";
const CLIENT_SECRET: &str = "s3cret";

#[derive(Clone)]
struct Idp(Arc<IdpInner>);

struct IdpInner {
    issuer: String,
    /// `token_endpoint_auth_methods_supported`, also enforced by /token.
    auth_methods: Vec<&'static str>,
    /// Put name and e-mail into the ID token (otherwise only in userinfo).
    profile_in_id_token: bool,
    email_verified: bool,
    key: CoreRsaPrivateSigningKey,
    codes: Mutex<HashMap<String, String>>,
    used_auth: Mutex<Vec<&'static str>>,
}

fn b64(data: &[u8]) -> String {
    data_encoding::BASE64URL_NOPAD.encode(data)
}

impl Idp {
    fn id_token(&self, nonce: &str) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut claims = json!({
            "iss": self.0.issuer, "aud": CLIENT_ID, "sub": "user-1",
            "iat": now, "exp": now + 300, "nonce": nonce,
        });
        if self.0.profile_in_id_token {
            claims["name"] = json!("Olivia");
            claims["email"] = json!("olivia@example.com");
            claims["email_verified"] = json!(self.0.email_verified);
        }
        let header = json!({"alg": "RS256", "typ": "JWT", "kid": "test"});
        let signing_input = format!(
            "{}.{}",
            b64(header.to_string().as_bytes()),
            b64(claims.to_string().as_bytes())
        );
        let sig = self
            .0
            .key
            .sign(
                &CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
                signing_input.as_bytes(),
            )
            .unwrap();
        format!("{signing_input}.{}", b64(&sig))
    }
}

async fn discovery(State(idp): State<Idp>) -> Json<Value> {
    let i = &idp.0.issuer;
    Json(json!({
        "issuer": i,
        "authorization_endpoint": format!("{i}/authorize"),
        "token_endpoint": format!("{i}/token"),
        "userinfo_endpoint": format!("{i}/userinfo"),
        "jwks_uri": format!("{i}/jwks"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "token_endpoint_auth_methods_supported": idp.0.auth_methods,
    }))
}

async fn jwks(State(idp): State<Idp>) -> Json<Value> {
    Json(json!({ "keys": [serde_json::to_value(idp.0.key.as_verification_key()).unwrap()] }))
}

async fn authorize(State(idp): State<Idp>, Query(q): Query<HashMap<String, String>>) -> Response {
    let code = format!("code-{}", idp.0.codes.lock().unwrap().len());
    idp.0
        .codes
        .lock()
        .unwrap()
        .insert(code.clone(), q["nonce"].clone());
    Redirect::to(&format!(
        "{}?code={code}&state={}",
        q["redirect_uri"], q["state"]
    ))
    .into_response()
}

async fn token(
    State(idp): State<Idp>,
    headers: HeaderMap,
    Form(f): Form<HashMap<String, String>>,
) -> Response {
    let basic = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
        .and_then(|v| data_encoding::BASE64.decode(v.as_bytes()).ok())
        .is_some_and(|v| v == format!("{CLIENT_ID}:{CLIENT_SECRET}").as_bytes());
    let post = f.get("client_id").map(String::as_str) == Some(CLIENT_ID)
        && f.get("client_secret").map(String::as_str) == Some(CLIENT_SECRET);
    let method = if basic && idp.0.auth_methods.contains(&"client_secret_basic") {
        "client_secret_basic"
    } else if post && idp.0.auth_methods.contains(&"client_secret_post") {
        "client_secret_post"
    } else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid_client"})),
        )
            .into_response();
    };
    idp.0.used_auth.lock().unwrap().push(method);
    let Some(nonce) = idp.0.codes.lock().unwrap().remove(&f["code"]) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_grant"})),
        )
            .into_response();
    };
    Json(json!({
        "access_token": "at-1", "token_type": "Bearer", "expires_in": 300,
        "id_token": idp.id_token(&nonce),
    }))
    .into_response()
}

async fn userinfo(State(idp): State<Idp>) -> Json<Value> {
    Json(json!({
        "sub": "user-1", "name": "Olivia from userinfo",
        "email": "olivia@example.com", "email_verified": idp.0.email_verified,
    }))
}

struct Setup {
    idp: Idp,
    base: String,
    http: reqwest::Client,
    _dir: tempfile::TempDir,
    state: notekpr_server::state::AppState,
}

async fn setup(
    auth_methods: Vec<&'static str>,
    profile_in_id_token: bool,
    email_verified: bool,
) -> Setup {
    let idp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", idp_listener.local_addr().unwrap());
    let idp = Idp(Arc::new(IdpInner {
        issuer: issuer.clone(),
        auth_methods,
        profile_in_id_token,
        email_verified,
        key: CoreRsaPrivateSigningKey::from_pem(
            TEST_KEY_PEM,
            Some(JsonWebKeyId::new("test".into())),
        )
        .unwrap(),
        codes: Mutex::default(),
        used_auth: Mutex::default(),
    }));
    let app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/jwks", get(jwks))
        .route("/authorize", get(authorize))
        .route("/token", post(token))
        .route("/userinfo", get(userinfo))
        .with_state(idp.clone());
    tokio::spawn(async move { axum::serve(idp_listener, app).await.unwrap() });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = notekpr_server::config::Config::for_tests(dir.path().to_path_buf());
    cfg.dev_mode = false;
    cfg.public_url = format!("http://{addr}");
    cfg.oidc_issuer = Some(issuer);
    cfg.oidc_client_id = Some(CLIENT_ID.into());
    cfg.oidc_client_secret = Some(CLIENT_SECRET.into());
    let (app, state) = notekpr_server::build(cfg, None).await.unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    Setup {
        idp,
        base: format!("http://{addr}"),
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        _dir: dir,
        state,
    }
}

fn location(r: &reqwest::Response) -> String {
    r.headers()[header::LOCATION].to_str().unwrap().to_string()
}

impl Setup {
    /// Run the browser side of a login. `send_cookie = false` plays a victim
    /// who opens a callback link someone else prepared.
    async fn login(&self, send_cookie: bool) -> String {
        let start = self
            .http
            .get(format!("{}/api/v1/auth/oidc/start", self.base))
            .send()
            .await
            .unwrap();
        assert!(start.status().is_redirection());
        let cookie = start
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .find(|c| c.starts_with("notekpr_login="))
            .expect("login cookie");
        assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Lax"));
        let cookie = cookie.split(';').next().unwrap().to_string();
        let at_idp = self.http.get(location(&start)).send().await.unwrap();
        let mut callback = self.http.get(location(&at_idp));
        if send_cookie {
            callback = callback.header(header::COOKIE, cookie);
        }
        let done = callback.send().await.unwrap();
        location(&done)
    }

    async fn exchange(&self, complete: &str) -> SessionResponse {
        let code = complete
            .split_once("#code=")
            .and_then(|(_, f)| f.split('&').next())
            .unwrap_or_else(|| panic!("no code in {complete}"))
            .to_string();
        let r = self
            .http
            .post(format!("{}/api/v1/auth/exchange", self.base))
            .json(&ExchangeRequest { code })
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        r.json().await.unwrap()
    }
}

#[tokio::test]
async fn login_works_and_is_bound_to_the_browser() {
    let s = setup(
        vec!["client_secret_basic", "client_secret_post"],
        true,
        true,
    )
    .await;
    let done = s.login(true).await;
    let session = s.exchange(&done).await;
    assert_eq!(session.user.display_name, "Olivia");
    assert_eq!(session.user.email.as_deref(), Some("olivia@example.com"));
    // Basic is preferred when the provider offers both.
    assert_eq!(
        *s.idp.0.used_auth.lock().unwrap(),
        vec!["client_secret_basic"]
    );

    // A callback link from someone else's login doesn't sign this browser in.
    let done = s.login(false).await;
    assert!(done.ends_with("#error=login_failed"), "{done}");
}

#[tokio::test]
async fn client_secret_post_and_profile_from_userinfo() {
    // Like Authelia configured for client_secret_post, with the profile only
    // available from the userinfo endpoint.
    let s = setup(vec!["client_secret_post"], false, true).await;
    let session = s.exchange(&s.login(true).await).await;
    assert_eq!(
        *s.idp.0.used_auth.lock().unwrap(),
        vec!["client_secret_post"]
    );
    assert_eq!(session.user.display_name, "Olivia from userinfo");
    assert_eq!(session.user.email.as_deref(), Some("olivia@example.com"));
}

#[tokio::test]
async fn unverified_email_is_not_used() {
    let s = setup(vec!["client_secret_basic"], true, false).await;
    let session = s.exchange(&s.login(true).await).await;
    assert_eq!(session.user.display_name, "Olivia");
    assert_eq!(session.user.email, None);
}

#[tokio::test]
async fn dev_accounts_never_take_the_first_admin_role() {
    let s = setup(vec!["client_secret_basic"], true, true).await;
    // The same database was used in development mode before.
    let dir = s._dir.path().to_path_buf();
    let mut cfg = notekpr_server::config::Config::for_tests(dir.clone());
    cfg.public_url = s.base.clone();
    let (dev_app, _) = notekpr_server::build(cfg, None).await.unwrap();
    let dev_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dev_base = format!("http://{}", dev_listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(
            dev_listener,
            dev_app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let r = s
        .http
        .post(format!("{dev_base}/api/v1/auth/dev-login"))
        .json(&json!({"username": "tester"}))
        .send()
        .await
        .unwrap();
    let dev: SessionResponse = r.json().await.unwrap();
    assert!(dev.user.is_admin, "first dev user is admin in dev mode");

    // The first real person to sign in still becomes the administrator.
    let session = s.exchange(&s.login(true).await).await;
    assert!(session.user.is_admin);

    // Restarting without development mode ends the dev account's sessions.
    let mut cfg = notekpr_server::config::Config::for_tests(dir);
    cfg.dev_mode = false;
    cfg.oidc_issuer = Some(s.idp.0.issuer.clone());
    cfg.oidc_client_id = Some(CLIENT_ID.into());
    let (app, _) = notekpr_server::build(cfg, None).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let me = |token: String| {
        let http = s.http.clone();
        let base = base.clone();
        async move {
            http.get(format!("{base}/api/v1/me"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(me(dev.token).await, 401);
    assert_eq!(me(session.token).await, 200);
}

#[tokio::test]
async fn a_full_server_takes_no_new_accounts_but_lets_people_back_in() {
    use notekpr_core::protocol::{InstanceSettingsUpdate, Limit};
    let s = setup(vec!["client_secret_basic"], true, true).await;
    let set = |change: InstanceSettingsUpdate| {
        let state = s.state.clone();
        async move { state.instance.update(&state.db, &change).await.unwrap() }
    };
    set(InstanceSettingsUpdate {
        max_users: Some(Limit::Max(0)),
        ..Default::default()
    })
    .await;
    let done = s.login(true).await;
    assert!(done.ends_with("#error=no_new_accounts"), "{done}");
    set(InstanceSettingsUpdate {
        max_users: Some(Limit::Unlimited),
        read_only: Some(true),
        ..Default::default()
    })
    .await;
    let done = s.login(true).await;
    assert!(done.ends_with("#error=no_new_accounts"), "{done}");
    set(InstanceSettingsUpdate {
        read_only: Some(false),
        ..Default::default()
    })
    .await;
    s.exchange(&s.login(true).await).await;
    // Someone who has an account signs in while it's full or read-only.
    set(InstanceSettingsUpdate {
        max_users: Some(Limit::Max(1)),
        read_only: Some(true),
        ..Default::default()
    })
    .await;
    s.exchange(&s.login(true).await).await;
}
