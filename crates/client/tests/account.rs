//! Signing in: first-device setup and device removal.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use common::*;
use coolpsonotes_client::account::{self, LoginOptions};
use coolpsonotes_client::session::Session;

fn dev_login(srv_url: &str, user: &str) -> LoginOptions {
    LoginOptions {
        server: srv_url.to_string(),
        dev_user: Some(user.to_string()),
        device_name: Some(format!("{user}'s test device")),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_interrupted_first_device_setup_is_finished_by_the_next_login() {
    let srv = start().await;
    // The upload of the new account key reaches the server, but its answer
    // is lost (a proxy hiccup): the key is on the server, the client saw an
    // error. Before, the secret was saved only after a successful upload, so
    // this account had no secret anywhere.
    let failed = Arc::new(AtomicBool::new(false));
    let f = failed.clone();
    let url = proxy(
        &srv.url,
        Arc::new(move |m, path| {
            (m == axum::http::Method::PUT
                && path.ends_with("/me/account-key")
                && !f.swap(true, Ordering::SeqCst))
            .then_some(Fault::LoseResponse(502))
        }),
    )
    .await;
    let dirs = srv.dirs("alice");
    let err = account::login(&dirs, "alice", dev_login(&url, "alice"))
        .await
        .unwrap_err();
    assert!(failed.load(Ordering::SeqCst), "{err:#}");

    // The keys were kept: the next login uploads them and shows the recovery
    // key (it was never shown).
    let out = account::login(&dirs, "alice", dev_login(&url, "alice"))
        .await
        .unwrap();
    let rk = out.recovery_key.clone().expect("recovery key shown again");
    let s = Session::open(&dirs, "alice").unwrap();
    assert_eq!(s.fingerprint(), out.fingerprint);
    let me = s.api.me().await.unwrap();
    assert_eq!(
        me.account_key.unwrap().verify().unwrap().fingerprint(),
        out.fingerprint
    );
    // Until confirmed as shown, it stays available; afterwards not.
    let again = account::login(&dirs, "alice", dev_login(&url, "alice"))
        .await
        .unwrap();
    assert_eq!(again.recovery_key.as_deref(), Some(rk.as_str()));
    account::recovery_key_shown(&dirs, "alice").unwrap();
    let later = account::login(&dirs, "alice", dev_login(&url, "alice"))
        .await
        .unwrap();
    assert!(later.recovery_key.is_none() && later.reused_device);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn login_keeps_the_account_secret_unless_the_device_was_really_removed() {
    use coolpsonotes_client::profile::ProfileStore;
    let srv = start().await;
    let a_dirs = srv.dirs("alice-a");
    let first = account::login(&a_dirs, "alice", dev_login(&srv.url, "alice"))
        .await
        .unwrap();
    let rk = first.recovery_key.clone().unwrap();
    let a = Session::open(&a_dirs, "alice").unwrap();
    let b_dirs = srv.dirs("alice-b");
    let with_rk = |url: &str| LoginOptions {
        recovery_key: Some(rk.clone()),
        approval_timeout: Some(std::time::Duration::from_secs(1)),
        ..dev_login(url, "alice")
    };
    let b = account::login(&b_dirs, "alice", with_rk(&srv.url))
        .await
        .unwrap();
    assert!(b.recovered);
    let b_data = b_dirs.profile_data("alice");
    let b_store = ProfileStore::new(&b_dirs, "alice").unwrap();

    // A 404 that is not a removal (here: a misconfigured proxy) changes
    // nothing: the device keys stay.
    let url = proxy(
        &srv.url,
        Arc::new(|m, path| {
            (m == axum::http::Method::PUT && path.ends_with("/me/session/device"))
                .then_some(Fault::Reply(404))
        }),
    )
    .await;
    b_store.update(|p| p.server = url.clone()).unwrap();
    let no_rk = LoginOptions {
        approval_timeout: Some(std::time::Duration::from_secs(1)),
        ..dev_login(&url, "alice")
    };
    assert!(account::login(&b_dirs, "alice", no_rk).await.is_err());
    assert!(b_data.join("account.sealed").exists());
    assert_eq!(
        b_store.require().unwrap().device_id.as_deref(),
        Some(b.device_id.as_str())
    );
    b_store.update(|p| p.server = srv.url.clone()).unwrap();

    // Really removed from another device: B enrolls again, and its old
    // secrets are moved aside, not deleted.
    coolpsonotes_client::manage::revoke_device(&a, &b.device_id)
        .await
        .unwrap();
    let again = account::login(&b_dirs, "alice", with_rk(&srv.url))
        .await
        .unwrap();
    assert!(again.recovered);
    assert_ne!(again.device_id, b.device_id);
    let aside: Vec<String> = std::fs::read_dir(&b_data)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("account.sealed.removed-"))
        .collect();
    assert_eq!(aside.len(), 1, "{aside:?}");
    assert!(Session::open(&b_dirs, "alice").is_ok());
}
