//! Interactive login: OIDC authorization code flow, the explicit development
//! login, logout and session management.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use coolpsonotes_core::protocol::{
    DEV_ISSUER, DevLoginRequest, ExchangeRequest, ServerInfo, SessionEntry, SessionResponse,
};
use serde::Deserialize;

use crate::auth::{self, Principal};
use crate::db;
use crate::error::{AppError, AppResult, bad};
use crate::hub::Control;
use crate::repo;
use crate::state::AppState;

const DAY_MS: i64 = 24 * 3600 * 1000;
const LOGIN_CODE_TTL_MS: i64 = 120_000;
const OIDC_STATE_TTL_MS: i64 = 600_000;

pub async fn info(State(state): State<AppState>) -> Json<ServerInfo> {
    Json(ServerInfo {
        name: "coolpsonotes".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        dev_mode: state.config.dev_mode,
        oidc_enabled: state.oidc.is_some(),
        oidc_display_name: state
            .oidc
            .as_ref()
            .map(|_| state.config.oidc_display_name.clone()),
        max_attachment_bytes: state.config.max_attachment_bytes(),
        max_update_bytes: state.config.max_update_bytes(),
        note_keys: true,
        max_snapshot_bytes: state.config.max_snapshot_bytes(),
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
const LOGIN_COOKIE: &str = "cpn_login";

fn login_cookie(state: &AppState, value: &str, max_age_secs: i64) -> HeaderValue {
    let secure = if state.config.public_url().starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{LOGIN_COOKIE}={value}; Path=/api/v1/auth/oidc; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}{secure}"
    ))
    .expect("cookie is ASCII")
}

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
    if let Some(err) = q.error {
        let err: String = err
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(64)
            .collect();
        return Ok(complete_redirect(&format!("error={err}")));
    }
    let (Some(code), Some(st)) = (q.code, q.state) else {
        return Err(bad("missing code or state"));
    };
    // States are single-use.
    let row = repo::sessions::take_oidc_state(&state.db, &st)
        .await?
        .ok_or_else(|| bad("unknown or already used login state"))?;
    if row.expires_at < db::now() {
        return Ok(complete_redirect("error=login_expired"));
    }
    // Only the browser that started this login may finish it.
    let same_browser = match (&row.browser_hash, &browser) {
        (Some(e), Some(b)) => {
            use subtle::ConstantTimeEq;
            bool::from(e.ct_eq(b))
        }
        _ => false,
    };
    if !same_browser {
        tracing::warn!("OIDC callback from a browser that didn't start the login");
        return Ok(complete_redirect("error=login_failed"));
    }
    let identity = match oidc.finish(code, row.pkce_verifier, row.nonce).await {
        Ok(i) => i,
        Err(AppError::Unauthorized) => return Ok(complete_redirect("error=login_failed")),
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
        Err(AppError::Forbidden(_)) => return Ok(complete_redirect("error=account_disabled")),
        Err(e) => return Err(e),
    };
    let (code, hash) = db::new_secret("cpn_l_");
    repo::sessions::insert_login_code(&state.db, &hash, &user_id, db::now() + LOGIN_CODE_TTL_MS)
        .await?;
    let encoded: String = url::form_urlencoded::byte_serialize(row.return_to.as_bytes()).collect();
    Ok(complete_redirect(&format!(
        "code={code}&return_to={encoded}"
    )))
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
        state.config.session_ttl_days * DAY_MS,
    )
    .await?;
    Ok(SessionResponse {
        token: s.token,
        expires_at: s.expires_at,
        user: auth::user_info(&state.db, user_id).await?,
    })
}

/// Swap the one-time code from the OIDC callback for a session token.
pub async fn exchange(
    State(state): State<AppState>,
    Json(req): Json<ExchangeRequest>,
) -> AppResult<Json<SessionResponse>> {
    let hash = db::sha256(req.code.as_bytes());
    let (user_id, expires) = repo::sessions::take_login_code(&state.db, &hash)
        .await?
        .ok_or(AppError::Unauthorized)?;
    if expires < db::now() {
        return Err(AppError::Unauthorized);
    }
    Ok(Json(
        session_response(&state, &user_id, "web", Some("browser")).await?,
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
    let user_id = auth::provision_user(&state, DEV_ISSUER, &username, &display, None).await?;
    Ok(Json(
        session_response(&state, &user_id, "web", Some("dev login")).await?,
    ))
}

pub async fn logout(State(state): State<AppState>, who: Principal) -> AppResult<StatusCode> {
    revoke_session(&state, &who.user_id, &who.session_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn revoke_session(state: &AppState, user_id: &str, session_id: &str) -> AppResult<()> {
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
