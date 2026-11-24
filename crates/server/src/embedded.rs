//! The desktop app's own server, for notes kept only on that computer
//! (docs/desktop.md, "Only on this computer"; docs/security.md, "The
//! desktop app"; docs/protocol.md §3): this server's code, run inside the
//! app, for one account. What differs from the server's program:
//!
//! * It listens on `127.0.0.1` only, at a port the system chose at start
//!   ([`bind`]). Every request whose `Host` isn't exactly that address gets
//!   `421`, so no name that resolves to it (DNS rebinding) reaches it; one
//!   from a web page (an `Origin` other than the app's) gets `403`.
//! * Nothing signs in: no OIDC, no development mode, and none of the routes
//!   to sign in, for agents or administration. The app makes the account's
//!   sessions itself, in its own process ([`Embedded::session`]).
//! * Admin-less, no metrics, no web files.
//!
//! Only the app builds it, through [`Embedded::start`], which fixes all of
//! that: nothing in [`Config`], its flags or the environment turns any of
//! it on, and the server's program never calls it.

use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, bail};
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use notekpr_core::protocol::{InstanceSettingsUpdate, SessionResponse};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::config::{Config, LogFormat, TokenAuth};
use crate::routes::{APP_ORIGINS, Mount};
use crate::state::AppState;

/// The one account's identity (issuer and subject, as an identity
/// provider's would be). No route signs anyone in with it.
pub const ISSUER: &str = "notekpr:this-computer";
const SUBJECT: &str = "this-computer";

/// What its sessions are called where sessions are listed.
const CLIENT: &str = "this computer";

/// How long [`Embedded::stop`] waits for the server to finish.
const STOP_WITHIN: Duration = Duration::from_secs(5);

/// A listening address on `127.0.0.1`, at a port the system chose: taken
/// before the server starts, so the app knows the address first (its
/// window's Content-Security-Policy names it).
pub fn bind() -> std::io::Result<TcpListener> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
}

pub struct Embedded {
    state: AppState,
    origin: String,
    stop: Mutex<Option<oneshot::Sender<()>>>,
    task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

impl Embedded {
    /// Serve on `listener` ([`bind`]), with the database and attachments
    /// in `data_dir`.
    pub async fn start(data_dir: PathBuf, listener: TcpListener) -> anyhow::Result<Embedded> {
        let addr = listener.local_addr()?;
        if addr.ip() != Ipv4Addr::LOCALHOST {
            bail!("the app's own server listens on 127.0.0.1 only, not {addr}");
        }
        let origin = format!("http://{addr}");
        let state = crate::open(config(data_dir, addr, &origin), None).await?;
        if state.write_lock.is_writer() {
            // One person's window and Markdown folder, from one address:
            // no reason to make them wait.
            let _w = state.write_lock.lock().await?;
            state
                .instance
                .update(
                    &state.db,
                    &InstanceSettingsUpdate {
                        rate_limit_rps: Some(10_000),
                        person_rate_limit_rps: Some(10_000),
                        ..Default::default()
                    },
                )
                .await?;
        }
        let host: Arc<str> = addr.to_string().into();
        let app = crate::routes::routes(state.clone(), Mount::NONE)
            .layer(from_fn_with_state(host, only_the_app));
        listener.set_nonblocking(true)?;
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn({
            let state = state.clone();
            async move {
                let served = axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .with_graceful_shutdown(async move {
                    let _ = stopped.await;
                    state.hand_off().await;
                })
                .await;
                if let Err(e) = served {
                    tracing::error!(error = %e, "the app's own server stopped");
                }
            }
        });
        tracing::info!(
            version = notekpr_core::VERSION,
            "the app's own server is ready"
        );
        Ok(Embedded {
            state,
            origin,
            stop: Mutex::new(Some(stop)),
            task: tokio::sync::Mutex::new(Some(task)),
        })
    }

    /// Where it listens: `http://127.0.0.1:<port>`.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// A new session of the account (made the first time), for the app's
    /// window or its Markdown folder: what signing in gives elsewhere.
    pub async fn session(&self, display_name: &str) -> anyhow::Result<SessionResponse> {
        let state = &self.state;
        if !state.write_lock.is_writer() {
            bail!("another copy of the app keeps these notes open");
        }
        let user_id =
            crate::auth::provision_user(state, ISSUER, SUBJECT, display_name, None).await?;
        let s = crate::auth::create_session(
            &state.db,
            &user_id,
            "web",
            Some(CLIENT),
            state.instance.settings().session_ttl_ms(),
        )
        .await?;
        Ok(SessionResponse {
            token: s.token,
            expires_at: s.expires_at,
            user: crate::auth::user_info(state, &user_id)
                .await
                .context("the account")?,
        })
    }

    /// Stop serving: live connections are told it's a restart, and what is
    /// under way finishes (for a few seconds at most).
    pub async fn stop(&self) {
        if let Some(stop) = self.stop.lock().unwrap().take() {
            let _ = stop.send(());
        }
        if let Some(mut task) = self.task.lock().await.take()
            && tokio::time::timeout(STOP_WITHIN, &mut task).await.is_err()
        {
            tracing::warn!("the app's own server took too long to stop: cut short");
            task.abort();
        }
    }
}

/// Everything fixed: no setting of the server's own decides any of it.
fn config(data_dir: PathBuf, listen: SocketAddr, origin: &str) -> Config {
    Config {
        listen,
        data_dir,
        public_url: origin.to_string(),
        web_dir: None,
        oidc_issuer: None,
        oidc_client_id: None,
        oidc_client_secret: None,
        oidc_scopes: String::new(),
        oidc_token_auth: TokenAuth::Auto,
        oidc_ca_file: None,
        oidc_allow_unverified_email: false,
        oidc_display_name: String::new(),
        dev_mode: false,
        dev_mode_anywhere: false,
        demo_resets_at: None,
        admins: vec![],
        first_user_admin: false,
        adminless: true,
        file_store: None,
        trust_proxy: false,
        metrics: false,
        metrics_token: None,
        log_format: LogFormat::Pretty,
    }
}

/// Requests addressed to this server's own address (`Host`, or HTTP/2's
/// authority), and not from a web page: no `Origin`, or the app's.
async fn only_the_app(State(host): State<Arc<str>>, req: Request, next: Next) -> Response {
    let to = match req.headers().get(header::HOST) {
        Some(h) => h.to_str().ok(),
        None => req.uri().authority().map(|a| a.as_str()),
    };
    if to != Some(&*host) {
        return (StatusCode::MISDIRECTED_REQUEST, "not this server").into_response();
    }
    if let Some(origin) = req.headers().get(header::ORIGIN)
        && !APP_ORIGINS.iter().any(|o| origin == o)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(req).await
}
