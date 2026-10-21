//! Local plaintext HTTP API: an explicit cryptographic endpoint for agents
//! and scripts.
//!
//! The gateway decrypts vaults with the profile's keys (an agent identity, or
//! a person's) and exposes documents as Markdown over HTTP on a loopback
//! address. It is protected by a bearer token stored in the profile, can be
//! restricted to some vaults and to read-only access, never exposes key
//! material, and appends every write to the audit log. Edits are merged into
//! the CRDT as diffs, then encrypted and synced like any other client's.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures::Stream;
use serde::Deserialize;
use serde_json::json;
use subtle::ConstantTimeEq;
use tokio::sync::broadcast;

use crate::audit::AuditLog;
use crate::hub::{Hub, HubEvent};
use crate::ops::{self, OpError};

pub struct Gateway {
    pub hub: Arc<Hub>,
    pub token: String,
    pub audit: Arc<AuditLog>,
}

type St = Arc<Gateway>;

/// A fresh random gateway token.
pub fn new_token() -> String {
    format!(
        "cpgw_{}",
        data_encoding::BASE32_NOPAD
            .encode(&notekpr_core::crypto::random_bytes::<20>())
            .to_lowercase()
    )
}

pub fn router(gw: St) -> Router {
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/vaults", get(list_vaults))
        .route("/v1/vaults/{v}/documents", get(list_documents))
        .route(
            "/v1/vaults/{v}/documents/{*path}",
            get(get_document).put(put_document).delete(delete_document),
        )
        .route("/v1/vaults/{v}/move", post(move_document))
        .route("/v1/vaults/{v}/backlinks/{*path}", get(backlinks))
        .route(
            "/v1/vaults/{v}/comments/{*path}",
            get(list_comments).post(comment),
        )
        .route("/v1/vaults/{v}/history/{*path}", get(history))
        .route(
            "/v1/vaults/{v}/attachments/{*path}",
            get(get_attachment)
                .put(put_attachment)
                .layer(DefaultBodyLimit::max(128 << 20)),
        )
        .route("/v1/search", get(search))
        .route("/v1/events", get(events))
        .fallback(|| async { error(StatusCode::NOT_FOUND, "not_found", "no such endpoint") })
        .layer(middleware::from_fn_with_state(gw.clone(), auth))
        .layer(DefaultBodyLimit::max(16 << 20))
        .with_state(gw)
}

/// Serve on an already bound listener until `shutdown` resolves.
pub async fn serve(
    listener: tokio::net::TcpListener,
    gw: St,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let app = router(gw);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await?;
    Ok(())
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        axum::Json(json!({ "error": code, "message": message })),
    )
        .into_response()
}

fn op_error(e: OpError) -> Response {
    let (status, code) = match &e {
        OpError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
        OpError::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
        OpError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
        OpError::Precondition(_) => (StatusCode::PRECONDITION_FAILED, "precondition_failed"),
        OpError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
        OpError::Unavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
        OpError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    };
    error(status, code, &e.to_string())
}

async fn auth(State(gw): State<St>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| bool::from(t.trim().as_bytes().ct_eq(gw.token.as_bytes())));
    if !ok {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or wrong gateway token",
        );
    }
    let mut resp = next.run(req).await;
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// The refusal to send when the gateway is read-only.
fn read_only_refusal(gw: &Gateway) -> Option<Response> {
    gw.hub.read_only.then(|| {
        error(
            StatusCode::FORBIDDEN,
            "read_only",
            "this gateway is read-only",
        )
    })
}

async fn status(State(gw): State<St>) -> Response {
    axum::Json(json!({
        "user_id": gw.hub.session.user_id,
        "kind": gw.hub.session.kind,
        "online": gw.hub.is_online(),
        "read_only": gw.hub.read_only,
        "vaults": gw.hub.vault_ids().len(),
    }))
    .into_response()
}

async fn list_vaults(State(gw): State<St>) -> Response {
    axum::Json(ops::list_vaults(&gw.hub).await).into_response()
}

#[derive(Deserialize)]
struct ListQuery {
    folder: Option<String>,
    #[serde(default)]
    all: bool,
}

async fn list_documents(
    State(gw): State<St>,
    Path(v): Path<String>,
    Query(q): Query<ListQuery>,
) -> Response {
    match ops::list_documents(&gw.hub, &v, q.folder.as_deref(), q.all).await {
        Ok(d) => axum::Json(d).into_response(),
        Err(e) => op_error(e),
    }
}

#[derive(Deserialize)]
struct FormatQuery {
    format: Option<String>,
}

async fn get_document(
    State(gw): State<St>,
    Path((v, path)): Path<(String, String)>,
    Query(q): Query<FormatQuery>,
) -> Response {
    match ops::read_document(&gw.hub, &v, &path).await {
        Ok(doc) if q.format.as_deref() == Some("json") => axum::Json(doc).into_response(),
        Ok(doc) => {
            let etag = format!("\"{}\"", doc.hash);
            (
                [
                    (
                        header::CONTENT_TYPE,
                        "text/markdown; charset=utf-8".to_string(),
                    ),
                    (header::ETAG, etag),
                ],
                doc.content,
            )
                .into_response()
        }
        Err(e) => op_error(e),
    }
}

async fn put_document(
    State(gw): State<St>,
    Path((v, path)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(r) = read_only_refusal(&gw) {
        return r;
    }
    let Ok(text) = String::from_utf8(body.to_vec()) else {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "the document must be UTF-8 Markdown",
        );
    };
    let if_match = headers
        .get(header::IF_MATCH)
        .and_then(|h| h.to_str().ok())
        .map(str::to_string);
    let create_only = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|h| h.to_str().ok())
        == Some("*");
    let res = if create_only {
        ops::create_document(&gw.hub, &v, &path, &text).await
    } else {
        ops::put_document(&gw.hub, &v, &path, &text, if_match.as_deref()).await
    };
    match res {
        Ok(out) => {
            if out.changed {
                gw.audit.record(
                    "gateway",
                    "put_document",
                    &out.vault,
                    Some(&out.id),
                    out.bytes,
                );
            }
            let status = if out.created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            let etag = format!("\"{}\"", out.hash);
            (status, [(header::ETAG, etag)], axum::Json(out)).into_response()
        }
        Err(e) => op_error(e),
    }
}

async fn delete_document(
    State(gw): State<St>,
    Path((v, path)): Path<(String, String)>,
) -> Response {
    if let Some(r) = read_only_refusal(&gw) {
        return r;
    }
    match ops::delete_document(&gw.hub, &v, &path).await {
        Ok((vault, ids)) => {
            for id in &ids {
                gw.audit
                    .record("gateway", "trash_document", &vault, Some(id), 0);
            }
            axum::Json(json!({ "vault": vault, "trashed": ids })).into_response()
        }
        Err(e) => op_error(e),
    }
}

#[derive(Deserialize)]
struct MoveBody {
    from: String,
    to: String,
    #[serde(default)]
    update_links: bool,
}

async fn move_document(State(gw): State<St>, Path(v): Path<String>, body: Bytes) -> Response {
    if let Some(r) = read_only_refusal(&gw) {
        return r;
    }
    let Ok(m) = serde_json::from_slice::<MoveBody>(&body) else {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "expected JSON {\"from\": ..., \"to\": ...}",
        );
    };
    match ops::move_document(&gw.hub, &v, &m.from, &m.to, m.update_links).await {
        Ok(out) => {
            gw.audit
                .record("gateway", "move_document", &out.vault, out.id.as_deref(), 0);
            axum::Json(out).into_response()
        }
        Err(e) => op_error(e),
    }
}

async fn list_comments(State(gw): State<St>, Path((v, path)): Path<(String, String)>) -> Response {
    match ops::comments(&gw.hub, &v, &path).await {
        Ok(c) => axum::Json(c).into_response(),
        Err(e) => op_error(e),
    }
}

/// `{"on": QUOTE, "text": …}` comments on that text; `{"id": …, "action":
/// "reply"|"edit"|"delete"|"resolve"|"reopen", "text": …}` acts on a comment.
#[derive(Deserialize)]
struct CommentBody {
    on: Option<String>,
    id: Option<String>,
    action: Option<String>,
    #[serde(default)]
    text: String,
}

async fn comment(
    State(gw): State<St>,
    Path((v, path)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    use crate::vault::CommentAct;
    if let Some(r) = read_only_refusal(&gw) {
        return r;
    }
    let Ok(b) = serde_json::from_slice::<CommentBody>(&body) else {
        return error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "expected JSON {\"on\": ..., \"text\": ...} or {\"id\": ..., \"action\": ...}",
        );
    };
    let len = b.text.len();
    let res = match (b.on, b.id, b.action.as_deref()) {
        (Some(on), None, None) => ops::add_comment(&gw.hub, &v, &path, &on, &b.text).await,
        (None, Some(id), Some(action)) => {
            let act = match action {
                "reply" => CommentAct::Reply(b.text),
                "edit" => CommentAct::Edit(b.text),
                "delete" => CommentAct::Delete,
                "resolve" => CommentAct::Resolve,
                "reopen" => CommentAct::Reopen,
                _ => {
                    return error(
                        StatusCode::BAD_REQUEST,
                        "bad_request",
                        "action is reply, edit, delete, resolve or reopen",
                    );
                }
            };
            ops::comment_action(&gw.hub, &v, &path, &id, act).await
        }
        _ => {
            return error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "give either `on` (a new comment) or `id` and `action`",
            );
        }
    };
    match res {
        Ok(sent) => {
            gw.audit.record(
                "gateway",
                "comment",
                &sent.vault_id,
                Some(&sent.note_id),
                len,
            );
            axum::Json(sent).into_response()
        }
        Err(e) => op_error(e),
    }
}

async fn backlinks(State(gw): State<St>, Path((v, path)): Path<(String, String)>) -> Response {
    match ops::backlinks(&gw.hub, &v, &path).await {
        Ok(d) => axum::Json(d).into_response(),
        Err(e) => op_error(e),
    }
}

#[derive(Deserialize)]
struct HistoryQuery {
    revision: Option<String>,
}

async fn history(
    State(gw): State<St>,
    Path((v, path)): Path<(String, String)>,
    Query(q): Query<HistoryQuery>,
) -> Response {
    match q.revision {
        Some(rev) => match ops::revision(&gw.hub, &v, &path, &rev).await {
            Ok(text) => (
                [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
                text,
            )
                .into_response(),
            Err(e) => op_error(e),
        },
        None => match ops::history(&gw.hub, &v, &path).await {
            Ok(h) => axum::Json(h).into_response(),
            Err(e) => op_error(e),
        },
    }
}

async fn get_attachment(State(gw): State<St>, Path((v, path)): Path<(String, String)>) -> Response {
    match ops::get_attachment(&gw.hub, &v, &path).await {
        Ok((info, data)) => {
            let mime = info
                .mime
                .unwrap_or_else(|| "application/octet-stream".into());
            (
                [
                    (header::CONTENT_TYPE, mime),
                    // Never render attachments inline (stored HTML/SVG could script the origin).
                    (header::CONTENT_DISPOSITION, "attachment".to_string()),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                ],
                data,
            )
                .into_response()
        }
        Err(e) => op_error(e),
    }
}

async fn put_attachment(
    State(gw): State<St>,
    Path((v, path)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(r) = read_only_refusal(&gw) {
        return r;
    }
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .filter(|m| !m.starts_with("application/x-www-form-urlencoded") && !m.is_empty())
        .map(str::to_string);
    if body.is_empty() {
        return error(StatusCode::BAD_REQUEST, "bad_request", "empty attachment");
    }
    match ops::put_attachment(&gw.hub, &v, &path, &body, mime.as_deref()).await {
        Ok(out) => {
            if out.changed {
                gw.audit.record(
                    "gateway",
                    "put_attachment",
                    &out.vault,
                    Some(&out.id),
                    out.bytes,
                );
            }
            let status = if out.created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            (status, axum::Json(out)).into_response()
        }
        Err(e) => op_error(e),
    }
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    vault: Option<String>,
    limit: Option<usize>,
}

async fn search(State(gw): State<St>, Query(q): Query<SearchQuery>) -> Response {
    match ops::search(
        &gw.hub,
        &q.q,
        q.vault.as_deref(),
        q.limit.unwrap_or(50).min(500),
    )
    .await
    {
        Ok(r) => axum::Json(r).into_response(),
        Err(e) => op_error(e),
    }
}

/// Change notifications as JSON events (ids and paths, never content).
pub async fn event_json(hub: &Hub, ev: HubEvent) -> Vec<serde_json::Value> {
    match ev {
        HubEvent::Changed {
            vault_id,
            objects,
            index_changed,
            purged,
            local,
        } => {
            let origin = if local { "local" } else { "remote" };
            let mut out = Vec::new();
            if let Some(v) = hub.get(&vault_id) {
                let g = v.lock().await;
                for o in objects {
                    if let Some(path) = g.display_path(&o) {
                        out.push(json!({ "type": "document_changed", "vault": vault_id, "id": o, "path": path, "origin": origin }));
                    }
                }
            }
            if index_changed {
                out.push(json!({ "type": "index_changed", "vault": vault_id, "origin": origin }));
            }
            for p in purged {
                out.push(json!({ "type": "document_deleted", "vault": vault_id, "id": p }));
            }
            out
        }
        HubEvent::VaultAdded { vault_id } => {
            vec![json!({ "type": "vault_added", "vault": vault_id })]
        }
        HubEvent::VaultClosed { vault_id, reason } => {
            vec![json!({ "type": "vault_closed", "vault": vault_id, "reason": reason })]
        }
        HubEvent::Connection { online } => vec![json!({ "type": "connection", "online": online })],
    }
}

async fn events(State(gw): State<St>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = gw.hub.subscribe();
    let hub = gw.hub.clone();
    let stream = futures::stream::unfold(
        (rx, hub, VecDeque::<Event>::new()),
        |(mut rx, hub, mut queue)| async move {
            loop {
                if let Some(ev) = queue.pop_front() {
                    return Some((Ok(ev), (rx, hub, queue)));
                }
                match rx.recv().await {
                    Ok(e) => {
                        for j in event_json(&hub, e).await {
                            let name = j["type"].as_str().unwrap_or("event").to_string();
                            queue.push_back(Event::default().event(name).data(j.to_string()));
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        queue.push_back(
                            Event::default()
                                .event("lagged")
                                .data(json!({ "type": "lagged", "missed": n }).to_string()),
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::default())
}
