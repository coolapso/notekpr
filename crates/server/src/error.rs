use std::time::Duration;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use notekpr_core::protocol::{
    ApiError, ERROR_LIMIT_REACHED, ERROR_RATE_LIMITED, ERROR_READ_ONLY, ERROR_RESTARTING,
    ERROR_SUSPENDED, ERROR_UPDATE_REQUIRED, ERROR_VAULT_FULL, ERROR_WRITE_LIMIT, ReadOnly,
};
use notekpr_core::version::Version;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),
    #[error("authentication required")]
    Unauthorized,
    #[error("{0}")]
    Forbidden(String),
    /// Also used when the caller lacks access, so existence is not revealed.
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    /// Content was encrypted with an outdated key (e.g. the note became
    /// individually shared). Clients reload keys and re-encrypt; nothing is lost.
    #[error("{0}")]
    StaleKey(String),
    #[error("request too large")]
    TooLarge,
    /// The device was removed from the account (or never belonged to it).
    /// Clients may then forget its keys; any other 404 must not make them.
    #[error("this device was removed from the account")]
    DeviceRevoked,
    /// An answer, not a busy server (a snapshot within its cooldown): no
    /// `Retry-After`.
    #[error("too many requests")]
    RateLimited,
    /// A rate limit refused the request before it was handled: the same
    /// request works after this long (`Retry-After`).
    #[error("too many requests; try again in a moment")]
    RateLimitedFor(Duration),
    /// The person (an agent's owner) has sent as many changes as their daily
    /// write allowance lets them for now (`person_limits.rs`); nothing was
    /// done. Clients keep the change and send it after this long.
    #[error("you have changed as much as this server allows for now; try again later")]
    WriteLimit(Duration),
    /// The account (an agent's owner's) is suspended until then (ms).
    #[error("this account is suspended for now")]
    Suspended(i64),
    /// The server refuses changes for now (see `instance.rs`); clients keep
    /// them and send them when it's writable again.
    #[error("{}", read_only_message(*.0))]
    ReadOnly(ReadOnly),
    /// An instance limit (accounts, vaults, agents) is reached.
    #[error("{0}")]
    LimitReached(String),
    /// The vault reached its storage limit (`instance.rs`): it takes no new
    /// content until something is deleted for good or the limit is raised.
    /// Clients keep their changes and send them once it has room.
    #[error(
        "this vault is full: delete something in it for good, or ask an administrator to raise its limit"
    )]
    VaultFull,
    /// The app is older than the oldest version the server accepts
    /// (`apps.rs`).
    #[error("this server needs notekpr {0} or newer: update this app")]
    UpdateRequired(Version),
    #[error("{0}")]
    Unavailable(String),
    /// This server doesn't write to the database (`writer.rs`): it is
    /// handing over to its next version, or waiting to take over. Nothing
    /// was done; the same request works in a moment.
    #[error("the server is restarting: try again in a moment")]
    Restarting,
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

pub type AppResult<T> = Result<T, AppError>;

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        AppError::Internal(e.into())
    }
}

impl AppError {
    pub fn code(&self) -> &'static str {
        match self {
            AppError::BadRequest(_) => "bad_request",
            AppError::Unauthorized => "unauthorized",
            AppError::Forbidden(_) => "forbidden",
            AppError::NotFound => "not_found",
            AppError::Conflict(_) => "conflict",
            AppError::StaleKey(_) => "stale_key",
            AppError::TooLarge => "too_large",
            AppError::DeviceRevoked => "device_revoked",
            AppError::RateLimited | AppError::RateLimitedFor(_) => ERROR_RATE_LIMITED,
            AppError::WriteLimit(_) => ERROR_WRITE_LIMIT,
            AppError::Suspended(_) => ERROR_SUSPENDED,
            AppError::ReadOnly(_) => ERROR_READ_ONLY,
            AppError::LimitReached(_) => ERROR_LIMIT_REACHED,
            AppError::VaultFull => ERROR_VAULT_FULL,
            AppError::UpdateRequired(_) => ERROR_UPDATE_REQUIRED,
            AppError::Unavailable(_) => "unavailable",
            AppError::Restarting => ERROR_RESTARTING,
            AppError::Internal(_) => "internal",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::Forbidden(_) => StatusCode::FORBIDDEN,
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::StaleKey(_) => StatusCode::PRECONDITION_FAILED,
            AppError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            AppError::DeviceRevoked => StatusCode::NOT_FOUND,
            AppError::RateLimited
            | AppError::RateLimitedFor(_)
            | AppError::WriteLimit(_)
            | AppError::Suspended(_) => StatusCode::TOO_MANY_REQUESTS,
            AppError::ReadOnly(_) => StatusCode::SERVICE_UNAVAILABLE,
            AppError::LimitReached(_) => StatusCode::FORBIDDEN,
            AppError::VaultFull => StatusCode::INSUFFICIENT_STORAGE,
            AppError::UpdateRequired(_) => StatusCode::UPGRADE_REQUIRED,
            AppError::Unavailable(_) | AppError::Restarting => StatusCode::SERVICE_UNAVAILABLE,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// When the same request may be made again, in whole seconds (at least
    /// one): `Retry-After`, the WebSocket error's `retry_after`.
    pub fn retry_after(&self) -> Option<u64> {
        let wait = match self {
            AppError::RateLimitedFor(d) | AppError::WriteLimit(d) => *d,
            AppError::Suspended(until) => {
                Duration::from_millis(until.saturating_sub(crate::db::now()).max(0) as u64)
            }
            AppError::Restarting => Duration::from_secs(1),
            _ => return None,
        };
        Some((wait.as_secs() + u64::from(wait.subsec_nanos() > 0)).max(1))
    }

    pub fn body(&self) -> ApiError {
        let message = match self {
            // Never leak internal error details to clients.
            AppError::Internal(_) => "internal server error".to_string(),
            e => e.to_string(),
        };
        ApiError {
            error: self.code().to_string(),
            message,
            oldest_version: match self {
                AppError::UpdateRequired(v) => Some(v.to_string()),
                _ => None,
            },
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if let AppError::Internal(e) = &self {
            tracing::error!(error = %e, "internal error");
        }
        let mut resp = (self.status(), Json(self.body())).into_response();
        // Only these: what else says 503 (read-only mode) or 429 (a
        // snapshot's cooldown) won't change by trying again.
        if let Some(secs) = self.retry_after() {
            resp.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                axum::http::HeaderValue::from(secs),
            );
        }
        resp
    }
}

/// Said of whatever was refused (note edits are kept by the clients; other
/// changes, such as a new vault, are tried again by the person).
fn read_only_message(why: ReadOnly) -> &'static str {
    match why {
        ReadOnly::Manual | ReadOnly::Other => "the server is read-only for now; try again later",
        ReadOnly::StorageFull => "the server's storage is full; try again later",
    }
}

pub fn bad(msg: impl Into<String>) -> AppError {
    AppError::BadRequest(msg.into())
}

pub fn forbidden(msg: impl Into<String>) -> AppError {
    AppError::Forbidden(msg.into())
}

pub fn conflict(msg: impl Into<String>) -> AppError {
    AppError::Conflict(msg.into())
}
