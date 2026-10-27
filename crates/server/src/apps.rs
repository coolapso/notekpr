//! Apps too old for this server are told to update, rather than misbehave
//! (docs/protocol.md §11). Every app says which it is and its version with
//! each request ([`APP_HEADER`]) and in the WebSocket's `auth` message.

use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use notekpr_core::protocol::APP_HEADER;
use notekpr_core::version::Version;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// The oldest version of the apps this build works with. The release that
/// stops working with older apps raises it (docs/development.md §Releases):
/// those are then told to update. Administrators can raise it further
/// (`oldest_app_version`, [`crate::instance::Instance::oldest_app`]).
pub const OLDEST_APP_VERSION: Version = Version(0, 0, 0);

/// Refuse an app older than `oldest`. Development builds, and requests that
/// don't say which app they are (scripts, apps from before the header), are
/// never refused.
pub fn check(app: Option<&str>, oldest: Version) -> AppResult<()> {
    match app.and_then(Version::of_app) {
        Some(v) if v < oldest => Err(AppError::UpdateRequired(oldest)),
        _ => Ok(()),
    }
}

/// Every API request but `GET /info`, which apps read the server's version
/// from.
pub async fn require_current(
    State(state): State<AppState>,
    matched: Option<MatchedPath>,
    req: Request,
    next: Next,
) -> Response {
    let path = matched.as_ref().map(|m| m.as_str()).unwrap_or_default();
    if path.strip_prefix("/api/v1").unwrap_or(path) != "/info" {
        let app = req.headers().get(APP_HEADER).and_then(|v| v.to_str().ok());
        if let Err(e) = check(app, state.instance.oldest_app()) {
            return e.into_response();
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_released_apps_older_than_the_oldest_are_refused() {
        let oldest = Version(0, 6, 0);
        assert!(check(Some("cli/0.5.9"), oldest).is_err());
        assert!(check(Some("web/0.5.0-3-gabc1234"), oldest).is_err());
        for fine in [
            Some("cli/0.6.0"),
            Some("cli/0.6.0-1-gabc1234-dev"),
            Some("web/1.0.0"),
            Some("cli/dev"),
            Some("cli/abc1234"),
            Some("something else"),
            None,
        ] {
            assert!(check(fine, oldest).is_ok(), "{fine:?}");
        }
    }
}
