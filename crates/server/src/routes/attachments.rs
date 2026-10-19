//! Encrypted attachment blobs. Names, types and paths live in the encrypted
//! vault index; the server only sees opaque bytes and their size.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::access;
use crate::auth::Principal;
use crate::blobs::Stored;
use crate::db;
use crate::error::{AppError, AppResult, bad};
use crate::state::AppState;

pub async fn put(
    State(state): State<AppState>,
    who: Principal,
    Path((v, id)): Path<(String, String)>,
    body: Bytes,
) -> AppResult<StatusCode> {
    if !db::valid_id(&id) {
        return Err(bad("invalid attachment id"));
    }
    if body.is_empty() {
        return Err(bad("empty attachment"));
    }
    if body.len() as u64 > state.config.max_attachment_bytes() + 1024 {
        return Err(AppError::TooLarge);
    }
    access::require_writer(&state.db, &v, &who.user_id).await?;
    let _w = state.write_lock.lock().await;
    match state.blobs.put(&v, &id, &who.user_id, &body).await? {
        Stored::AlreadyThere => Ok(StatusCode::OK),
        Stored::Created => {
            metrics::counter!("coolpsonotes_attachment_bytes_total").increment(body.len() as u64);
            Ok(StatusCode::CREATED)
        }
    }
}

pub async fn get(
    State(state): State<AppState>,
    who: Principal,
    Path((v, id)): Path<(String, String)>,
) -> AppResult<Response> {
    access::member_role(&state.db, &v, &who.user_id).await?;
    let data = state.blobs.get(&v, &id).await?;
    let mut resp = data.into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    Ok(resp)
}

pub async fn delete(
    State(state): State<AppState>,
    who: Principal,
    Path((v, id)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    access::require_writer(&state.db, &v, &who.user_id).await?;
    state.blobs.delete(&v, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
