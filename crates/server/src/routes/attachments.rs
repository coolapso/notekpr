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
use crate::sync::storage_changed;

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
    state.instance.require_room(&state.db, &v).await?;
    let size = body.len() as u64;
    // A file store's file is written before the write lock, so a slow
    // upload holds nobody up; the row claims it under the lock.
    let staged = state.blobs.stage(&v, &id, body).await?;
    let stored = {
        let _w = state.write_lock.lock().await;
        // The bytes may live outside the database, so the role is re-checked
        // under the write lock (which every membership change takes) rather
        // than in a transaction.
        let checked = async {
            access::require_writer(&state.db, &v, &who.user_id).await?;
            state.instance.require_room(&state.db, &v).await
        };
        match checked.await {
            Ok(()) => state.blobs.commit(&staged, &who.user_id).await,
            Err(e) => Err(e),
        }
    };
    if !matches!(stored, Ok(Stored::Created)) {
        state.blobs.discard(staged).await;
    }
    match stored? {
        Stored::AlreadyThere => Ok(StatusCode::OK),
        Stored::Created => {
            metrics::counter!("notekpr_attachment_bytes_total").increment(size);
            storage_changed(&state, &v, false).await;
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
    let (size, body) = state.blobs.get(&v, &id).await?;
    let mut resp = body.into_response();
    let h = resp.headers_mut();
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
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

/// Deleting an attachment for good (emptying the trash): the vault's owner
/// and admins only, as for notes (`sync::purge`). Kept for the undo period
/// like them, unless that is 0.
pub async fn delete(
    State(state): State<AppState>,
    who: Principal,
    Path((v, id)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    access::require_admin(&state.db, &v, &who.user_id).await?;
    let _w = state.write_lock.lock().await;
    access::require_admin(&state.db, &v, &who.user_id).await?;
    let was_full = state.instance.vault_full(&state.db, &v).await?;
    if state.config.purge_undo_days == 0 {
        state.blobs.delete(&v, &id).await?;
    } else {
        state.blobs.purge(&v, &id, db::now()).await?;
    }
    storage_changed(&state, &v, was_full).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Undo deleting an attachment for good, while it is kept (owner and
/// admins, as deleting it).
pub async fn restore(
    State(state): State<AppState>,
    who: Principal,
    Path((v, id)): Path<(String, String)>,
) -> AppResult<StatusCode> {
    access::require_admin(&state.db, &v, &who.user_id).await?;
    let _w = state.write_lock.lock().await;
    access::require_admin(&state.db, &v, &who.user_id).await?;
    // It counts again.
    state.instance.require_room(&state.db, &v).await?;
    let since = db::now() - state.config.purge_undo_ms();
    state.blobs.restore(&v, &id, since).await?;
    storage_changed(&state, &v, false).await;
    Ok(StatusCode::NO_CONTENT)
}
