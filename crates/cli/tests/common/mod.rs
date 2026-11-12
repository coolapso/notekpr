//! Shared helpers: an in-process server (development mode) and dev-login
//! clients with isolated config/data directories.
#![allow(dead_code)]

use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use notekpr_cli::account::{self, LoginOptions};
use notekpr_cli::hub::{Hub, HubOptions};
use notekpr_cli::livesync;
use notekpr_cli::profile::Dirs;
use notekpr_cli::session::Session;
use notekpr_core::doc::Picture;
use tokio::sync::watch;

pub struct TestServer {
    pub url: String,
    pub dir: tempfile::TempDir,
    /// The server's state: tests that play a compromised server change its
    /// database directly.
    pub state: notekpr_server::state::AppState,
}

pub async fn start() -> TestServer {
    start_with(|_| {}).await
}

/// [`start`] with a changed server configuration (e.g. smaller limits).
pub async fn start_with(configure: impl FnOnce(&mut notekpr_server::config::Config)) -> TestServer {
    notekpr_cli::ui::set_quiet(std::env::var_os("TEST_LOG").is_none());
    // Never wait for a prompt, even when `cargo test` runs in a terminal.
    notekpr_cli::ui::set_non_interactive(true);
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_env("TEST_LOG"))
        .with_writer(std::io::stderr)
        .try_init();
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = notekpr_server::config::Config::for_tests(dir.path().join("server"));
    configure(&mut cfg);
    let (app, state) = notekpr_server::build_for_tests(cfg).await.unwrap();
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
        state,
    }
}

impl TestServer {
    /// Change instance settings, as an administrator does while it runs.
    pub async fn set(&self, change: notekpr_core::protocol::InstanceSettingsUpdate) {
        self.state
            .instance
            .update(&self.state.db, &change)
            .await
            .unwrap();
    }

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
    let s = Arc::new(Session::open(&dirs, user).unwrap());
    people()
        .lock()
        .unwrap()
        .insert((s.profile.server.clone(), user.to_string()), s.clone());
    s
}

/// The sessions made by [`login_profile`], by user name: what accepts
/// invitations for them (so `share` can take just a name, as before
/// invitations). The latest login of a name wins.
type People = std::collections::BTreeMap<(String, String), Arc<Session>>;

fn people() -> &'static std::sync::Mutex<People> {
    static PEOPLE: std::sync::OnceLock<std::sync::Mutex<People>> = std::sync::OnceLock::new();
    PEOPLE.get_or_init(Default::default)
}

/// Each test has its own server: people are told apart by server too.
fn person(owner: &Session, name: &str) -> Option<Arc<Session>> {
    people()
        .lock()
        .unwrap()
        .get(&(owner.profile.server.clone(), name.to_string()))
        .cloned()
}

/// Share vault `v` with `who` as the apps do: one's agents (and members, a
/// role) at once; someone else by invitation, which they accept (with
/// their session from [`login`]) and the owner's app completes.
pub async fn share(
    owner: &Session,
    v: &notekpr_cli::session::SharedVault,
    who: &str,
    role: notekpr_core::protocol::Role,
    accept_new_key: bool,
) -> anyhow::Result<notekpr_cli::manage::ShareOutcome> {
    use notekpr_cli::manage::{self, Shared};
    use notekpr_core::protocol::Role;
    let reference = match person(owner, who) {
        Some(_) => format!("{who}@{}", notekpr_core::protocol::DEV_EMAIL_DOMAIN),
        None => who.to_string(),
    };
    // Owners are made from members.
    let joins_as = if role == Role::Owner {
        Role::Editor
    } else {
        role.clone()
    };
    match manage::share_or_invite(owner, v, &reference, joins_as, accept_new_key).await? {
        Shared::Done(out) if role != Role::Owner || out.updated_role => Ok(out),
        Shared::Done(_) => manage::share(owner, v, &reference, role, accept_new_key).await,
        Shared::Invited(_) => {
            let out = finish(owner, v, None, who, accept_new_key).await?;
            if role == Role::Owner {
                return manage::share(owner, v, &reference, role, accept_new_key).await;
            }
            Ok(out)
        }
    }
}

/// [`share`] for one note: `note_shares::share_or_invite`, then the same.
pub async fn share_note(
    owner: &Session,
    v: &notekpr_cli::session::SharedVault,
    path: &str,
    who: &str,
    role: notekpr_core::protocol::Role,
    accept_new_key: bool,
) -> anyhow::Result<notekpr_cli::manage::ShareOutcome> {
    use notekpr_cli::note_shares::{self, NoteShared};
    let reference = match person(owner, who) {
        Some(_) => format!("{who}@{}", notekpr_core::protocol::DEV_EMAIL_DOMAIN),
        None => who.to_string(),
    };
    // (Its comments go with it too, unless a test carries them itself.)
    match note_shares::share_or_invite(owner, v, path, &reference, role, accept_new_key, false)
        .await?
    {
        NoteShared::Done(out) => Ok(out),
        NoteShared::Invited(_) => {
            let note = v.lock().await.find_note(path)?.1.id;
            finish(owner, v, Some(&note), who, accept_new_key).await
        }
    }
}

/// `who` accepts the owner's invitation to `v` (or its note), and the
/// owner's app adds them.
async fn finish(
    owner: &Session,
    v: &notekpr_cli::session::SharedVault,
    note: Option<&str>,
    who: &str,
    accept_new_key: bool,
) -> anyhow::Result<notekpr_cli::manage::ShareOutcome> {
    use notekpr_cli::invitations;
    use notekpr_core::engine::PinStatus;
    let invitee = person(owner, who).expect("someone made by login");
    let vid = v.lock().await.id().to_string();
    let theirs = invitations::list(&invitee).await?;
    let inv = theirs
        .received
        .iter()
        .find(|i| i.vault_id == vid && i.object_id.as_deref() == note && !i.accepted)
        .ok_or_else(|| anyhow::anyhow!("{who} got no invitation"))?;
    invitations::accept(&invitee, &inv.id).await?;
    let list = invitations::list(owner).await?;
    let sent = list
        .sent
        .iter()
        .find(|i| i.id == inv.id)
        .ok_or_else(|| anyhow::anyhow!("the invitation is gone"))?;
    let user = sent.accepted.as_ref().expect("accepted").user.clone();
    let fp = user
        .account_key
        .as_ref()
        .unwrap()
        .verify()
        .unwrap()
        .fingerprint();
    let before = owner.profiles.update(|p| p.pins.get(&user.id).cloned())?;
    if accept_new_key {
        owner.accept_pin(&user.id, &fp)?;
    }
    let done = invitations::complete(owner, std::slice::from_ref(v), &list).await?;
    if !done.key_changed.is_empty() {
        anyhow::bail!("the account key of {who} CHANGED since it was pinned");
    }
    if done.added.is_empty() {
        // Another app of the owner's (a running sync) may have added them.
        let path = match note {
            Some(o) => format!("/vaults/{vid}/objects/{o}/shares"),
            None => format!("/vaults/{vid}/members"),
        };
        let mut in_it = false;
        for _ in 0..50 {
            let people: Vec<serde_json::Value> = owner.api.get(&path).await?;
            if people.iter().any(|p| p["user"]["id"] == user.id.as_str()) {
                in_it = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        anyhow::ensure!(in_it, "{who} wasn't added");
        owner.reload_member_log(v).await?;
    }
    Ok(notekpr_cli::manage::ShareOutcome {
        user,
        pin: Some(match before {
            None => PinStatus::New,
            Some(p) if p == fp => PinStatus::Match,
            Some(_) => PinStatus::Changed,
        }),
        fingerprint: Some(fp),
        key_shared: true,
        updated_role: false,
    })
}

pub async fn reopen(srv: &TestServer, dirs_name: &str, profile: &str) -> Arc<Session> {
    let s = Arc::new(Session::open(&srv.dirs(dirs_name), profile).unwrap());
    // (A person's first session is the one that answers invitations.)
    people()
        .lock()
        .unwrap()
        .entry((s.profile.server.clone(), profile.to_string()))
        .or_insert_with(|| s.clone());
    s
}

/// Give the note or folder at `path` a picture as its icon, as the web app
/// does (the command line sets only emoji), and tell the people a note is
/// shared with.
pub async fn set_picture(hub: &Hub, vault: &str, path: &str, picture: &Picture) {
    let v = hub.resolve(vault).await.unwrap();
    let (vault_id, id) = {
        let mut g = v.lock().await;
        let id = g.set_picture(path, picture).unwrap();
        g.save().unwrap();
        (g.id().to_string(), id)
    };
    hub.local_changed(&vault_id, vec![], true).await;
    notekpr_cli::note_shares::refresh_meta(&hub.session, &v, &id)
        .await
        .unwrap();
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

/// What a [`proxy`] does to a request instead of forwarding it normally.
#[derive(Clone, Copy, Debug)]
pub enum Fault {
    /// Answer with this status (empty body) without forwarding.
    Reply(u16),
    /// Forward it, but answer with this status: the response is "lost".
    LoseResponse(u16),
}

/// Decides the fate of a request passing through a [`proxy`].
pub type Rule = Arc<dyn Fn(&axum::http::Method, &str) -> Option<Fault> + Send + Sync>;

/// An HTTP proxy in front of `target` (REST only, no WebSocket) that can
/// fail chosen requests — a reverse proxy answering 502, a lost upload.
pub async fn proxy(target: &str, rule: Rule) -> String {
    use axum::body::Body;
    use axum::extract::Request;
    use axum::response::Response;
    let target = target.to_string();
    let http = reqwest::Client::new();
    let app = axum::Router::new().fallback(move |req: Request| {
        let (target, http, rule) = (target.clone(), http.clone(), rule.clone());
        async move {
            let method = req.method().clone();
            let path = req
                .uri()
                .path_and_query()
                .map(|p| p.as_str().to_string())
                .unwrap_or_default();
            let fault = rule(&method, req.uri().path());
            if let Some(Fault::Reply(status)) = fault {
                return Response::builder()
                    .status(status)
                    .body(Body::empty())
                    .unwrap();
            }
            let headers = req.headers().clone();
            let body = axum::body::to_bytes(req.into_body(), usize::MAX)
                .await
                .unwrap();
            let mut out = http.request(method, format!("{target}{path}"));
            for (k, v) in headers.iter() {
                if k != axum::http::header::HOST {
                    out = out.header(k, v);
                }
            }
            let resp = out.body(body.to_vec()).send().await.unwrap();
            if let Some(Fault::LoseResponse(status)) = fault {
                return Response::builder()
                    .status(status)
                    .body(Body::empty())
                    .unwrap();
            }
            let mut b = Response::builder().status(resp.status().as_u16());
            for (k, v) in resp.headers().iter() {
                if !matches!(k.as_str(), "transfer-encoding" | "connection") {
                    b = b.header(k, v);
                }
            }
            b.body(Body::from(resp.bytes().await.unwrap())).unwrap()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// An invitation from `owner` to vault `vid` (or its note `object`) for
/// `who`, by their address, accepted by them: its id, and who they are as
/// the owner's app sees them (for tests that add them by hand).
pub async fn invited_and_accepted(
    owner: &Session,
    vid: &str,
    object: Option<&str>,
    who: &Session,
    role: notekpr_core::protocol::Role,
) -> (String, notekpr_core::protocol::PublicUser) {
    use notekpr_cli::invitations;
    use notekpr_core::protocol::{InvitationsResponse, SentInvitation};
    let address = format!(
        "{}@{}",
        who.name(),
        notekpr_core::protocol::DEV_EMAIL_DOMAIN
    );
    let req = notekpr_sync::members::invitation(
        owner.account(),
        &owner.user_id,
        vid,
        object,
        notekpr_core::invitation::Invitee::Email(&address),
        role,
        false,
        notekpr_core::now_ms(),
    )
    .unwrap();
    let sent: SentInvitation = owner
        .api
        .post(&format!("/vaults/{vid}/invitations"), &req)
        .await
        .unwrap();
    invitations::accept(who, &sent.id).await.unwrap();
    let list: InvitationsResponse = owner.api.get("/invitations").await.unwrap();
    let user = list
        .sent
        .into_iter()
        .find(|i| i.id == sent.id)
        .and_then(|i| i.accepted)
        .unwrap()
        .user;
    (sent.id, user)
}
