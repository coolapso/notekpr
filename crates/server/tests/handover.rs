//! An update on one machine (docs/operations.md, "Updates without
//! downtime"): the next version of the server starts beside the running one,
//! on the same database, and takes over writing when that one is told to
//! stop. Real processes, with the locks between them, as between
//! containers sharing a volume.
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::*;
use notekpr_core::doc::NoteDoc;
use notekpr_core::protocol::*;

struct Server {
    child: Child,
    /// `http://…/api/v1`
    base: String,
    log: PathBuf,
}

impl Server {
    async fn start(dir: &Path, name: &str) -> Server {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let log = dir.join(format!("{name}.log"));
        let out = std::fs::File::create(&log).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_notekpr-server"));
        for (k, _) in std::env::vars() {
            if k.starts_with("NOTEKPR_") {
                cmd.env_remove(k);
            }
        }
        let child = cmd
            .args(["--dev-mode", "--data-dir"])
            .arg(dir.join("data"))
            .args(["--listen", &format!("127.0.0.1:{port}")])
            .args(["--public-url", &format!("http://127.0.0.1:{port}")])
            .stdout(Stdio::from(out.try_clone().unwrap()))
            .stderr(Stdio::from(out))
            .spawn()
            .unwrap();
        let s = Server {
            child,
            base: format!("http://127.0.0.1:{port}/api/v1"),
            log,
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        while s.ready().await != Some(200) {
            assert!(
                Instant::now() < deadline,
                "{name} never ready: {}",
                s.logs()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        s
    }

    async fn ready(&self) -> Option<u16> {
        let url = format!("{}/readyz", self.base.trim_end_matches("/api/v1"));
        let r = reqwest::get(url).await.ok()?;
        Some(r.status().as_u16())
    }

    fn logs(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn terminate(&self) {
        // SAFETY: a signal to our own child.
        assert_eq!(
            unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM) },
            0
        );
    }

    /// Wait for it to end on its own; how long that took.
    fn wait(&mut self, at_most: Duration) -> Duration {
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "{status}: {}", self.logs());
                return start.elapsed();
            }
            assert!(start.elapsed() < at_most, "still running: {}", self.logs());
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test]
async fn a_stopping_server_hands_over_to_the_next_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut old = Server::start(dir.path(), "old").await;
    let mut alice = new_user(&old.base, "alice").await;
    let v = alice.create_vault("Home").await;
    let note = NoteDoc::new();
    let u = note.set_text("before the update").unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n", &u)])
        .await
        .unwrap();

    // Ready while the old one writes; a change waits for it to stop.
    let new = Server::start(dir.path(), "new").await;
    let on_new = Api {
        base: new.base.clone(),
        ..alice.api.clone()
    };
    let push = format!("/vaults/{v}/updates");
    let u = note.set_text("during the update").unwrap();
    let held = tokio::spawn({
        let (api, push) = (on_new.clone(), push.clone());
        let body = PushRequest {
            updates: vec![alice.encrypt_update(&v, "n", &u)],
        };
        async move {
            api.call::<_, PushResponse>(reqwest::Method::POST, &push, Some(&body))
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!held.is_finished());

    old.terminate();
    let pushed = held.await.unwrap().unwrap();
    assert!(pushed.rejected.is_empty());
    // It answers a moment longer, while proxies move to the next one:
    // reads, and changes are told to try again.
    let vault = format!("/vaults/{v}");
    assert_eq!(
        alice
            .api
            .status::<()>(reqwest::Method::GET, &vault, None)
            .await,
        200
    );
    let refused = alice
        .api
        .req(reqwest::Method::POST, &push)
        .json(&PushRequest {
            updates: vec![alice.encrypt_update(&v, "m", &u)],
        })
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status().as_u16(), 503);
    assert_eq!(refused.headers()["retry-after"], "1");
    assert_eq!(old.ready().await, Some(503));
    old.wait(Duration::from_secs(20));
    let logs = old.logs();
    assert!(
        logs.contains("handing over to the server waiting"),
        "{logs}"
    );
    assert!(new.logs().contains("took over writing"), "{}", new.logs());

    alice.api = on_new;
    let u = note.set_text("after the update").unwrap();
    alice
        .push(&v, vec![alice.encrypt_update(&v, "n", &u)])
        .await
        .unwrap();
    let (_, notes) = alice.read_vault(&v).await;
    assert_eq!(notes["n"].text(), "after the update");
}

#[tokio::test]
async fn a_server_nobody_takes_over_from_stops_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut only = Server::start(dir.path(), "only").await;
    only.terminate();
    let took = only.wait(Duration::from_secs(20));
    assert!(took < Duration::from_secs(4), "{took:?}: {}", only.logs());
    // Started again (as Compose or systemd do): it writes at once.
    let again = Server::start(dir.path(), "again").await;
    new_user(&again.base, "alice").await;
}
