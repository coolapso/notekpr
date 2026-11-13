//! Signing in through the system browser (RFC 8252): the app listens on a
//! random loopback port, opens the server's sign-in page in the default
//! browser, and the server sends the browser back to that port with a
//! one-time code — bound to this attempt by PKCE (RFC 7636), so the code is
//! useless to anyone who sees it without the verifier, which never leaves
//! this process (protocol.md §3, "Apps").

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use axum::Router;
use axum::extract::Query;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use data_encoding::BASE64URL_NOPAD;
use notekpr_cli::api::{Api, Credentials};
use notekpr_core::protocol::{ExchangeRequest, SessionResponse};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

/// How long a sign-in in the browser may take.
pub const TIMEOUT: Duration = Duration::from_secs(600);

/// A PKCE verifier (43 characters) and its S256 challenge.
pub fn pkce() -> (Zeroizing<String>, String) {
    let verifier =
        Zeroizing::new(BASE64URL_NOPAD.encode(&notekpr_core::crypto::random_bytes::<32>()));
    let challenge = BASE64URL_NOPAD.encode(&Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// The sign-in page's address for an app listening at `redirect_uri`.
pub fn start_url(server: &str, redirect_uri: &str, challenge: &str) -> String {
    let mut u = url::Url::parse(&format!("{server}/api/v1/auth/oidc/start"))
        .expect("a normalized server URL");
    u.query_pairs_mut()
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256");
    u.into()
}

#[derive(Deserialize)]
struct Back {
    code: Option<String>,
    error: Option<String>,
}

const DONE_PAGE: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>notekpr</title>\
<style>body{font:16px system-ui,sans-serif;display:grid;place-items:center;min-height:90vh;\
margin:0;color:#1f1d2b;background:#f7f7fb}@media(prefers-color-scheme:dark){body{color:#e8e6f3;\
background:#1f1d2b}}main{text-align:center;padding:1rem}</style></head><body><main>\
<h1>Back to notekpr</h1><p>You can close this tab: the app takes it from here.</p></main></body></html>";

/// What the server's error codes mean to the person.
fn error_text(code: &str) -> String {
    match code {
        "account_disabled" => "This account has been disabled by an administrator.".into(),
        "access_denied" => "Sign-in was cancelled.".into(),
        "login_expired" => "The sign-in took too long. Please try again.".into(),
        "no_new_accounts" => "This server isn't accepting new accounts right now.".into(),
        _ => "Sign-in failed. Please try again.".into(),
    }
}

/// Sign in to `server` (normalized, without `/api/v1`): `open` shows the
/// sign-in page in the default browser; `show_code` gets the code the
/// server's page asks the person to compare before it sends the browser
/// back (`notekpr_core::keys::app_sign_in_code`). Ends with the session, or
/// an error when it failed, timed out or `cancel` fired.
pub async fn sign_in(
    server: &str,
    open: impl FnOnce(&str) -> Result<()>,
    show_code: impl FnOnce(&str),
    cancel: oneshot::Receiver<()>,
) -> Result<SessionResponse> {
    // IPv4's loopback, or IPv6's where there is none (RFC 8252 §8.3).
    let listener =
        match tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await {
            Ok(l) => l,
            Err(_) => tokio::net::TcpListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, 0)))
                .await
                .context("listening for the browser's return")?,
        };
    let host = match listener.local_addr()? {
        SocketAddr::V4(a) => format!("127.0.0.1:{}", a.port()),
        SocketAddr::V6(a) => format!("[::1]:{}", a.port()),
    };
    let path = format!(
        "/callback/{}",
        BASE64URL_NOPAD.encode(&notekpr_core::crypto::random_bytes::<16>())
    );
    let redirect_uri = format!("http://{host}{path}");
    let (verifier, challenge) = pkce();
    show_code(&notekpr_core::keys::app_sign_in_code(&challenge));

    let (got_tx, got_rx) = oneshot::channel::<Back>();
    let got_tx = Arc::new(Mutex::new(Some(got_tx)));
    let app = Router::new()
        .route(
            &path,
            get(move |headers: HeaderMap, Query(back): Query<Back>| {
                let got_tx = got_tx.clone();
                let host = host.clone();
                async move {
                    // Only the browser coming back to this exact address: a
                    // web page reaching the port through another name (DNS
                    // rebinding) can't end the sign-in.
                    if headers.get(header::HOST).and_then(|h| h.to_str().ok())
                        != Some(host.as_str())
                    {
                        return StatusCode::NOT_FOUND.into_response();
                    }
                    // The first return counts; a reload of the tab changes nothing.
                    if let Some(tx) = got_tx.lock().unwrap().take() {
                        let _ = tx.send(back);
                    }
                    (
                        [
                            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                            (header::CACHE_CONTROL, "no-store"),
                            (header::REFERRER_POLICY, "no-referrer"),
                        ],
                        DONE_PAGE,
                    )
                        .into_response()
                }
            }),
        )
        .fallback(|| async { StatusCode::NOT_FOUND.into_response() });
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let server_task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await;
    });

    // Cancelling (or a new attempt) ends this one at any point, the
    // exchange included: no session comes out of an attempt given up.
    let mut cancel = cancel;
    let result = async {
        open(&start_url(server, &redirect_uri, &challenge))?;
        let back = tokio::select! {
            b = got_rx => b.map_err(|_| anyhow!("the sign-in stopped"))?,
            _ = &mut cancel => bail!("cancelled"),
            _ = tokio::time::sleep(TIMEOUT) => bail!("The sign-in took too long. Please try again."),
        };
        if let Some(e) = back.error {
            bail!("{}", error_text(&e));
        }
        let code = back.code.ok_or_else(|| anyhow!("{}", error_text("")))?;
        let api = Api::new(server, Credentials::None)?;
        let request = ExchangeRequest {
            code,
            code_verifier: Some(verifier.to_string()),
        };
        let exchange = api.post::<_, SessionResponse>("/auth/exchange", &request);
        let session = tokio::select! {
            r = exchange => r.map_err(|e| anyhow!("{}: {e}", error_text("")))?,
            _ = &mut cancel => bail!("cancelled"),
        };
        if cancel.try_recv().is_ok() {
            bail!("cancelled");
        }
        Ok(session)
    }
    .await;
    // Give the browser a moment to receive its page, then stop listening.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let _ = stop_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), server_task).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_pairs() {
        let (v, c) = pkce();
        assert_eq!(v.len(), 43);
        assert_eq!(c.len(), 43);
        assert_eq!(c, BASE64URL_NOPAD.encode(&Sha256::digest(v.as_bytes())));
        assert_ne!(pkce().0, v);
    }

    #[test]
    fn start_urls() {
        let u = start_url(
            "https://n.example.com/sub",
            "http://127.0.0.1:4321/callback/abc",
            "xyz",
        );
        assert_eq!(
            u,
            "https://n.example.com/sub/api/v1/auth/oidc/start?redirect_uri=http%3A%2F%2F127.0.0.1%3A4321%2Fcallback%2Fabc&code_challenge=xyz&code_challenge_method=S256"
        );
    }

    #[tokio::test]
    async fn the_browser_comes_back() {
        // A "browser" that returns with an error: the sign-in ends with its
        // meaning, and only the right path is answered.
        let (_cancel_tx, cancel) = oneshot::channel();
        let shown = std::sync::Arc::new(Mutex::new(None::<String>));
        let s2 = shown.clone();
        let result = sign_in(
            "https://n.example.com",
            |url| {
                let u = url::Url::parse(url).unwrap();
                let back = u
                    .query_pairs()
                    .find(|(k, _)| k == "redirect_uri")
                    .unwrap()
                    .1
                    .into_owned();
                tokio::spawn(async move {
                    let http = reqwest::Client::new();
                    let wrong = back.rsplit_once('/').unwrap().0.to_string() + "/nope";
                    assert_eq!(http.get(&wrong).send().await.unwrap().status(), 404);
                    let rebound = http
                        .get(format!("{back}?error=access_denied"))
                        .header("host", "attacker.example")
                        .send()
                        .await
                        .unwrap();
                    assert_eq!(rebound.status(), 404, "another name for the address");
                    let r = http
                        .get(format!("{back}?error=access_denied"))
                        .send()
                        .await
                        .unwrap();
                    assert_eq!(r.status(), 200);
                });
                Ok(())
            },
            move |code| *s2.lock().unwrap() = Some(code.to_string()),
            cancel,
        )
        .await;
        assert_eq!(result.unwrap_err().to_string(), "Sign-in was cancelled.");
        let code = shown.lock().unwrap().clone().unwrap();
        assert_eq!(code.len(), 9, "the code to compare is shown: {code}");
    }

    #[tokio::test]
    async fn cancelling() {
        let (cancel_tx, cancel) = oneshot::channel();
        cancel_tx.send(()).unwrap();
        let r = sign_in("https://n.example.com", |_| Ok(()), |_| {}, cancel).await;
        assert_eq!(r.unwrap_err().to_string(), "cancelled");
    }
}
