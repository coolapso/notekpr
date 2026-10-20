pub mod admin;
pub mod agents;
pub mod attachments;
pub mod auth;
pub mod devices;
pub mod me;
pub mod member_log;
pub mod oauth;
pub mod shares;
pub mod sync;
pub mod users;
pub mod vaults;

use std::time::Instant;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::{Next, from_fn, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use tower_http::compression::CompressionLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::error::AppError;
use crate::ratelimit;
use crate::state::AppState;
use crate::ws;

const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; \
img-src 'self' blob: data:; media-src 'self' blob:; font-src 'self'; connect-src 'self'; object-src 'none'; \
frame-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

async fn api_not_found() -> AppError {
    AppError::NotFound
}

pub fn router(state: AppState) -> Router {
    let cfg = &state.config;
    let b64 = |n: u64| (n as usize) * 4 / 3 + 64 * 1024;
    let push_limit = DefaultBodyLimit::max(b64(cfg.max_update_bytes()).max(16 * 1024 * 1024));
    let snapshot_limit = DefaultBodyLimit::max(b64(cfg.max_snapshot_bytes()));
    let attachment_limit = DefaultBodyLimit::max(cfg.max_attachment_bytes() as usize + 64 * 1024);

    let login = Router::new()
        .route("/auth/oidc/start", get(auth::oidc_start))
        .route("/auth/oidc/callback", get(auth::oidc_callback))
        .route("/auth/exchange", post(auth::exchange))
        .route("/auth/dev-login", post(auth::dev_login))
        .route(
            "/oauth/device_authorization",
            post(oauth::device_authorization),
        )
        .route("/oauth/token", post(oauth::token))
        // Looking up and approving device codes is limited like logins, so
        // codes can't be guessed.
        .route("/oauth/device", get(oauth::device_lookup))
        .route("/oauth/device/decision", post(oauth::device_decision))
        .layer(from_fn_with_state(state.clone(), ratelimit::auth_limit));

    let api = Router::new()
        .route("/info", get(auth::info))
        .route("/auth/logout", post(auth::logout))
        .route("/me", get(me::me))
        .route("/me/account-key", put(me::setup_account_key))
        .route("/me/recovery", get(me::get_recovery).put(me::put_recovery))
        .route("/me/prefs", get(me::get_prefs).put(me::put_prefs))
        .route("/me/session/device", put(me::bind_session_device))
        .route("/me/sessions", get(auth::list_sessions))
        .route("/me/sessions/{id}", delete(auth::delete_session))
        .route("/me/audit", get(me::my_audit))
        .route("/me/export", get(me::export))
        .route("/devices", get(devices::list).post(devices::register))
        .route(
            "/devices/{id}",
            patch(devices::rename).delete(devices::revoke),
        )
        .route("/devices/{id}/envelope", get(devices::envelope))
        .route("/devices/{id}/activate", post(devices::activate))
        .route("/users/search", get(users::search))
        .route("/users/{id}", get(users::get_user))
        .route("/vaults", get(vaults::list).post(vaults::create))
        .route(
            "/vaults/{v}",
            get(vaults::get)
                .patch(vaults::update)
                .delete(vaults::delete),
        )
        .route("/vaults/{v}/keys", get(vaults::keys))
        .route(
            "/vaults/{v}/log",
            get(member_log::log).post(member_log::append),
        )
        .route("/vaults/{v}/keys/grant", post(vaults::grant_keys))
        .route("/vaults/{v}/keys/rotate", post(vaults::rotate_keys))
        .route(
            "/vaults/{v}/members",
            get(vaults::members).post(vaults::add_member),
        )
        .route(
            "/vaults/{v}/members/{u}",
            patch(vaults::update_member).delete(vaults::remove_member),
        )
        .route("/vaults/{v}/audit", get(vaults::audit))
        .route("/vaults/{v}/changes", get(sync::changes))
        .route("/vaults/{v}/updates", post(sync::push).layer(push_limit))
        .route(
            "/vaults/{v}/objects/{o}",
            get(sync::object).delete(sync::purge),
        )
        .route(
            "/vaults/{v}/objects/{o}/snapshots",
            get(sync::list_snapshots)
                .post(sync::create_snapshot)
                .layer(snapshot_limit),
        )
        .route(
            "/vaults/{v}/objects/{o}/snapshots/{id}",
            get(sync::get_snapshot),
        )
        .route("/vaults/{v}/objects/{o}/changes", get(sync::object_changes))
        .route("/vaults/{v}/objects/{o}/keys", get(shares::object_keys))
        .route(
            "/vaults/{v}/objects/{o}/keys/signatures",
            put(shares::sign_grants),
        )
        .route(
            "/vaults/{v}/objects/{o}/key",
            put(shares::set_key).layer(DefaultBodyLimit::max(b64(cfg.max_snapshot_bytes()))),
        )
        .route("/vaults/{v}/objects/{o}/meta", put(shares::set_meta))
        .route(
            "/vaults/{v}/objects/{o}/shares",
            get(shares::list).post(shares::add),
        )
        .route(
            "/vaults/{v}/objects/{o}/shares/{u}",
            patch(shares::update).delete(shares::remove),
        )
        .route("/shares", get(shares::shared_with_me))
        .route(
            "/vaults/{v}/attachments/{id}",
            put(attachments::put)
                .layer(attachment_limit)
                .get(attachments::get)
                .delete(attachments::delete),
        )
        .route("/agents", get(agents::list).post(agents::create))
        .route("/agents/{id}", delete(agents::delete))
        .route("/agents/{id}/credentials", post(agents::create_credential))
        .route(
            "/agents/{id}/credentials/{client_id}",
            delete(agents::revoke_credential),
        )
        .route("/admin/users", get(admin::users))
        .route("/admin/users/{id}", patch(admin::update_user))
        .route("/admin/stats", get(admin::stats))
        .route("/ws", get(ws::handler))
        .merge(login)
        .fallback(api_not_found)
        .layer(from_fn_with_state(state.clone(), ratelimit::api_limit))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ));

    let mut app = Router::new()
        .nest("/api/v1", api)
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics));

    app = match &cfg.web_dir {
        Some(dir) => {
            let index = dir.join("index.html");
            let spa = ServeDir::new(dir).fallback(ServeFile::new(index));
            app.fallback_service(
                tower::ServiceBuilder::new()
                    .layer(SetResponseHeaderLayer::if_not_present(
                        header::CACHE_CONTROL,
                        HeaderValue::from_static("no-cache"),
                    ))
                    .service(spa),
            )
        }
        None => {
            app.fallback(|| async { (StatusCode::NOT_FOUND, "coolpsonotes server (API only)") })
        }
    };

    let hsts = cfg.public_url.starts_with("https://");
    let mut app = app
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CSP),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static("cross-origin-opener-policy"),
            HeaderValue::from_static("same-origin"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static("permissions-policy"),
            HeaderValue::from_static(
                "camera=(), microphone=(), geolocation=(), interest-cohort=()",
            ),
        ));
    if hsts {
        app = app.layer(SetResponseHeaderLayer::overriding(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000"),
        ));
    }
    app.layer(CompressionLayer::new())
        .layer(from_fn(observe))
        .with_state(state)
}

/// Request logging and metrics. Only method, path and status are recorded:
/// query strings (which can carry login codes) and bodies are never logged.
async fn observe(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let start = Instant::now();
    let resp = next.run(req).await;
    let status = resp.status().as_u16();
    let elapsed = start.elapsed();
    let class = format!("{}xx", status / 100);
    metrics::counter!("coolpsonotes_http_requests_total", "method" => method.to_string(), "status" => class).increment(1);
    metrics::histogram!("coolpsonotes_http_request_duration_seconds").record(elapsed.as_secs_f64());
    let quiet = matches!(path.as_str(), "/healthz" | "/readyz" | "/metrics");
    let path = redact_path(&path);
    if quiet {
        tracing::debug!(%method, path, status, ms = elapsed.as_millis() as u64, "request");
    } else if status >= 500 {
        tracing::warn!(%method, path, status, ms = elapsed.as_millis() as u64, "request");
    } else {
        tracing::info!(%method, path, status, ms = elapsed.as_millis() as u64, "request");
    }
    resp
}

/// Collapse ids in paths so logs show route shapes rather than identifiers.
fn redact_path(path: &str) -> String {
    path.split('/')
        .map(|seg| {
            let id_like =
                seg.len() >= 16 && seg.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-');
            if id_like { ":id" } else { seg }
        })
        .collect::<Vec<_>>()
        .join("/")
}

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz(State(state): State<AppState>) -> Response {
    if crate::db::ping(&state.db).await {
        (StatusCode::OK, "ready").into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "database unavailable").into_response()
    }
}

async fn metrics(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(handle) = &state.metrics else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if let Some(expected) = &state.config.metrics_token {
        let ok = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .is_some_and(|t| {
                use subtle::ConstantTimeEq;
                t.as_bytes().ct_eq(expected.as_bytes()).into()
            });
        if !ok {
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }
    metrics::gauge!("coolpsonotes_ws_connected_users").set(state.hub.connection_count() as f64);
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        handle.render(),
    )
        .into_response()
}
