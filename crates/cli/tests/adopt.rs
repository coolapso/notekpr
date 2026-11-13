//! Another app on this computer handing its device over — the desktop
//! app's Markdown folder (docs/desktop.md): the same device and session,
//! checked against the account key the device was approved for, with the
//! account secret unwrapped from the device's own envelope.

mod common;

use common::*;
use notekpr_cli::account::{self, Adopt};
use notekpr_cli::api::{Api, Credentials};
use notekpr_cli::secrets::SecretStore;
use notekpr_cli::session::Session;
use notekpr_cli::{manage, ops};
use notekpr_core::protocol::BindDeviceRequest;
use zeroize::Zeroizing;

/// What the desktop app's window hands over for `s`'s device.
fn handoff(s: &Session) -> Adopt {
    let p = &s.profile;
    let user_id = p.user_id.clone().unwrap();
    let device_id = p.device_id.clone().unwrap();
    let device = SecretStore::open(&s.data_dir())
        .unwrap()
        .load_device(&user_id, &device_id)
        .unwrap()
        .unwrap();
    Adopt {
        server: p.server.clone(),
        display_name: p.display_name.clone().unwrap_or_default(),
        user_id,
        device_id,
        device_name: Some("desktop".into()),
        device_secret: Zeroizing::new(device.secret),
        account_public: s.account().public(),
        token: p.session_token.clone().unwrap(),
        expires_at: p.session_expires_at.unwrap(),
    }
}

/// Bind `s`'s session to its device, as the web app does when it opens.
async fn bind(s: &Session) {
    s.api
        .put::<_, ()>(
            "/me/session/device",
            &BindDeviceRequest {
                device_id: s.profile.device_id.clone().unwrap(),
            },
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_handed_over_device_syncs_as_the_same_device() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    manage::create_vault(&alice, "Notes").await.unwrap();
    let alice_hub = hub(&alice).await;
    let vid = alice_hub
        .resolve("Notes")
        .await
        .unwrap()
        .lock()
        .await
        .id()
        .to_string();
    ops::put_document(&alice_hub, &vid, "Plan.md", "the plan\n", None)
        .await
        .unwrap();
    bind(&alice).await;

    let desktop = srv.dirs("desktop");
    account::adopt(&desktop, "folder", handoff(&alice))
        .await
        .unwrap();
    let folder = std::sync::Arc::new(Session::open(&desktop, "folder").unwrap());
    assert_eq!(folder.account().public(), alice.account().public());
    assert_eq!(folder.profile.device_id, alice.profile.device_id);
    // One device for both: nothing new in the account's device list.
    let devices: Vec<notekpr_core::protocol::DeviceInfo> = alice.api.get("/devices").await.unwrap();
    assert_eq!(devices.len(), 1);
    let folder_hub = hub(&folder).await;
    assert_eq!(
        note(&folder_hub, "Notes", "Plan.md").await.as_deref(),
        Some("the plan\n")
    );

    // Handing it over again (a new session) is fine; forgetting removes
    // what the profile kept.
    account::adopt(&desktop, "folder", handoff(&alice))
        .await
        .unwrap();
    account::drop_session(&desktop, "folder").unwrap();
    assert!(
        Session::open(&desktop, "folder")
            .unwrap()
            .profile
            .session_token
            .is_none()
    );
    account::forget_locally(&desktop, "folder").unwrap();
    assert!(Session::open(&desktop, "folder").is_err());
    assert!(!desktop.profile_data("folder").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hand_over_that_doesnt_add_up_is_refused() {
    let srv = start().await;
    let alice = login(&srv, "alice").await;
    let bob = login(&srv, "bob").await;
    bind(&alice).await;
    bind(&bob).await;
    let desktop = srv.dirs("desktop");
    let refused = |a: Adopt| {
        let desktop = desktop.clone();
        async move {
            let e = account::adopt(&desktop, "folder", a).await.unwrap_err();
            assert!(Session::open(&desktop, "folder").is_err(), "nothing kept");
            format!("{e:#}")
        }
    };

    // The pin is another account's key (a server handing out its own).
    let mut a = handoff(&alice);
    a.account_public = bob.account().public();
    assert!(
        refused(a)
            .await
            .contains("differs from the one this device was approved for")
    );

    // A device key that isn't the device's.
    let mut a = handoff(&alice);
    a.device_secret = Zeroizing::new(notekpr_core::crypto::random_bytes::<32>());
    assert!(refused(a).await.contains("could not be decrypted"));

    // Someone else's session, or one of alice's not bound to this device.
    let mut a = handoff(&alice);
    a.token = bob.profile.session_token.clone().unwrap();
    assert!(refused(a).await.contains("someone else"));
    let unbound = Api::new(&srv.url, Credentials::None)
        .unwrap()
        .dev_login("alice")
        .await
        .unwrap();
    let mut a = handoff(&alice);
    a.token = unbound.token;
    assert!(refused(a).await.contains("isn't this device's"));

    // A profile of another account stays as it is.
    account::adopt(&desktop, "folder", handoff(&alice))
        .await
        .unwrap();
    let e = account::adopt(&desktop, "folder", handoff(&bob))
        .await
        .unwrap_err();
    assert!(format!("{e:#}").contains("another account"));
    assert_eq!(
        Session::open(&desktop, "folder").unwrap().profile.user_id,
        alice.profile.user_id
    );
}
