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
