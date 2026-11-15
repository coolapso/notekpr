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
    let dir = tempfile::tempdir().unwrap();
    setup_in(dir, auth_methods, profile_in_id_token, email_verified).await
}

/// [`setup`] on a database that's there already.
async fn setup_in(
    dir: tempfile::TempDir,
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
    let base = format!("http://{}", listener.local_addr().unwrap());
    let state = serve(dir.path(), &issuer, &base, listener).await;
    Setup {
        idp,
        base,
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        _dir: dir,
        state,
    }
}

/// A server signing in with the provider at `issuer`, at `public_url`,
/// answering on `listener`.
async fn serve(
    dir: &std::path::Path,
    issuer: &str,
    public_url: &str,
    listener: tokio::net::TcpListener,
) -> notekpr_server::state::AppState {
    let mut cfg = notekpr_server::config::Config::for_tests(dir.to_path_buf());
    cfg.dev_mode = false;
    cfg.public_url = public_url.to_string();
    cfg.oidc_issuer = Some(issuer.to_string());
    cfg.oidc_client_id = Some(CLIENT_ID.into());
    cfg.oidc_client_secret = Some(CLIENT_SECRET.into());
    let (app, state) = notekpr_server::build_for_tests(cfg).await.unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    state
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
            .json(&ExchangeRequest {
                code,
                code_verifier: None,
            })
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
    // The database was used in development mode before.
    let dir = tempfile::tempdir().unwrap();
    let cfg = notekpr_server::config::Config::for_tests(dir.path().to_path_buf());
    let (dev_app, dev_state) = notekpr_server::build_for_tests(cfg).await.unwrap();
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
    let r = reqwest::Client::new()
        .post(format!("{dev_base}/api/v1/auth/dev-login"))
        .json(&json!({"username": "tester"}))
        .send()
        .await
        .unwrap();
    let dev: SessionResponse = r.json().await.unwrap();
    assert!(dev.user.is_admin, "first dev user is admin in dev mode");
    dev_state.hand_off().await;

    // Started again without development mode: the dev account's sessions
    // end.
    let s = setup_in(dir, vec!["client_secret_basic"], true, true).await;
    let me = |token: String| {
        let http = s.http.clone();
        let base = s.base.clone();
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

    // The first real person to sign in still becomes the administrator.
    let session = s.exchange(&s.login(true).await).await;
    assert!(session.user.is_admin);
    assert_eq!(me(session.token).await, 200);
}

#[tokio::test]
async fn a_sign_in_reaching_the_server_handing_over_finishes_on_the_next_one() {
    let s = setup(vec!["client_secret_basic"], true, true).await;
    let start = s
        .http
        .get(format!("{}/api/v1/auth/oidc/start", s.base))
        .send()
        .await
        .unwrap();
    let cookie = start
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .find(|c| c.starts_with("notekpr_login="))
        .unwrap();
    let cookie = cookie.split(';').next().unwrap().to_string();
    let at_idp = s.http.get(location(&start)).send().await.unwrap();
    let callback = location(&at_idp);

    // The server stops meanwhile: the way back from the provider is
    // refused before it uses up the sign-in.
    s.state.hand_off().await;
    let refused = s
        .http
        .get(&callback)
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 503);
    assert_eq!(refused.headers()["retry-after"], "1");
    // Starting one is held too: it stores the sign-in's state.
    let (_, challenge) = pkce(1);
    for start in [
        s.http.get(format!("{}/api/v1/auth/oidc/start", s.base)),
        s.app_start(APP_BACK, &challenge),
    ] {
        assert_eq!(start.send().await.unwrap().status(), 503);
    }

    // The next version, at the same address for the provider, finishes it.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let next = format!("http://{}", listener.local_addr().unwrap());
    serve(s._dir.path(), &s.idp.0.issuer, &s.base, listener).await;
    let done = s
        .http
        .get(callback.replacen(&s.base, &next, 1))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    let complete = location(&done);
    assert!(complete.contains("#code="), "{complete}");
    let session = Setup { base: next, ..s }.exchange(&complete).await;
    assert!(session.user.is_admin, "the first person");
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

// ------------------------------------------------- apps (protocol.md §3)

/// A PKCE verifier and its S256 challenge.
fn pkce(seed: u8) -> (String, String) {
    use sha2::Digest;
    let verifier = data_encoding::BASE64URL_NOPAD.encode(&[seed; 32]);
    let challenge =
        data_encoding::BASE64URL_NOPAD.encode(&sha2::Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

const APP_BACK: &str = "http://127.0.0.1:4321/callback/x9-_";

/// The confirmation page's form token.
fn form_token(page: &str) -> String {
    page.split_once("name=\"form\" value=\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(v, _)| v.to_string())
        .unwrap_or_else(|| panic!("no form token in {page}"))
}

/// The value of the response's `Set-Cookie` that starts with `name`.
fn first_cookie(r: &reqwest::Response, name: &str) -> String {
    r.headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .find(|c| c.starts_with(name))
        .unwrap_or_else(|| panic!("no {name} cookie"))
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

impl Setup {
    fn app_start(&self, redirect: &str, challenge: &str) -> reqwest::RequestBuilder {
        self.http
            .get(format!("{}/api/v1/auth/oidc/start", self.base))
            .query(&[
                ("redirect_uri", redirect),
                ("code_challenge", challenge),
                ("code_challenge_method", "S256"),
            ])
    }

    /// An app's sign-in in the browser, up to the page confirming it: that
    /// page and the cookie that goes with it.
    async fn app_to_confirm(&self, challenge: &str) -> (String, String) {
        let start = self.app_start(APP_BACK, challenge).send().await.unwrap();
        assert!(start.status().is_redirection(), "{}", start.status());
        let at_idp = self.http.get(location(&start)).send().await.unwrap();
        let back = self
            .http
            .get(location(&at_idp))
            .header(header::COOKIE, first_cookie(&start, "notekpr_login="))
            .send()
            .await
            .unwrap();
        // Not back to the app yet: to the page confirming it.
        assert_eq!(location(&back), "/api/v1/auth/oidc/app");
        let cookie = first_cookie(&back, "notekpr_app=");
        let page = self
            .http
            .get(format!("{}/api/v1/auth/oidc/app", self.base))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(page.status(), 200);
        // Its form's post carries the page's origin (`no-referrer` would
        // make it `null`), and its answer may lead to the app, nowhere else.
        let h = page.headers();
        assert_eq!(h[header::REFERRER_POLICY], "same-origin");
        let csp = h[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(
            csp.ends_with("; form-action 'self' http://127.0.0.1:4321"),
            "{csp}"
        );
        (page.text().await.unwrap(), cookie)
    }

    fn app_confirm(
        &self,
        cookie: Option<&str>,
        form: &str,
        approve: &str,
    ) -> reqwest::RequestBuilder {
        let mut r = self
            .http
            .post(format!("{}/api/v1/auth/oidc/app", self.base))
            .form(&[("approve", approve), ("form", form)]);
        if let Some(c) = cookie {
            r = r.header(header::COOKIE, c);
        }
        r
    }

    /// An app's sign-in in the browser, confirmed: where the browser is
    /// sent back to.
    async fn app_login(&self, challenge: &str) -> String {
        let (page, cookie) = self.app_to_confirm(challenge).await;
        location(
            &self
                .app_confirm(Some(&cookie), &form_token(&page), "yes")
                .send()
                .await
                .unwrap(),
        )
    }

    async fn exchange_app(&self, back: &str, verifier: Option<&str>) -> reqwest::Response {
        let code = back
            .strip_prefix(&format!("{APP_BACK}?code="))
            .unwrap_or_else(|| panic!("no code in {back}"))
            .to_string();
        self.http
            .post(format!("{}/api/v1/auth/exchange", self.base))
            .json(&ExchangeRequest {
                code,
                code_verifier: verifier.map(str::to_string),
            })
            .send()
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn apps_sign_in_through_the_browser_and_get_back() {
    let s = setup(vec!["client_secret_basic"], true, true).await;
    let info: Value = s
        .http
        .get(format!("{}/api/v1/info", s.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["app_sign_in"], true);
    let (verifier, challenge) = pkce(1);

    // The browser goes back to the app, with the code in the query.
    let back = s.app_login(&challenge).await;
    let r = s.exchange_app(&back, Some(&verifier)).await;
    assert_eq!(r.status(), 200);
    let session: SessionResponse = r.json().await.unwrap();
    assert_eq!(session.user.display_name, "Olivia");
    let sessions: Value = s
        .http
        .get(format!("{}/api/v1/me/sessions", s.base))
        .bearer_auth(&session.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        sessions.to_string().contains("\"desktop\""),
        "labelled as the desktop app's: {sessions}"
    );

    // Without the verifier, or with another one, the code is useless — and
    // used up: the right verifier afterwards doesn't bring it back.
    let back = s.app_login(&challenge).await;
    assert_eq!(s.exchange_app(&back, None).await.status(), 401);
    assert_eq!(s.exchange_app(&back, Some(&verifier)).await.status(), 401);
    let back = s.app_login(&challenge).await;
    let (other, _) = pkce(2);
    assert_eq!(s.exchange_app(&back, Some(&other)).await.status(), 401);
    assert_eq!(s.exchange_app(&back, Some("short")).await.status(), 401);

    // The web app's own sign-in is as before (a verifier changes nothing).
    let done = s.login(true).await;
    assert!(done.starts_with("/auth/complete#code="), "{done}");
    s.exchange(&done).await;
}

#[tokio::test]
async fn an_apps_address_must_be_its_own_loopback() {
    let s = setup(vec!["client_secret_basic"], true, true).await;
    let (_, challenge) = pkce(1);
    for bad in [
        "http://localhost:4321/callback",
        "https://127.0.0.1:4321/callback",
        "http://127.0.0.1/callback",
        "http://127.0.0.1:4321/callback?x=1",
        "http://127.0.0.1:4321/callback#x",
        "http://127.0.0.2:4321/callback",
        "http://user@127.0.0.1:4321/callback",
        "http://notes.example.com:4321/callback",
        "http://127.0.0.1:4321/a%20b",
        "http://127.0.0.1:4321/a.b",
        "//127.0.0.1:4321/callback",
        "notekpr://callback",
    ] {
        let r = s.app_start(bad, &challenge).send().await.unwrap();
        assert_eq!(r.status(), 400, "{bad}");
    }
    let r = s
        .app_start("http://[::1]:4321/callback", &challenge)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_redirection(), "IPv6 loopback");
    // The challenge goes with the address, as S256 only.
    let start = format!("{}/api/v1/auth/oidc/start", s.base);
    for q in [
        vec![("redirect_uri", APP_BACK)],
        vec![
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ],
        vec![
            ("redirect_uri", APP_BACK),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "plain"),
        ],
        vec![
            ("redirect_uri", APP_BACK),
            ("code_challenge", "too-short"),
            ("code_challenge_method", "S256"),
        ],
    ] {
        let r = s.http.get(&start).query(&q).send().await.unwrap();
        assert_eq!(r.status(), 400, "{q:?}");
    }
}

#[tokio::test]
async fn a_providers_error_goes_back_to_the_app_that_asked() {
    let s = setup(vec!["client_secret_basic"], true, true).await;
    let (_, challenge) = pkce(1);
    let error_for = |send_cookie: bool| {
        let s = &s;
        let challenge = challenge.clone();
        async move {
            let start = s.app_start(APP_BACK, &challenge).send().await.unwrap();
            let cookie = start.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_string();
            let to_idp = url::Url::parse(&location(&start)).unwrap();
            let state = to_idp
                .query_pairs()
                .find(|(k, _)| k == "state")
                .unwrap()
                .1
                .into_owned();
            let mut cb = s
                .http
                .get(format!("{}/api/v1/auth/oidc/callback", s.base))
                .query(&[("error", "access_denied"), ("state", state.as_str())]);
            if send_cookie {
                cb = cb.header(header::COOKIE, cookie);
            }
            location(&cb.send().await.unwrap())
        }
    };
    assert_eq!(
        error_for(true).await,
        format!("{APP_BACK}?error=access_denied")
    );
    // A browser that didn't start it is told on the server's own page.
    assert_eq!(error_for(false).await, "/auth/complete#error=access_denied");
}

#[tokio::test]
async fn only_the_desktop_app_may_call_the_api_from_another_origin() {
    let s = setup(vec!["client_secret_basic"], true, true).await;
    let preflight = |origin: &'static str| {
        s.http
            .request(reqwest::Method::OPTIONS, format!("{}/api/v1/me", s.base))
            .header(header::ORIGIN, origin)
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
            .header(
                header::ACCESS_CONTROL_REQUEST_HEADERS,
                "authorization, notekpr-app",
            )
            .send()
    };
    for origin in [
        "tauri://localhost",
        "http://tauri.localhost",
        "https://tauri.localhost",
    ] {
        let r = preflight(origin).await.unwrap();
        assert!(r.status().is_success(), "{origin}: {}", r.status());
        assert_eq!(r.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], origin);
        let allowed = r.headers()[header::ACCESS_CONTROL_ALLOW_HEADERS]
            .to_str()
            .unwrap()
            .to_ascii_lowercase();
        assert!(allowed.contains("authorization") && allowed.contains("notekpr-app"));
        assert!(
            !r.headers()
                .contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
        );
    }
    for origin in [
        "https://evil.example",
        "null",
        "tauri://evil",
        "http://tauri.localhost:8080",
    ] {
        let r = preflight(origin).await.unwrap();
        assert!(
            !r.headers()
                .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            "{origin}"
        );
    }
    // Answers say which origin they were for, and show Retry-After.
    let r = s
        .http
        .get(format!("{}/api/v1/info", s.base))
        .header(header::ORIGIN, "tauri://localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "tauri://localhost"
    );
    assert!(
        r.headers()[header::VARY]
            .to_str()
            .unwrap()
            .to_ascii_lowercase()
            .contains("origin")
    );
    assert!(
        r.headers()[header::ACCESS_CONTROL_EXPOSE_HEADERS]
            .to_str()
            .unwrap()
            .to_ascii_lowercase()
            .contains("retry-after")
    );
}

#[tokio::test]
async fn an_apps_sign_in_waits_for_the_person_to_confirm_its_code() {
    let s = setup(vec!["client_secret_basic"], true, true).await;
    let (verifier, challenge) = pkce(1);
    let code = notekpr_core::keys::app_sign_in_code(&challenge);

    // The page shows the code the app derives from its own challenge.
    let (page, cookie) = s.app_to_confirm(&challenge).await;
    assert!(page.contains(&code), "{page}");
    assert!(page.contains("method=\"post\""));
    let form = form_token(&page);
    // No other browser (no cookie) can confirm it; nor can a form from
    // another page of the same site, which the cookie reaches but which
    // can't read the token, or from another origin — and their posts use
    // nothing up.
    let r = s.app_confirm(None, &form, "yes").send().await.unwrap();
    assert_eq!(r.status(), 400);
    for bad in ["", "not-the-token"] {
        let r = s
            .app_confirm(Some(&cookie), bad, "yes")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400, "form token {bad:?}");
    }
    let r = s
        .app_confirm(Some(&cookie), &form, "yes")
        .header(header::ORIGIN, "https://sibling.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400, "another origin");
    // Cancelled: back to the app, without a code — and used up.
    let r = s
        .app_confirm(Some(&cookie), &form, "no")
        .header(header::ORIGIN, s.base.as_str())
        .send()
        .await
        .unwrap();
    assert_eq!(location(&r), format!("{APP_BACK}?error=access_denied"));
    let r = s
        .app_confirm(Some(&cookie), &form, "yes")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400, "single use");

    // Continued: back to the app with a code its verifier swaps.
    let back = s.app_login(&challenge).await;
    assert_eq!(s.exchange_app(&back, Some(&verifier)).await.status(), 200);

    // A sign-in link someone else made (their loopback address, their
    // challenge) only gets as far as the page, showing their code — not the
    // one the person's app shows, if it shows one at all.
    let (_, attackers) = pkce(9);
    let (page, _) = s.app_to_confirm(&attackers).await;
    assert!(page.contains(&notekpr_core::keys::app_sign_in_code(&attackers)));
    assert!(!page.contains(&code));
}
