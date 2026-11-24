//! Apps too old for this server are told to update, rather than misbehave
//! (docs/protocol.md §11). Every app says which it is and its version with
//! each request ([`APP_HEADER`]) and in the WebSocket's `auth` message.

use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use notekpr_core::protocol::APP_HEADER;
use notekpr_core::version::{Build, Version};

use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// The oldest version of the apps this build works with. The release that
/// stops working with older apps raises it (docs/development.md §Releases):
/// those are then told to update. Administrators can raise it further
/// (`oldest_app_version`, [`crate::instance::Instance::oldest_app`]).
///
/// 0.21.0: limits per person (docs/protocol.md §9). Older apps don't know
/// `write_limit` or `suspended`: they keep changes the server holds back
/// but send them only once they reconnect or are restarted, without saying
/// why, and a suspended one keeps reconnecting.
///
/// (0.17.0: sharing is by invitation, §5.6. Older apps add people to vaults
/// and notes directly, which the server refuses now, and don't add whoever
/// accepted an invitation.)
///
/// (0.8.0: vaults have several owners. Older apps check a vault's log
/// against the key of the owner the server names, which is no longer its
/// creator once they have gone, and the keys of shared notes against that
/// one owner only.)
pub const OLDEST_APP_VERSION: Version = Version(0, 21, 0);

/// Refuse an app older than `oldest` (docs/protocol.md §11).
pub fn check(app: Option<&str>, oldest: Version) -> AppResult<()> {
    match app.and_then(Build::of_app) {
        Some(app) if refused(Build::of(notekpr_core::VERSION), app, oldest) => {
            Err(AppError::UpdateRequired(oldest))
        }
        _ => Ok(()),
    }
}

/// Whether a server built as `server` refuses an app built as `app`.
/// Development builds are never refused, nor are requests that don't say
/// which app they are (scripts, apps from before the header). A build a few
/// commits after a release counts as that release, except on a server that
/// isn't a release either and was built after the same release or an
/// earlier one (or is a development build): the version it needs is then
/// one still to come, which both are on their way to. So a server built
/// from main between two releases takes the apps built beside it.
fn refused(server: Build, app: Build, oldest: Version) -> bool {
    match app {
        Build::Dev => false,
        Build::Release(v) => v < oldest,
        Build::After(v) => {
            v < oldest
                && match server {
                    Build::Release(_) => true,
                    Build::After(s) => s > v,
                    Build::Dev => false,
                }
        }
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
        let released = Build::Release(Version(0, 6, 0));
        let refused_by = |server, app: &str| refused(server, Build::of(app), oldest);
        for server in [released, Build::After(Version(0, 5, 0)), Build::Dev] {
            assert!(refused_by(server, "0.5.9"), "{server:?}");
            assert!(refused_by(server, "0.1.0"), "{server:?}");
            for fine in ["0.6.0", "0.6.0-1-gabc1234-dev", "1.0.0", "dev", "abc1234"] {
                assert!(!refused_by(server, fine), "{server:?} {fine}");
            }
        }
        // A build after a release counts as that release on a released
        // server, and on one built after a later release…
        assert!(refused_by(released, "0.5.0-3-gabc1234"));
        assert!(refused_by(
            Build::After(Version(0, 5, 1)),
            "0.5.0-3-gabc1234"
        ));
        // …but not on a server that needs a release still to come: one built
        // after the same release, or an earlier one, or a development build.
        for server in [
            Build::After(Version(0, 5, 0)),
            Build::After(Version(0, 4, 2)),
            Build::Dev,
        ] {
            assert!(!refused_by(server, "0.5.0-3-gabc1234"), "{server:?}");
            assert!(!refused_by(server, "0.5.0-dev"), "{server:?}");
        }
        // Requests that don't say which app they are.
        for fine in [Some("something else"), None] {
            assert!(check(fine, oldest).is_ok(), "{fine:?}");
        }
    }
}
