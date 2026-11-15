//! Interactive login: OIDC authorization code flow, the explicit development
//! login, logout and session management.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use notekpr_core::protocol::{
    DEV_EMAIL_DOMAIN, DEV_ISSUER, DevLoginRequest, ExchangeRequest, ServerInfo, SessionEntry,
    SessionResponse,
};
use serde::Deserialize;

use crate::auth::{self, Principal};
use crate::db;
use crate::error::{AppError, AppResult, bad};
use crate::hub::Control;
use crate::repo;
use crate::state::AppState;

const LOGIN_CODE_TTL_MS: i64 = 120_000;
const OIDC_STATE_TTL_MS: i64 = 600_000;

pub async fn info(State(state): State<AppState>) -> Json<ServerInfo> {
    let settings = state.instance.settings();
    Json(ServerInfo {
        name: "notekpr".into(),
        version: notekpr_core::VERSION.into(),
        dev_mode: state.config.dev_mode,
        oidc_enabled: state.oidc.is_some(),
        oidc_display_name: state
            .oidc
            .as_ref()
            .map(|_| state.config.oidc_display_name.clone()),
        max_attachment_bytes: settings.max_attachment_bytes(),
        max_update_bytes: settings.max_update_bytes(),
        note_keys: true,
        max_snapshot_bytes: settings.max_snapshot_bytes(),
        read_only: state.instance.read_only(),
        purge_undo_days: settings.purge_undo_days,
        // Apps sign in through OIDC (`/auth/oidc/app`); development mode
        // has its own sign-in.
        app_sign_in: state.oidc.is_some(),
        demo_resets_at: state
            .config
            .demo_resets_at
            .filter(|_| state.config.dev_mode)
            .map(|s| s * 1000),
    })
}

#[derive(Deserialize)]
pub struct StartQuery {
    return_to: Option<String>,
    /// An app's sign-in (protocol.md §3, "Apps"): where the browser goes
    /// back to, and the PKCE challenge the code is bound to.
    redirect_uri: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
}

/// An app's loopback address (RFC 8252 §7.3): `http://127.0.0.1:<port>/…`
/// or `http://[::1]:<port>/…` — IP literals only, so no name can point it
/// elsewhere —, with a plain path and nothing else.
fn app_redirect(uri: &str) -> Option<String> {
    let u = url::Url::parse(uri).ok()?;
    let loopback = match u.host()? {
        url::Host::Ipv4(ip) => ip == std::net::Ipv4Addr::LOCALHOST,
        url::Host::Ipv6(ip) => ip == std::net::Ipv6Addr::LOCALHOST,
        url::Host::Domain(_) => false,
    };
    let path = u.path();
    let plain = path.len() <= 128
        && path.starts_with('/')
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-".contains(&b));
    (u.scheme() == "http"
        && loopback
        && u.port().is_some()
        && u.username().is_empty()
        && u.password().is_none()
        && u.query().is_none()
        && u.fragment().is_none()
        && plain)
        .then(|| u.to_string())
}

/// An S256 challenge: base64url of a SHA-256 (RFC 7636 §4.2).
fn valid_challenge(c: &str) -> bool {
    c.len() == 43
        && c.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A PKCE verifier (RFC 7636 §4.1) whose S256 is `challenge`.
fn verifier_matches(verifier: &str, challenge: &str) -> bool {
    use subtle::ConstantTimeEq;
    let valid = (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b));
    if !valid {
        return false;
    }
    use sha2::Digest;
    let computed =
        data_encoding::BASE64URL_NOPAD.encode(&sha2::Sha256::digest(verifier.as_bytes()));
    bool::from(computed.as_bytes().ct_eq(challenge.as_bytes()))
}

/// Only same-origin absolute paths; blocks open redirects like `//evil.com`.
fn safe_return_to(r: Option<String>) -> String {
    match r {
        Some(r)
            if r.starts_with('/') && !r.starts_with("//") && !r.contains('\\') && r.len() < 512 =>
        {
            r
        }
        _ => "/".to_string(),
    }
}

/// Cookie that ties an OIDC login to the browser that started it, so a
/// login link prepared by someone else can't sign this browser in as them.
const LOGIN_COOKIE: &str = "notekpr_login";

fn login_cookie(state: &AppState, value: &str, max_age_secs: i64) -> HeaderValue {
    auth_cookie(state, LOGIN_COOKIE, value, max_age_secs)
}

fn auth_cookie(state: &AppState, name: &str, value: &str, max_age_secs: i64) -> HeaderValue {
    let secure = if state.config.public_url().starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{name}={value}; Path=/api/v1/auth/oidc; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}{secure}"
    ))
    .expect("cookie is ASCII")
}

/// Cookie holding an app's sign-in that waits for the person's
/// confirmation ([`app_confirm_page`]): only this browser can confirm it,
/// and no other site can post the confirmation (SameSite).
const APP_COOKIE: &str = "notekpr_app";
/// How long an app's sign-in waits for its confirmation.
const APP_CONFIRM_TTL_MS: i64 = 600_000;

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|kv| kv.trim().strip_prefix(name)?.strip_prefix('='))
}

pub async fn oidc_start(
    State(state): State<AppState>,
    Query(q): Query<StartQuery>,
) -> AppResult<Response> {
    let oidc = state.oidc.as_ref().ok_or(AppError::NotFound)?;
    let (app_redirect, code_challenge) = match (q.redirect_uri, q.code_challenge) {
        (None, None) => (None, None),
        (Some(uri), Some(c)) => {
            let uri = app_redirect(&uri).ok_or_else(|| {
                bad("redirect_uri must be http://127.0.0.1:<port>/<path> or http://[::1]:<port>/<path>")
            })?;
            if q.code_challenge_method.as_deref() != Some("S256") || !valid_challenge(&c) {
                return Err(bad("an app's sign-in needs an S256 code_challenge"));
            }
            (Some(uri), Some(c))
        }
        _ => return Err(bad("redirect_uri and code_challenge go together")),
    };
    let start = oidc.start().await?;
    let now = db::now();
    repo::sessions::delete_expired_oidc_states(&state.db, now).await?;
    let (browser_secret, browser_hash) = db::new_secret("");
    repo::sessions::insert_oidc_state(
        &state.db,
        &start.state,
        &repo::sessions::OidcState {
            pkce_verifier: start.pkce_verifier,
            nonce: start.nonce,
            return_to: safe_return_to(q.return_to),
            expires_at: now + OIDC_STATE_TTL_MS,
            browser_hash: Some(browser_hash),
            app_redirect,
            code_challenge,
        },
    )
    .await?;
    let mut resp = Redirect::to(&start.url).into_response();
    resp.headers_mut().append(
        header::SET_COOKIE,
        login_cookie(&state, &browser_secret, OIDC_STATE_TTL_MS / 1000),
    );
    Ok(resp)
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

fn complete_redirect(fragment: &str) -> Response {
    Redirect::to(&format!("/auth/complete#{fragment}")).into_response()
}

/// Back to where the sign-in started: the web app (`/auth/complete#…`), or
/// an app's loopback address (`…?…`: a fragment never reaches its server).
fn back_to(row: Option<&repo::sessions::OidcState>, params: &str) -> Response {
    match row.and_then(|r| r.app_redirect.as_deref()) {
        Some(app) => Redirect::to(&format!("{app}?{params}")).into_response(),
        None => complete_redirect(params),
    }
}

pub async fn oidc_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> AppResult<Response> {
    let browser = cookie_value(&headers, LOGIN_COOKIE).map(|v| db::sha256(v.as_bytes()));
    let mut resp = oidc_callback_inner(&state, browser, q).await?;
    // The cookie is single-use: always clear it.
    resp.headers_mut()
        .append(header::SET_COOKIE, login_cookie(&state, "", 0));
    Ok(resp)
}

async fn oidc_callback_inner(
    state: &AppState,
    browser: Option<Vec<u8>>,
    q: CallbackQuery,
) -> AppResult<Response> {
    let oidc = state.oidc.as_ref().ok_or(AppError::NotFound)?;
    // States are single-use, also when the provider reports an error: the
    // state says where to report it (an app's loopback address or the web
    // app), once.
    let row = match &q.state {
        Some(st) => repo::sessions::take_oidc_state(&state.db, st).await?,
        None => None,
    };
    // Only the browser that started this login may finish it.
    let same_browser = match (row.as_ref().and_then(|r| r.browser_hash.as_ref()), &browser) {
        (Some(e), Some(b)) => {
            use subtle::ConstantTimeEq;
            bool::from(e.ct_eq(b))
        }
        _ => false,
    };
    if let Some(err) = q.error {
        let err: String = err
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(64)
            .collect();
        // To an app only when this browser started its sign-in.
        let row = row.as_ref().filter(|_| same_browser);
        return Ok(back_to(row, &format!("error={err}")));
    }
    let Some(code) = q.code else {
        return Err(bad("missing code or state"));
    };
    let row = row.ok_or_else(|| bad("unknown or already used login state"))?;
    if !same_browser {
        tracing::warn!("OIDC callback from a browser that didn't start the login");
        return Ok(complete_redirect("error=login_failed"));
    }
    if row.expires_at < db::now() {
        return Ok(back_to(Some(&row), "error=login_expired"));
    }
    let identity = match oidc
        .finish(code, row.pkce_verifier.clone(), row.nonce.clone())
        .await
    {
        Ok(i) => i,
        Err(AppError::Unauthorized) => return Ok(back_to(Some(&row), "error=login_failed")),
        Err(e) => return Err(e),
    };
    let user_id = match auth::provision_user(
        state,
        &identity.issuer,
        &identity.subject,
        &identity.display_name,
        identity.email.as_deref(),
    )
    .await
    {
        Ok(u) => u,
        Err(AppError::Forbidden(_)) => return Ok(back_to(Some(&row), "error=account_disabled")),
        Err(AppError::LimitReached(_) | AppError::ReadOnly(_)) => {
            return Ok(back_to(Some(&row), "error=no_new_accounts"));
        }
        Err(e) => return Err(e),
    };
    // An app's sign-in waits for the person to confirm it, comparing the
    // code the app shows: a sign-in link someone else made (with their own
    // loopback address and challenge) goes nowhere without that.
    if let (Some(app_redirect), Some(code_challenge)) = (row.app_redirect, row.code_challenge) {
        let now = db::now();
        repo::sessions::delete_expired_app_sign_ins(&state.db, now).await?;
        let (token, hash) = db::new_secret("notekpr_a_");
        repo::sessions::insert_app_sign_in(
            &state.db,
            &hash,
            &repo::sessions::AppSignIn {
                user_id,
                app_redirect,
                code_challenge,
                expires_at: now + APP_CONFIRM_TTL_MS,
            },
        )
        .await?;
        let mut resp = Redirect::to("/api/v1/auth/oidc/app").into_response();
        resp.headers_mut().append(
            header::SET_COOKIE,
            auth_cookie(state, APP_COOKIE, &token, APP_CONFIRM_TTL_MS / 1000),
        );
        return Ok(resp);
    }
    let (code, hash) = db::new_secret("notekpr_l_");
    repo::sessions::insert_login_code(
        &state.db,
        &hash,
        &user_id,
        db::now() + LOGIN_CODE_TTL_MS,
        None,
    )
    .await?;
    let encoded: String = url::form_urlencoded::byte_serialize(row.return_to.as_bytes()).collect();
    Ok(complete_redirect(&format!(
        "code={code}&return_to={encoded}"
    )))
}

/// The page confirming an app's sign-in, in the browser that signed in:
/// the code the app shows, and the person's choice. Nothing on it comes
/// from anyone but the server (no names: no markup from elsewhere on this
/// origin).
fn app_page(body: &str) -> Response {
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>notekpr</title>\
<style>body{{font:16px system-ui,sans-serif;display:grid;place-items:center;min-height:90vh;margin:0;\
color:#1f1d2b;background:#f7f7fb}}@media(prefers-color-scheme:dark){{body{{color:#e8e6f3;background:#1f1d2b}}}}\
main{{max-width:28rem;padding:1.5rem;text-align:center}}.code{{font:600 2rem ui-monospace,monospace;\
letter-spacing:.1em;margin:1rem 0}}button{{font:inherit;padding:.6rem 1.2rem;margin:.3rem;border-radius:.5rem;\
border:1px solid #8885;background:transparent;color:inherit;cursor:pointer}}button.yes{{background:#5b5bd6;\
color:#fff;border-color:#5b5bd6}}</style></head><body><main>{body}</main></body></html>"
    );
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

fn app_sign_in_over() -> Response {
    let mut r = app_page(
        "<h1>This sign-in is over</h1><p>It was confirmed or cancelled already, took too long, or \
         started in another browser. Start it again from the app.</p>",
    );
    *r.status_mut() = StatusCode::BAD_REQUEST;
    r
}

/// The confirmation form's token: derived from the browser's cookie, so
/// only a page that could read the form (this origin) can send it — a
/// sibling site, whose forms the cookie reaches too (SameSite is per
/// site), can neither read the page nor work the token out.
fn app_form_token(cookie: &str) -> String {
    let mut m = b"notekpr app sign-in form\0".to_vec();
    m.extend_from_slice(cookie.as_bytes());
    data_encoding::BASE64URL_NOPAD.encode(&db::sha256(&m))
}

/// A form posted from this server's own origin: an `Origin` naming
/// another is refused (browsers send it with every form post).
fn from_own_origin(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let own = url::Url::parse(&state.config.public_url())
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_default();
    origin.to_str().is_ok_and(|o| !own.is_empty() && o == own)
}

/// `GET /auth/oidc/app`: the code to compare with the app's, and the
/// person's choice (a form posted to the same address).
pub async fn app_confirm_page(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let Some(cookie) = cookie_value(&headers, APP_COOKIE) else {
        return Ok(app_sign_in_over());
    };
    let hash = db::sha256(cookie.as_bytes());
    let form = app_form_token(cookie);
    let Some(s) = repo::sessions::get_app_sign_in(&state.db, &hash).await? else {
        return Ok(app_sign_in_over());
    };
    if s.expires_at < db::now() {
        return Ok(app_sign_in_over());
    }
    let code = notekpr_core::keys::app_sign_in_code(&s.code_challenge);
    Ok(app_page(&format!(
        "<h1>Sign in to the notekpr app?</h1><p>The notekpr app on this computer asks to be signed in as you. \
         Continue only if it shows this code:</p><p class=\"code\">{code}</p><p>Didn't start a sign-in in the app, \
         or it shows another code? Cancel.</p><form method=\"post\"><input type=\"hidden\" name=\"form\" \
         value=\"{form}\"><button name=\"approve\" value=\"no\">Cancel</button><button class=\"yes\" name=\"approve\" value=\"yes\">Continue</button></form>"
    )))
}

#[derive(Deserialize)]
pub struct AppConfirm {
    approve: String,
    #[serde(default)]
    form: String,
}

/// `POST /auth/oidc/app`: the person's choice. Continue sends the browser
/// back to the app with a one-time code bound to its challenge; Cancel
/// sends it back with `access_denied`. Either way the sign-in is used up —
/// but only by the page's own form (its token, this origin): a post from
/// anywhere else changes nothing.
pub async fn app_confirm(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Form(f): axum::Form<AppConfirm>,
) -> AppResult<Response> {
    let Some(cookie) = cookie_value(&headers, APP_COOKIE) else {
        return Ok(app_sign_in_over());
    };
    let own_form = {
        use subtle::ConstantTimeEq;
        bool::from(app_form_token(cookie).as_bytes().ct_eq(f.form.as_bytes()))
    };
    if !own_form || !from_own_origin(&state, &headers) {
        return Ok(app_sign_in_over());
    }
    let taken = repo::sessions::take_app_sign_in(&state.db, &db::sha256(cookie.as_bytes())).await?;
    let mut resp = match taken {
        Some(s) if s.expires_at >= db::now() => {
            if f.approve == "yes" {
                let (code, hash) = db::new_secret("notekpr_l_");
                repo::sessions::insert_login_code(
                    &state.db,
                    &hash,
                    &s.user_id,
                    db::now() + LOGIN_CODE_TTL_MS,
                    Some(&s.code_challenge),
                )
                .await?;
                Redirect::to(&format!("{}?code={code}", s.app_redirect)).into_response()
            } else {
                Redirect::to(&format!("{}?error=access_denied", s.app_redirect)).into_response()
            }
        }
        _ => app_sign_in_over(),
    };
    resp.headers_mut()
        .append(header::SET_COOKIE, auth_cookie(&state, APP_COOKIE, "", 0));
    Ok(resp)
}

async fn session_response(
    state: &AppState,
    user_id: &str,
    kind: &str,
    client: Option<&str>,
) -> AppResult<SessionResponse> {
    let s = auth::create_session(
        &state.db,
        user_id,
        kind,
        client,
        state.instance.settings().session_ttl_ms(),
    )
    .await?;
    Ok(SessionResponse {
        token: s.token,
        expires_at: s.expires_at,
        user: auth::user_info(state, user_id).await?,
    })
}

/// Swap the one-time code from the OIDC callback for a session token. A
/// code given out for an app needs the verifier of its PKCE challenge; it
/// is used up either way.
pub async fn exchange(
    State(state): State<AppState>,
    Json(req): Json<ExchangeRequest>,
) -> AppResult<Json<SessionResponse>> {
    let hash = db::sha256(req.code.as_bytes());
    let (user_id, expires, challenge) = repo::sessions::take_login_code(&state.db, &hash)
        .await?
        .ok_or(AppError::Unauthorized)?;
    if expires < db::now() {
        return Err(AppError::Unauthorized);
    }
    let client = match &challenge {
        Some(c) => match &req.code_verifier {
            Some(v) if verifier_matches(v, c) => "desktop",
            _ => return Err(AppError::Unauthorized),
        },
        None => "browser",
    };
    Ok(Json(
        session_response(&state, &user_id, "web", Some(client)).await?,
    ))
}

fn valid_dev_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}

/// DEVELOPMENT ONLY. Signs in as any user name. Disabled unless `--dev-mode`.
pub async fn dev_login(
    State(state): State<AppState>,
    Json(req): Json<DevLoginRequest>,
) -> AppResult<Json<SessionResponse>> {
    if !state.config.dev_mode {
        return Err(AppError::NotFound);
    }
    let username = req.username.trim().to_lowercase();
    if !valid_dev_name(&username) {
        return Err(bad(
            "user name must be 1-32 characters of a-z, 0-9, '.', '_' or '-'",
        ));
    }
    let display = req
        .display_name
        .map(|d| d.trim().chars().take(100).collect::<String>())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| {
            let mut c = username.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        });
    // An address of their own, to be invited by (no mail is ever sent).
    let email = format!("{username}@{DEV_EMAIL_DOMAIN}");
    let user_id =
        auth::provision_user(&state, DEV_ISSUER, &username, &display, Some(&email)).await?;
    Ok(Json(
        session_response(&state, &user_id, "web", Some("dev login")).await?,
    ))
}

pub async fn logout(State(state): State<AppState>, who: Principal) -> AppResult<StatusCode> {
    revoke_session(&state, &who.user_id, &who.session_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn revoke_session(state: &AppState, user_id: &str, session_id: &str) -> AppResult<()> {
    // Under the write lock, with closing its connections: a server taking
    // over from this one can't miss it (`writer.rs`).
    let _w = state.write_lock.lock().await?;
    if repo::sessions::revoke(&state.db, session_id, user_id, db::now()).await? == 0 {
        return Err(AppError::NotFound);
    }
    state
        .hub
        .control(user_id, Control::SessionRevoked(session_id.to_string()));
    Ok(())
}

pub async fn list_sessions(
    State(state): State<AppState>,
    who: Principal,
) -> AppResult<Json<Vec<SessionEntry>>> {
    Ok(Json(
        repo::sessions::list_active(&state.db, &who.user_id, &who.session_id, db::now()).await?,
    ))
}

pub async fn delete_session(
    State(state): State<AppState>,
    who: Principal,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    revoke_session(&state, &who.user_id, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
