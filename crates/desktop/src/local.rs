//! Notes only on this computer (docs/desktop.md): the server inside the
//! app (`notekpr_server::embedded`), with its data in the profile's data
//! folder ([`crate::paths::Paths::this_computer`]). Its address is taken
//! before the window exists, whose Content-Security-Policy names it; the
//! server starts once this is the only copy of the app running, and stops
//! when the app quits, after the Markdown folder. One that failed to start
//! (its folder unwritable, say) is tried again when the window asks again,
//! at the same address.

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use notekpr_core::protocol::SessionResponse;
use notekpr_server::embedded::Embedded;
use tokio::sync::Mutex;

pub struct Local {
    origin: String,
    dir: PathBuf,
    /// Kept for as long as the app runs: each try to start gets a copy,
    /// so the address stays the one the window's policy names.
    listener: TcpListener,
    server: Mutex<Option<Arc<Embedded>>>,
}

impl Local {
    /// Take its address, on `127.0.0.1`; its data goes in `dir`.
    pub fn bind(dir: PathBuf) -> Result<Local> {
        let listener = notekpr_server::embedded::bind().context("listening on 127.0.0.1")?;
        Ok(Local {
            origin: format!("http://{}", listener.local_addr()?),
            dir,
            listener,
            server: Mutex::new(None),
        })
    }

    /// `http://127.0.0.1:<port>`, another port at each start.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// The server, started the first time it's needed — or again, after
    /// it failed to.
    async fn server(&self) -> Result<Arc<Embedded>, String> {
        let mut server = self.server.lock().await;
        if let Some(s) = &*server {
            return Ok(s.clone());
        }
        let started = async {
            let listener = self.listener.try_clone()?;
            Embedded::start(self.dir.clone(), listener).await
        }
        .await
        .map_err(|e| format!("Your notes on this computer couldn't be opened: {e:#}"))?;
        let started = Arc::new(started);
        *server = Some(started.clone());
        Ok(started)
    }

    pub async fn start(&self) -> Result<(), String> {
        self.server().await.map(|_| ())
    }

    /// A session of the account, for the window: what signing in gives on
    /// a server.
    pub async fn session(&self) -> Result<SessionResponse, String> {
        self.server()
            .await?
            .session(&display_name())
            .await
            .map_err(|e| format!("{e:#}"))
    }

    /// Stop it, if it started; asked again, it starts again.
    pub async fn stop(&self) {
        let server = self.server.lock().await.take();
        if let Some(server) = server {
            server.stop().await;
        }
    }
}

/// The account's name, beside its comments and in history: the person's
/// own on this computer.
fn display_name() -> String {
    ["USER", "USERNAME", "LOGNAME"]
        .iter()
        .find_map(|v| std::env::var(v).ok())
        .map(|n| n.trim().chars().take(100).collect::<String>())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Me".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sessions_of_one_account_at_an_address_known_first() {
        let dir = tempfile::tempdir().unwrap();
        let local = Local::bind(dir.path().join("this-computer")).unwrap();
        let origin = local.origin().to_string();
        assert!(origin.starts_with("http://127.0.0.1:"));
        let a = local.session().await.unwrap();
        let b = local.session().await.unwrap();
        assert_eq!(a.user.id, b.user.id);
        assert_ne!(a.token, b.token);
        let info = reqwest::get(format!("{origin}/api/v1/info")).await.unwrap();
        assert_eq!(info.status(), 200);
        local.stop().await;
        assert!(dir.path().join("this-computer/notekpr.db").exists());
        // Stopped, it starts again when asked (a switch that failed).
        local.session().await.unwrap();
        let info = reqwest::get(format!("{origin}/api/v1/info")).await.unwrap();
        assert_eq!(info.status(), 200);
        local.stop().await;
    }

    #[tokio::test]
    async fn one_that_failed_to_start_starts_when_asked_again() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("this-computer");
        // Where its folder should be, a file: it can't open its database.
        std::fs::write(&data, b"").unwrap();
        let local = Local::bind(data.clone()).unwrap();
        let origin = local.origin().to_string();
        let e = local.start().await.unwrap_err();
        assert!(e.contains("couldn't be opened"), "{e}");
        assert!(local.session().await.is_err());

        std::fs::remove_file(&data).unwrap();
        local.session().await.unwrap();
        assert_eq!(local.origin(), origin);
        let info = reqwest::get(format!("{origin}/api/v1/info")).await.unwrap();
        assert_eq!(info.status(), 200, "at the address the window knows");
        local.stop().await;
    }
}
