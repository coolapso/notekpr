//! Telling people a newer release is out (docs/desktop.md "Update it"). The
//! app never updates itself: it says so, and they install the new version
//! the way they installed the app. What it compares its own version with is
//! the open account's server's (`GET /info`), at start and every hour —
//! nothing is asked of anyone else, so nobody hears of a release before
//! their server runs it. A newer one shows in the window (a bar, until
//! dismissed for that version), in the tray's menu and in the app's
//! settings. With notes only on this computer the server is the app's own:
//! there is nothing to ask.

use std::sync::Arc;
use std::time::Duration;

use notekpr_cli::api::{Api, ApiError, Credentials};
use notekpr_core::version::{Build, newer_release};
use serde::Serialize;
use tauri::AppHandle;

use crate::app::AppState;

/// How to update, on each system: the docs' page.
pub const HELP: &str = "https://docs.notekpr.md/tools/desktop/#update-it";

/// How often the server is asked again: it may be updated while the app
/// runs.
const EVERY: Duration = Duration::from_secs(60 * 60);

/// A newer release, as the window hears of it.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Update {
    /// `0.24.0`.
    pub version: String,
    /// Its bar was dismissed in the window; the tray and the settings still
    /// say it.
    pub dismissed: bool,
}

/// What to tell, given this build's version, its server's, and the release
/// whose bar was dismissed last.
pub fn notice(app: &str, server: &str, dismissed: Option<&str>) -> Option<Update> {
    let version = newer_release(Build::of(app), Build::of(server))?.to_string();
    Some(Update {
        dismissed: dismissed == Some(version.as_str()),
        version,
    })
}

/// Browser tests can't build an app and a server of other versions: with
/// `NOTEKPR_DESKTOP_TEST_VERSIONS=<app>,<server>` the check takes them to
/// be those (the server is still asked). Debug builds only — release builds
/// have no such thing.
#[cfg(debug_assertions)]
fn test_versions() -> Option<(String, String)> {
    let v = std::env::var("NOTEKPR_DESKTOP_TEST_VERSIONS").ok()?;
    let (app, server) = v.split_once(',')?;
    Some((app.into(), server.into()))
}

#[cfg(not(debug_assertions))]
fn test_versions() -> Option<(String, String)> {
    None
}

/// Ask the open account's server for its version now and every [`EVERY`]
/// after, for as long as the app runs. A server that can't be reached
/// changes nothing: it is asked again later.
pub fn watch(app: AppHandle, state: Arc<AppState>) {
    let server = match (&state.local, &state.open) {
        (None, Some(open)) => open.server.clone(),
        _ => None,
    };
    let Some(server) = server else {
        return;
    };
    tauri::async_runtime::spawn(async move {
        let mut shown: Option<String> = None;
        loop {
            match server_version(&server).await {
                Ok(theirs) => {
                    let (own, theirs) = test_versions()
                        .unwrap_or_else(|| (notekpr_core::VERSION.to_string(), theirs));
                    let dismissed = state.settings.lock().unwrap().update_dismissed.clone();
                    let now = notice(&own, &theirs, dismissed.as_deref());
                    let version = now.as_ref().map(|u| u.version.clone());
                    state.updates.update(|s| s.update = now);
                    if version != shown {
                        crate::app::tray_update(&app, version.as_deref());
                        shown = version;
                    }
                }
                Err(e) => tracing::debug!("couldn't ask the server for its version: {}", why(&e)),
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}

/// The server's version, asked there only: a redirect, which would send
/// the app to another address, is an error.
async fn server_version(server: &str) -> Result<String, ApiError> {
    Ok(Api::without_redirects(server, Credentials::None)?
        .info()
        .await?
        .version)
}

/// Why the server couldn't be asked, in the app's words only: nothing the
/// server sent reaches the log.
fn why(e: &ApiError) -> String {
    match e {
        _ if e.is_redirect() => "it sends the app to another address".into(),
        ApiError::Status { status, .. } => format!("HTTP {status}"),
        ApiError::Network(_) => "it can't be reached".into(),
        ApiError::Decode(_) | ApiError::Auth(_) => "it answered unexpectedly".into(),
    }
}

/// The window's bar dismissed for `version`: not shown again for it. A
/// script in the window saying so only hides the bar.
pub fn dismiss(state: &AppState, version: &str) -> anyhow::Result<()> {
    let v = notekpr_core::version::Version::parse(version)
        .ok_or_else(|| anyhow::anyhow!("{version} isn't a release's version"))?
        .to_string();
    {
        let mut s = state.settings.lock().unwrap();
        s.update_dismissed = Some(v.clone());
        s.save(&state.paths.settings_file())?;
    }
    state.updates.update(|s| {
        if let Some(u) = s.update.as_mut().filter(|u| u.version == v) {
            u.dismissed = true;
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_to_tell() {
        assert_eq!(
            notice("0.23.0", "0.24.0", None),
            Some(Update {
                version: "0.24.0".into(),
                dismissed: false
            })
        );
        // Dismissed for that version: the bar stays away…
        assert_eq!(
            notice("0.23.0", "0.24.0", Some("0.24.0")),
            Some(Update {
                version: "0.24.0".into(),
                dismissed: true
            })
        );
        // …but comes back for the next.
        assert_eq!(
            notice("0.23.0", "0.25.0", Some("0.24.0")),
            Some(Update {
                version: "0.25.0".into(),
                dismissed: false
            })
        );
        // Nothing newer, or nothing to compare.
        assert_eq!(notice("0.24.0", "0.24.0", None), None);
        assert_eq!(notice("0.24.0", "0.23.0", None), None);
        assert_eq!(notice("dev", "0.24.0", None), None);
        assert_eq!(notice("0.23.0", "dev", None), None);
    }
}
