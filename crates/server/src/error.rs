use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use coolpsonotes_core::protocol::ApiError;

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
    #[error("too many requests")]
    RateLimited,
    #[error("{0}")]
    Unavailable(String),
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
            AppError::RateLimited => "rate_limited",
            AppError::Unavailable(_) => "unavailable",
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
            AppError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            AppError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
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
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if let AppError::Internal(e) = &self {
            tracing::error!(error = %e, "internal error");
        }
        (self.status(), Json(self.body())).into_response()
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
