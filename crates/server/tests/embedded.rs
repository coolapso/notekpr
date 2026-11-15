//! The desktop app's own server (`notekpr_server::embedded`, "Only on this
//! computer"): the app signs itself in and nothing else can, it answers
//! only requests to its own address and not from web pages, and what it
//! keeps survives a new port.

use notekpr_core::protocol::*;
use notekpr_server::embedded::{Embedded, bind};
use reqwest::Method;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::common::Api;

async fn start(dir: &std::path::Path) -> (Embedded, Api) {
    let server = Embedded::start(dir.to_path_buf(), bind().unwrap())
        .await
        .unwrap();
    let api = Api::new(&format!("{}/api/v1", server.origin()));
    (server, api)
}

#[tokio::test]
async fn the_app_signs_itself_in_and_nothing_else_can() {
    let dir = tempfile::tempdir().unwrap();
    let (server, mut api) = start(dir.path()).await;
    assert!(server.origin().starts_with("http://127.0.0.1:"));

    let info: ServerInfo = api.get("/info").await;
    assert!(!info.app_sign_in && !info.dev_mode && !info.oidc_enabled);
    assert_eq!(api.status::<()>(Method::GET, "/me", None).await, 401);

    // Every way to sign in from outside is gone, as for unknown paths.
    let dev = DevLoginRequest {
        username: "alice".into(),
        display_name: None,
    };
    assert_eq!(
        api.status(Method::POST, "/auth/dev-login", Some(&dev))
            .await,
        404
    );
    for (m, path) in [
        (Method::GET, "/auth/oidc/start"),
        (Method::GET, "/auth/oidc/callback"),
        (Method::GET, "/auth/oidc/app"),
        (Method::POST, "/auth/exchange"),
        (Method::POST, "/oauth/device_authorization"),
        (Method::POST, "/oauth/token"),
        (Method::GET, "/oauth/device"),
    ] {
        assert_eq!(api.status::<()>(m, path, None).await, 404, "{path}");
    }

    // The app's own.
    let s = server.session("Ana").await.unwrap();
    assert!(!s.user.is_admin);
    api.token = Some(s.token.clone());
    let me: MeResponse = api.get("/me").await;
    assert_eq!(me.user.id, s.user.id);
    assert_eq!(me.user.display_name, "Ana");

    // No agents, no administration, no metrics, even with a session.
    for path in ["/agents", "/admin/stats", "/admin/settings", "/admin/users"] {
        assert_eq!(
            api.status::<()>(Method::GET, path, None).await,
            404,
            "{path}"
        );
    }
    let raw = reqwest::Client::new();
    let get = |path: &str| raw.get(format!("{}{path}", server.origin())).send();
    assert_eq!(get("/metrics").await.unwrap().status(), 404);
    assert_eq!(get("/").await.unwrap().status(), 404);
    assert_eq!(get("/healthz").await.unwrap().status(), 200);

    // Another session is the same account's.
    let again = server.session("Ana").await.unwrap();
    assert_eq!(again.user.id, s.user.id);
    assert_ne!(again.token, s.token);
    let sessions: Vec<SessionEntry> = api.get("/me/sessions").await;
    assert_eq!(sessions.len(), 2);
    assert!(
        sessions
            .iter()
            .all(|x| x.client_name.as_deref() == Some("this computer"))
    );
    server.stop().await;
}

#[tokio::test]
async fn only_requests_to_its_own_address_and_not_from_web_pages() {
    let dir = tempfile::tempdir().unwrap();
    let (server, _) = start(dir.path()).await;
    let origin = server.origin().to_string();
    let port = origin.rsplit(':').next().unwrap().to_string();
    let raw = reqwest::Client::new();
    let info = |host: Option<&str>, from: Option<&str>| {
        let mut r = raw.get(format!("{origin}/api/v1/info"));
        if let Some(h) = host {
            r = r.header("Host", h);
        }
        if let Some(o) = from {
            r = r.header("Origin", o);
        }
        async move { r.send().await.unwrap().status().as_u16() }
    };

    assert_eq!(info(None, None).await, 200);
    assert_eq!(info(None, Some("tauri://localhost")).await, 200);
    assert_eq!(info(None, Some("https://tauri.localhost")).await, 200);
    // DNS rebinding: a name of someone else's that resolves to 127.0.0.1.
    assert_eq!(info(Some(&format!("evil.example:{port}")), None).await, 421);
    assert_eq!(info(Some(&format!("localhost:{port}")), None).await, 421);
    assert_eq!(info(Some("127.0.0.1"), None).await, 421);
    assert_eq!(info(Some("127.0.0.1:1"), None).await, 421);
    // A web page in a browser, at the right address.
    assert_eq!(info(None, Some("https://evil.example")).await, 403);
    assert_eq!(info(None, Some("null")).await, 403);
    assert_eq!(info(None, Some(&origin)).await, 403);
    // The same for everything outside the API.
    let healthz = raw
        .get(format!("{origin}/healthz"))
        .header("Host", format!("evil.example:{port}"))
        .send()
        .await
        .unwrap();
    assert_eq!(healthz.status(), 421);

    // The live connection: from a web page refused; from the app, or a
    // program, it opens (and needs a session's token in its first message).
    let ws = format!("ws://127.0.0.1:{port}/api/v1/ws");
    let from = |o: &str| {
        let mut r = ws.as_str().into_client_request().unwrap();
        r.headers_mut().insert("Origin", o.parse().unwrap());
        r
    };
    match tokio_tungstenite::connect_async(from("https://evil.example")).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => assert_eq!(r.status(), 403),
        other => panic!("a web page's WebSocket: {:?}", other.map(|_| ())),
    }
    tokio_tungstenite::connect_async(from("tauri://localhost"))
        .await
        .unwrap();
    tokio_tungstenite::connect_async(ws.as_str()).await.unwrap();
    server.stop().await;
}

#[tokio::test]
async fn what_it_keeps_outlives_its_port() {
    let dir = tempfile::tempdir().unwrap();
    let (server, mut api) = start(dir.path()).await;
    let s = server.session("Ana").await.unwrap();
    api.token = Some(s.token.clone());
    let prefs = PutPrefsRequest {
        ciphertext: b"sealed".to_vec(),
        expected_version: 0,
    };
    let _: PrefsBlob = api.put("/me/prefs", &prefs).await;
    let first = server.origin().to_string();
    server.stop().await;

    let (server, mut api) = start(dir.path()).await;
    assert_ne!(server.origin(), first, "a port of its own at each start");
    // The window's session from before still works…
    api.token = Some(s.token.clone());
    let me: MeResponse = api.get("/me").await;
    assert_eq!(me.user.id, s.user.id);
    let kept: PrefsBlob = api.get("/me/prefs").await;
    assert_eq!(kept.ciphertext.as_deref(), Some(&b"sealed"[..]));
    // …and a new one is the same account's.
    assert_eq!(server.session("Ana").await.unwrap().user.id, s.user.id);
    server.stop().await;
}
