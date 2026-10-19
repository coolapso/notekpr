//! Shared helpers: an in-process server (development mode) and dev-login
//! clients with isolated config/data directories.
#![allow(dead_code)]

use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use coolpsonotes_client::account::{self, LoginOptions};
use coolpsonotes_client::hub::{Hub, HubOptions};
use coolpsonotes_client::livesync;
use coolpsonotes_client::profile::Dirs;
use coolpsonotes_client::session::Session;
use tokio::sync::watch;

pub struct TestServer {
    pub url: String,
    pub dir: tempfile::TempDir,
}

pub async fn start() -> TestServer {
    coolpsonotes_client::ui::set_quiet(std::env::var_os("TEST_LOG").is_none());
    // Never wait for a prompt, even when `cargo test` runs in a terminal.
    coolpsonotes_client::ui::set_non_interactive(true);
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_env("TEST_LOG"))
        .with_writer(std::io::stderr)
        .try_init();
    let dir = tempfile::tempdir().unwrap();
    let cfg = coolpsonotes_server::config::Config::for_tests(dir.path().join("server"));
    let (app, _state) = coolpsonotes_server::build(cfg, None).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    TestServer {
        url: format!("http://{addr}"),
        dir,
    }
}

impl TestServer {
    pub fn dirs(&self, who: &str) -> Dirs {
        Dirs::under(&self.dir.path().join("clients").join(who))
    }

    pub fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }
}

/// Dev-login `name` into profile `name` (first login creates the account).
pub async fn login(srv: &TestServer, name: &str) -> Arc<Session> {
    login_profile(srv, name, name).await
}

pub async fn login_profile(srv: &TestServer, dirs_name: &str, user: &str) -> Arc<Session> {
    let dirs = srv.dirs(dirs_name);
    let out = account::login(
        &dirs,
        user,
        LoginOptions {
            server: srv.url.clone(),
            dev_user: Some(user.to_string()),
            device_name: Some(format!("{user}'s test device")),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(out.user.display_name.eq_ignore_ascii_case(user));
    Arc::new(Session::open(&dirs, user).unwrap())
}

pub async fn reopen(srv: &TestServer, dirs_name: &str, profile: &str) -> Arc<Session> {
    Arc::new(Session::open(&srv.dirs(dirs_name), profile).unwrap())
}

pub async fn hub(session: &Arc<Session>) -> Arc<Hub> {
    Hub::start(
        session.clone(),
        HubOptions {
            catch_up: true,
            offline_ok: true,
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

pub struct Live {
    pub hub: Arc<Hub>,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Live {
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(5), self.task).await;
    }
}

/// Run the WebSocket sync loop for a hub and wait until it is connected.
pub async fn live(hub: Arc<Hub>) -> Live {
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(livesync::run(hub.clone(), rx));
    let mut ev = hub.subscribe();
    let connected = async {
        while !hub.is_online() {
            let _ = ev.recv().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), connected)
        .await
        .expect("live sync did not connect");
    // Give the initial subscriptions a moment to reach `synced`.
    tokio::time::sleep(Duration::from_millis(200)).await;
    Live { hub, stop, task }
}

/// Poll `f` until it returns true (or panic after `secs`).
pub async fn eventually<F, Fut>(secs: u64, what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        if f().await {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("timed out waiting for: {what}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Text of a note in a hub's vault (None if missing).
pub async fn note(hub: &Hub, vault: &str, path: &str) -> Option<String> {
    let v = hub.resolve(vault).await.ok()?;
    let g = v.lock().await;
    let (_, e) = g.find(path)?;
    Some(g.note_text(&e.id))
}
