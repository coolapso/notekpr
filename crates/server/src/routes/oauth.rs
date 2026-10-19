//! OAuth 2.0 endpoints for non-browser clients.
//!
//! * RFC 8628 device authorization grant: the desktop/CLI client shows a code,
//!   the user approves it in the web app (where they signed in with OIDC).
//! * RFC 6749 §4.4 client credentials grant: agent identities authenticate with
//!   a revocable, per-agent client secret and receive short-lived tokens.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Form, Json};
use coolpsonotes_core::protocol::{
    DeviceAuthorizationResponse, DeviceCodeDecision, DeviceCodeLookup, OAuthError, TokenResponse,
};
use serde::Deserialize;
use sqlx::Row;
use subtle::ConstantTimeEq;

use crate::auth::{self, Principal};
use crate::db;
use crate::error::{AppError, AppResult, bad};
use crate::state::AppState;

const DEVICE_CODE_TTL_MS: i64 = 600_000;
const POLL_INTERVAL_S: i64 = 5;
const DAY_MS: i64 = 24 * 3600 * 1000;
pub const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

fn oauth_error(status: StatusCode, error: &str, desc: Option<&str>) -> Response {
    (
        status,
        Json(OAuthError {
            error: error.into(),
            error_description: desc.map(str::to_string),
        }),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct DeviceAuthorizationForm {
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_name: Option<String>,
}

pub async fn device_authorization(
    State(state): State<AppState>,
    Form(f): Form<DeviceAuthorizationForm>,
) -> AppResult<Json<DeviceAuthorizationResponse>> {
    let client_name: String = f
        .client_name
        .or(f.client_id)
        .unwrap_or_else(|| "coolpsonotes client".into())
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    let (device_code, hash) = db::new_secret("cpn_d_");
    let now = db::now();
    sqlx::query("DELETE FROM device_grants WHERE expires_at < ?")
        .bind(now - DAY_MS)
        .execute(&state.db)
        .await?;
    let mut user_code = db::new_user_code();
    for _ in 0..5 {
        let r = sqlx::query(
            "INSERT INTO device_grants (device_code_hash, user_code, client_name, status, created_at, expires_at)
             VALUES (?, ?, ?, 'pending', ?, ?)",
        )
        .bind(&hash)
        .bind(&user_code)
        .bind(&client_name)
        .bind(now)
        .bind(now + DEVICE_CODE_TTL_MS)
        .execute(&state.db)
        .await;
        match r {
            Ok(_) => break,
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                user_code = db::new_user_code()
            }
            Err(e) => return Err(e.into()),
        }
    }
    let base = state.config.public_url();
    Ok(Json(DeviceAuthorizationResponse {
        device_code,
        verification_uri: format!("{base}/device"),
        verification_uri_complete: format!("{base}/device?code={user_code}"),
        user_code,
        expires_in: (DEVICE_CODE_TTL_MS / 1000) as u64,
        interval: POLL_INTERVAL_S as u64,
    }))
}

#[derive(Deserialize)]
pub struct TokenForm {
    grant_type: String,
    #[serde(default)]
    device_code: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    client_secret: Option<String>,
}

fn basic_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let v = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, b64) = v.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let raw = data_encoding::BASE64.decode(b64.trim().as_bytes()).ok()?;
    let s = String::from_utf8(raw).ok()?;
    let (id, secret) = s.split_once(':')?;
    let dec = |x: &str| {
        url::form_urlencoded::parse(format!("x={x}").as_bytes())
            .next()
            .map(|(_, v)| v.into_owned())
    };
    Some((dec(id)?, dec(secret)?))
}

pub async fn token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<TokenForm>,
) -> AppResult<Response> {
    match f.grant_type.as_str() {
        DEVICE_GRANT => device_token(&state, f).await,
        "client_credentials" => {
            let (id, secret) = match (basic_credentials(&headers), f.client_id, f.client_secret) {
                (Some(c), _, _) => c,
                (None, Some(id), Some(secret)) => (id, secret),
                _ => {
                    return Ok(oauth_error(
                        StatusCode::UNAUTHORIZED,
                        "invalid_client",
                        None,
                    ));
                }
            };
            client_credentials(&state, &id, &secret).await
        }
        _ => Ok(oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            None,
        )),
    }
}

async fn device_token(state: &AppState, f: TokenForm) -> AppResult<Response> {
    let Some(code) = f.device_code else {
        return Ok(oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            Some("device_code is required"),
        ));
    };
    let hash = db::sha256(code.as_bytes());
    let _w = state.write_lock.lock().await;
    let Some(row) = sqlx::query(
        "SELECT status, user_id, client_name, expires_at, last_poll_at FROM device_grants WHERE device_code_hash = ?",
    )
    .bind(&hash)
    .fetch_optional(&state.db)
    .await?
    else {
        return Ok(oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", None));
    };
    let now = db::now();
    let expires: i64 = row.get("expires_at");
    if expires < now {
        return Ok(oauth_error(StatusCode::BAD_REQUEST, "expired_token", None));
    }
    let status: String = row.get("status");
    match status.as_str() {
        "pending" => {
            let last: Option<i64> = row.get("last_poll_at");
            sqlx::query("UPDATE device_grants SET last_poll_at = ? WHERE device_code_hash = ?")
                .bind(now)
                .bind(&hash)
                .execute(&state.db)
                .await?;
            if last.is_some_and(|l| now - l < (POLL_INTERVAL_S * 1000) - 500) {
                return Ok(oauth_error(StatusCode::BAD_REQUEST, "slow_down", None));
            }
            Ok(oauth_error(
                StatusCode::BAD_REQUEST,
                "authorization_pending",
                None,
            ))
        }
        "denied" => Ok(oauth_error(StatusCode::BAD_REQUEST, "access_denied", None)),
        "approved" => {
            let user_id: String = row.get("user_id");
            let client_name: String = row.get("client_name");
            sqlx::query("UPDATE device_grants SET status = 'consumed' WHERE device_code_hash = ?")
                .bind(&hash)
                .execute(&state.db)
                .await?;
            let ttl = state.config.device_session_ttl_days * DAY_MS;
            let s =
                auth::create_session(&state.db, &user_id, "cli", Some(&client_name), ttl).await?;
            db::audit(
                &state.db,
                Some(&user_id),
                Some(&user_id),
                None,
                "login.device_grant",
                None,
                serde_json::json!({ "client": client_name }),
            )
            .await;
            Ok(Json(TokenResponse {
                access_token: s.token,
                token_type: "Bearer".into(),
                expires_in: (ttl / 1000) as u64,
                scope: None,
            })
            .into_response())
        }
        _ => Ok(oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", None)),
    }
}

async fn client_credentials(
    state: &AppState,
    client_id: &str,
    secret: &str,
) -> AppResult<Response> {
    let row = sqlx::query(
        "SELECT c.agent_id, c.secret_hash, c.revoked_at, a.disabled AS agent_disabled, o.disabled AS owner_disabled
         FROM agent_credentials c JOIN users a ON a.id = c.agent_id LEFT JOIN users o ON o.id = a.owner_id
         WHERE c.client_id = ?",
    )
    .bind(client_id)
    .fetch_optional(&state.db)
    .await?;
    let hash = db::sha256(secret.as_bytes());
    let Some(row) = row else {
        return Ok(oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            None,
        ));
    };
    let stored: Vec<u8> = row.get("secret_hash");
    let revoked: Option<i64> = row.get("revoked_at");
    let agent_disabled: bool = row.get("agent_disabled");
    let owner_disabled: Option<bool> = row.get("owner_disabled");
    if !bool::from(stored.ct_eq(&hash))
        || revoked.is_some()
        || agent_disabled
        || owner_disabled.unwrap_or(false)
    {
        return Ok(oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            None,
        ));
    }
    let agent_id: String = row.get("agent_id");
    sqlx::query("UPDATE agent_credentials SET last_used_at = ? WHERE client_id = ?")
        .bind(db::now())
        .bind(client_id)
        .execute(&state.db)
        .await?;
    let ttl_ms = state.config.agent_token_ttl_secs * 1000;
    let s = auth::create_session(&state.db, &agent_id, "agent", Some(client_id), ttl_ms).await?;
    Ok(Json(TokenResponse {
        access_token: s.token,
        token_type: "Bearer".into(),
        expires_in: state.config.agent_token_ttl_secs as u64,
        scope: None,
    })
    .into_response())
}

fn normalize_user_code(s: &str) -> String {
    let c: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if c.len() == 8 {
        format!("{}-{}", &c[..4], &c[4..])
    } else {
        c
    }
}

#[derive(Deserialize)]
pub struct LookupQuery {
    user_code: String,
}

pub async fn device_lookup(
    State(state): State<AppState>,
    who: Principal,
    Query(q): Query<LookupQuery>,
) -> AppResult<Json<DeviceCodeLookup>> {
    who.require_human()?;
    let code = normalize_user_code(&q.user_code);
    let row = sqlx::query(
        "SELECT client_name, created_at, expires_at FROM device_grants
         WHERE user_code = ? AND status = 'pending' AND expires_at > ?",
    )
    .bind(&code)
    .bind(db::now())
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(Json(DeviceCodeLookup {
        user_code: code,
        client_name: row.get("client_name"),
        created_at: row.get("created_at"),
        expires_at: row.get("expires_at"),
    }))
}

pub async fn device_decision(
    State(state): State<AppState>,
    who: Principal,
    Json(d): Json<DeviceCodeDecision>,
) -> AppResult<StatusCode> {
    who.require_human()?;
    let code = normalize_user_code(&d.user_code);
    if code.len() != 9 {
        return Err(bad("invalid code"));
    }
    let status = if d.approve { "approved" } else { "denied" };
    let n = sqlx::query(
        "UPDATE device_grants SET status = ?, user_id = ? WHERE user_code = ? AND status = 'pending' AND expires_at > ?",
    )
    .bind(status)
    .bind(&who.user_id)
    .bind(&code)
    .bind(db::now())
    .execute(&state.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(AppError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}
