//! Encrypted attachment blobs. Names, types and paths live in the encrypted
//! vault index; the server only sees opaque bytes and their size.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::access;
use crate::auth::Principal;
use crate::db;
use crate::error::{AppError, AppResult, bad, conflict};
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
    let r = sqlx::query("INSERT INTO attachments (vault_id, id, size, author_id, data, created_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(&v)
        .bind(&id)
        .bind(body.len() as i64)
        .bind(&who.user_id)
        .bind(body.as_ref())
        .bind(db::now())
        .execute(&state.db)
        .await;
    match r {
        // Attachments are immutable; re-uploading the same id is a no-op retry.
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            let size: i64 =
                sqlx::query_scalar("SELECT size FROM attachments WHERE vault_id = ? AND id = ?")
                    .bind(&v)
                    .bind(&id)
                    .fetch_one(&state.db)
                    .await?;
            if size == body.len() as i64 {
                Ok(StatusCode::OK)
            } else {
                Err(conflict("attachment id already used"))
            }
        }
        r => {
            r?;
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
    let data: Vec<u8> =
        sqlx::query_scalar("SELECT data FROM attachments WHERE vault_id = ? AND id = ?")
            .bind(&v)
            .bind(&id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound)?;
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
    let n = sqlx::query("DELETE FROM attachments WHERE vault_id = ? AND id = ?")
        .bind(&v)
        .bind(&id)
        .execute(&state.db)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}
