//! Live synchronisation over the WebSocket (`docs/protocol.md` §7).
//!
//! The loop subscribes to every open vault with `since = local seq`, applies
//! catch-up pages and live updates as they arrive (the server guarantees
//! per-vault order without gaps), reacts to vault and user events, and
//! reconnects with exponential backoff plus jitter — or, when the server
//! says it's restarting, about every second for a while. Local edits are uploaded
//! over REST by [`Hub::push`]; they come back over the socket like any other
//! update and are merged idempotently.

use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use futures::{SinkExt, StreamExt};
use notekpr_core::crypto::{self, CryptoError};
use notekpr_core::engine::EngineError;
use notekpr_core::keys;
use notekpr_core::protocol::*;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

use crate::hub::Hub;
use crate::session::{SharedVault, random_duration};
use crate::ui;

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
type Sink = futures::stream::SplitSink<Ws, Message>;

const PING_EVERY: Duration = Duration::from_secs(30);
const DEAD_AFTER: Duration = Duration::from_secs(95);
const SAVE_EVERY: Duration = Duration::from_secs(2);
const MAINTENANCE_EVERY: Duration = Duration::from_secs(60);

/// Why a connection ended.
enum Exit {
    Shutdown,
    Reconnect(String),
    /// The server is going away to be started again (an update).
    Restarting,
}

/// How the server closed the connection.
fn closed(frame: Option<CloseFrame>) -> Exit {
    match frame {
        Some(f) if u16::from(f.code) == CLOSE_RESTARTING => Exit::Restarting,
        _ => Exit::Reconnect("connection closed by the server".into()),
    }
}

/// After the server said it's restarting, retry this often (spread out, so
/// not everyone comes back in the same instant) for up to `RESTART_WINDOW`,
/// instead of backing off.
const RESTART_RETRY: Duration = Duration::from_millis(250);
const RESTART_SPREAD: Duration = Duration::from_millis(1000);
const RESTART_WINDOW: Duration = Duration::from_secs(60);

/// An error that reconnecting cannot fix.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Fatal(pub String);

struct Backoff {
    attempt: u32,
    /// Retrying quickly until then: the server is restarting.
    quick_until: Option<Instant>,
}

impl Backoff {
    fn next(&mut self) -> Duration {
        if self.quick_until.is_some_and(|t| Instant::now() < t) {
            return RESTART_RETRY + random_duration(RESTART_SPREAD);
        }
        let base = Duration::from_secs(1u64 << self.attempt.min(6)).min(Duration::from_secs(60));
        self.attempt += 1;
        // ±25% jitter so many clients do not reconnect in lockstep after an outage.
        base * 3 / 4 + random_duration(base / 2)
    }

    fn reset(&mut self) {
        self.attempt = 0;
        self.quick_until = None;
    }

    /// The server is restarting: retry about every second for a while.
    fn restarting(&mut self) {
        self.attempt = 0;
        self.quick_until = Some(Instant::now() + RESTART_WINDOW);
    }
}

/// A connection that lasted this long was healthy: the next retry starts
/// with the shortest delay again.
const HEALTHY_AFTER: Duration = Duration::from_secs(30);

/// What to do after a connection ended.
#[derive(Debug)]
enum Next {
    Stop,
    Fail(anyhow::Error),
    Retry(String),
    /// The server is restarting: come back soon.
    RetrySoon,
}

/// Only a [`Fatal`] error (session revoked or expired) ends live sync; any
/// other error — a socket write failing, a key fetch timing out — is
/// transient and means reconnecting (the server resends everything after
/// the local cursor), never leaving the daemon or gateway with stale data.
fn after_session(r: Result<Exit>) -> Next {
    match r {
        Ok(Exit::Shutdown) => Next::Stop,
        Ok(Exit::Reconnect(reason)) => Next::Retry(reason),
        Ok(Exit::Restarting) => Next::RetrySoon,
        Err(e) if e.is::<Fatal>() => Next::Fail(e),
        Err(e) => Next::Retry(format!("{e:#}")),
    }
}

/// Run until `shutdown` becomes true or a fatal error (session revoked).
pub async fn run(hub: Arc<Hub>, mut shutdown: watch::Receiver<bool>) -> Result<()> {
    let mut backoff = Backoff {
        attempt: 0,
        quick_until: None,
    };
    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        let next = match connect(&hub).await {
            Ok(ws) => {
                // Back: a restart is over.
                backoff.quick_until = None;
                let started = Instant::now();
                let r = session_loop(&hub, ws, &mut shutdown).await;
                // A connection that fails right away again and again keeps
                // backing off instead of hammering the server.
                if started.elapsed() >= HEALTHY_AFTER {
                    backoff.reset();
                }
                after_session(r)
            }
            Err(e) if e.is::<Fatal>() => Next::Fail(e),
            Err(e) => Next::Retry(format!("{e:#}")),
        };
        let reason = match next {
            Next::Stop => {
                hub.set_push_now(true);
                return Ok(());
            }
            Next::Fail(e) => {
                hub.set_online(false);
                hub.set_push_now(true);
                return Err(e);
            }
            Next::Retry(reason) => reason,
            Next::RetrySoon => {
                backoff.restarting();
                "the server is restarting".to_string()
            }
        };
        hub.set_online(false);
        hub.set_push_now(false);
        hub.save_all().await;
        let delay = backoff.next();
        ui::status(&format!(
            "offline ({reason}); retrying in {}s — local changes are kept",
            delay.as_secs().max(1)
        ));
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = shutdown.changed() => {}
        }
    }
}

fn tls_config() -> Result<Arc<rustls::ClientConfig>> {
    static CFG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    if let Some(c) = CFG.get() {
        return Ok(c.clone());
    }
    use rustls_platform_verifier::BuilderVerifierExt;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_platform_verifier()?
        .with_no_client_auth();
    Ok(CFG.get_or_init(|| Arc::new(cfg)).clone())
}

async fn connect(hub: &Hub) -> Result<Ws> {
    let api = &hub.session.api;
    let url = api.ws_url();
    let token = match api.token().await {
        Ok(t) => t,
        Err(e) if e.is_offline() => return Err(anyhow!(e)),
        Err(e) => return Err(Fatal(format!("{e}")).into()),
    };
    let connector = if url.starts_with("wss://") {
        Some(Connector::Rustls(tls_config()?))
    } else {
        None
    };
    let cfg = WebSocketConfig::default()
        .max_message_size(Some(256 << 20))
        .max_frame_size(Some(64 << 20));
    let (mut ws, _) = tokio::time::timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async_tls_with_config(url.as_str(), Some(cfg), true, connector),
    )
    .await
    .map_err(|_| anyhow!("connection timed out"))?
    .map_err(|e| anyhow!("websocket connection failed: {e}"))?;
    let auth = serde_json::to_string(&ClientMessage::Auth {
        token,
        app: Some(crate::api::app()),
    })?;
    ws.send(Message::Text(auth.into())).await?;
    let first = tokio::time::timeout(Duration::from_secs(10), ws.next())
        .await
        .map_err(|_| anyhow!("no answer to authentication"))?
        .ok_or_else(|| anyhow!("connection closed during authentication"))??;
    let msg: ServerMessage = match first {
        Message::Text(t) => serde_json::from_str(t.as_str())?,
        other => return Err(anyhow!("unexpected first frame: {other:?}")),
    };
    match msg {
        ServerMessage::Ready { .. } => Ok(ws),
        ServerMessage::Error { code, .. } if code == "unauthorized" => {
            if hub.session.kind == PrincipalKind::Agent {
                api.invalidate_token().await;
                Err(anyhow!("agent token rejected; requesting a new one"))
            } else {
                Err(Fatal(
                    "the session expired or was revoked; run `notekpr login` again (local data is kept)".into(),
                )
                .into())
            }
        }
        other => Err(anyhow!("unexpected answer to authentication: {other:?}")),
    }
}

async fn send(sink: &mut Sink, m: &ClientMessage) -> Result<()> {
    sink.send(Message::Text(serde_json::to_string(m)?.into()))
        .await?;
    Ok(())
}

/// Subscribe to a vault. Returns false (and sends nothing) for a vault that
/// is not open or read-only here: such a vault never reports `Synced`, so it
/// must not be waited for.
async fn subscribe(hub: &Hub, sink: &mut Sink, vault_id: &str) -> Result<bool> {
    let Some(v) = hub.get(vault_id) else {
        return Ok(false);
    };
    let since = {
        let g = v.lock().await;
        if g.read_only.is_some() {
            return Ok(false);
        }
        g.replica.seq
    };
    send(
        sink,
        &ClientMessage::Subscribe {
            vault_id: vault_id.to_string(),
            since: Some(since),
            object_id: None,
        },
    )
    .await?;
    Ok(true)
}

struct Conn {
    /// Vaults whose initial catch-up finished on this connection.
    synced: BTreeSet<String>,
    subscribed: BTreeSet<String>,
    announced: bool,
    nonce: u64,
}

async fn session_loop(
    hub: &Arc<Hub>,
    ws: Ws,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<Exit> {
    let (mut sink, mut stream) = ws.split();
    let mut conn = Conn {
        synced: BTreeSet::new(),
        subscribed: BTreeSet::new(),
        announced: false,
        nonce: 0,
    };
    hub.set_online(true);
    // Pick up vaults shared while we were away before subscribing.
    if let Ok(list) = hub.session.api.vaults().await
        && let Err(e) = hub.open_listed(&list).await
    {
        tracing::warn!("refreshing the vault list: {e:#}");
    }
    // Quick notes deleted while offline go for good now.
    if let Err(e) = crate::ops::purge_deleted_quick_notes(hub).await {
        tracing::warn!("deleting quick notes: {e:#}");
    }
    for id in hub.vault_ids() {
        // Only vaults actually subscribed are waited for: a read-only one
        // would otherwise keep live uploads off after every reconnect.
        if subscribe(hub, &mut sink, &id).await? {
            conn.subscribed.insert(id);
        }
    }
    if conn.subscribed.is_empty() {
        announce(hub, &mut conn).await;
    }
    let mut last_rx = Instant::now();
    let mut ping = tokio::time::interval(PING_EVERY);
    let mut save = tokio::time::interval(SAVE_EVERY);
    let mut maintenance = tokio::time::interval(MAINTENANCE_EVERY);
    ping.tick().await;
    save.tick().await;
    maintenance.tick().await;
    loop {
        tokio::select! {
            msg = stream.next() => {
                let msg = match msg {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => return Ok(Exit::Reconnect(format!("connection error: {e}"))),
                    None => return Ok(Exit::Reconnect("connection closed by the server".into())),
                };
                last_rx = Instant::now();
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Close(frame) => return Ok(closed(frame)),
                    _ => continue,
                };
                let parsed: ServerMessage = match serde_json::from_str(text.as_str()) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!("ignoring an unrecognised server message: {e}");
                        continue;
                    }
                };
                if let Some(exit) = handle(hub, parsed, &mut sink, &mut conn).await? {
                    return Ok(exit);
                }
            }
            _ = ping.tick() => {
                if last_rx.elapsed() > DEAD_AFTER {
                    return Ok(Exit::Reconnect("no traffic from the server".into()));
                }
                conn.nonce += 1;
                send(&mut sink, &ClientMessage::Ping { nonce: conn.nonce }).await?;
            }
            _ = save.tick() => hub.save_all().await,
            _ = maintenance.tick() => spawn_maintenance(hub.clone()),
            _ = shutdown.changed() => {
                let _ = sink.send(Message::Close(None)).await;
                hub.save_all().await;
                return Ok(Exit::Shutdown);
            }
        }
    }
}

/// A vault became read-only (deleted, access revoked): stop waiting for it.
async fn close(hub: &Hub, conn: &mut Conn, vault_id: &str, reason: &str) {
    hub.close_vault(vault_id, reason).await;
    conn.subscribed.remove(vault_id);
    if conn.subscribed.iter().all(|id| conn.synced.contains(id)) {
        announce(hub, conn).await;
    }
}

async fn announce(hub: &Hub, conn: &mut Conn) {
    if conn.announced {
        return;
    }
    conn.announced = true;
    hub.set_push_now(true);
    let n = hub.vault_ids().len();
    ui::status(&format!("connected: {n} vault(s) live"));
}

/// Revision after an editing session: a note idle this long with at least
/// this many updates since its last snapshot gets one.
pub const SESSION_IDLE: Duration = Duration::from_secs(120);
pub const SESSION_MIN_UPDATES: u32 = 5;

pub fn read_only_reason(why: ReadOnly) -> &'static str {
    match why {
        ReadOnly::Manual => "the server is read-only for now",
        ReadOnly::StorageFull => "the server's storage is full",
    }
}

fn spawn_maintenance(hub: Arc<Hub>) {
    if hub.server_read_only().is_some() {
        return;
    }
    tokio::spawn(async move {
        hub.snapshot_idle(SESSION_IDLE, SESSION_MIN_UPDATES).await;
        for v in hub.vaults() {
            if let Err(e) = hub.session.compact(&v, Some(Duration::from_secs(10))).await {
                tracing::warn!("compaction failed: {e:#}");
            }
        }
    });
}

/// Notes kept from unsent edits of notes deleted elsewhere (the vault's
/// rescue): shown (the mirror writes them) and uploaded like any local change.
async fn upload_rescued(hub: &Arc<Hub>, vault_id: &str, rescued: Vec<String>) {
    if !rescued.is_empty() {
        hub.local_changed(vault_id, rescued, true).await;
    }
}

fn spawn_push(hub: &Arc<Hub>, vault_id: &str) {
    let hub = hub.clone();
    let id = vault_id.to_string();
    tokio::spawn(async move {
        match hub.push(&id).await {
            Ok(o) if o.pushed > 0 || o.blobs > 0 => {
                tracing::debug!(vault = %id, pushed = o.pushed, blobs = o.blobs, "uploaded local changes")
            }
            Ok(_) => {}
            Err(e) => ui::warn(&format!("upload failed (will retry): {e:#}")),
        }
    });
}

/// Whether a ciphertext uses a key we do not hold yet: a newer vault-key
/// epoch (rotated), or a document-key epoch (top bit set, an individually
/// shared note) missing from that note's keyring. Document epochs are always
/// "greater" than vault epochs, so comparing them with the vault's current
/// epoch made every page with a shared note refetch all keys.
/// Content by someone our copy of the membership log doesn't know waits for
/// a fresh copy: reload it in the background and check that content again.
fn reload_log_if_needed(hub: &Arc<Hub>, v: &SharedVault) {
    let (h, v) = (hub.clone(), v.clone());
    tokio::spawn(async move {
        if v.lock().await.replica.needs_log() {
            match h.session.reload_member_log(&v).await {
                Ok(a) => {
                    let id = v.lock().await.id().to_string();
                    h.emit_applied(&id, &a, false);
                }
                Err(e) => tracing::warn!("reloading the membership log: {e:#}"),
            }
            // What passed may still need a key we don't have yet.
            if v.lock().await.replica.needs_keys()
                && let Err(e) = h.session.refresh_keys(&v).await
            {
                tracing::warn!("refreshing vault keys: {e:#}");
            }
        }
    });
}

pub(crate) fn missing_epoch<'a>(
    v: &crate::vault::Vault,
    mut items: impl Iterator<Item = (&'a str, &'a [u8])>,
) -> bool {
    let current = v.replica.keyring.current_epoch().unwrap_or(0);
    items.any(|(object_id, payload)| match crypto::sym_epoch(payload) {
        Ok(e) if keys::is_doc_epoch(e) => v
            .replica
            .doc_keys
            .get(object_id)
            .is_none_or(|ring| ring.get(e).is_none()),
        Ok(e) => e > current,
        Err(_) => false,
    })
}

fn is_missing_epoch(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<EngineError>(),
        Some(EngineError::Crypto(CryptoError::MissingEpoch(_)))
    )
}

/// Fetch the notes whose deletion for good was undone (in the background:
/// the cursor waits for them, so a failure is retried with the next
/// catch-up).
/// Owner: renew keys someone who no longer has access still knows.
async fn renew_pending_keys(hub: &Hub, v: &SharedVault) {
    if let Err(e) = hub.session.renew_pending_keys(v).await {
        ui::warn(&format!(
            "could not renew a key someone who left still knows: {e:#}"
        ));
    }
}

fn spawn_fetch_restored(hub: &Arc<Hub>, vault_id: &str, a: &notekpr_core::engine::Applied) {
    if a.restored.is_empty() {
        return;
    }
    let (h, vault_id, ids) = (hub.clone(), vault_id.to_string(), a.restored.clone());
    tokio::spawn(async move {
        let Some(v) = h.get(&vault_id) else {
            return;
        };
        for o in ids {
            match h.session.fetch_restored(&v, &o).await {
                Ok(a) => h.emit_applied(&vault_id, &a, false),
                Err(e) => tracing::warn!("fetching a restored note: {e:#}"),
            }
        }
    });
}

async fn handle(
    hub: &Arc<Hub>,
    msg: ServerMessage,
    sink: &mut Sink,
    conn: &mut Conn,
) -> Result<Option<Exit>> {
    let session = &hub.session;
    match msg {
        ServerMessage::Changes { vault_id, changes } => {
            let Some(v) = hub.get(&vault_id) else {
                return Ok(None);
            };
            let needs_keys = {
                let g = v.lock().await;
                let it = changes
                    .updates
                    .iter()
                    .map(|u| (u.object_id.as_str(), u.payload.as_slice()))
                    .chain(
                        changes
                            .snapshots
                            .iter()
                            .map(|s| (s.object_id.as_str(), s.payload.as_slice())),
                    );
                missing_epoch(&g, it)
            };
            if needs_keys && let Err(e) = session.refresh_keys(&v).await {
                tracing::warn!("refreshing vault keys: {e:#}");
            }
            let mut g = v.lock().await;
            g.check_rollback(changes.head_seq);
            let a = g.apply_changes(&changes);
            let rescued = g.take_rescued();
            drop(g);
            hub.emit_applied(&vault_id, &a, false);
            upload_rescued(hub, &vault_id, rescued).await;
            reload_log_if_needed(hub, &v);
            spawn_fetch_restored(hub, &vault_id, &a);
        }
        ServerMessage::Restored {
            vault_id,
            object_id,
            seq,
        } => {
            if let Some(v) = hub.get(&vault_id) {
                let a = v.lock().await.apply_restore(&object_id, seq);
                hub.emit_applied(&vault_id, &a, false);
                spawn_fetch_restored(hub, &vault_id, &a);
            }
        }
        ServerMessage::Synced {
            vault_id, head_seq, ..
        } => {
            let Some(v) = hub.get(&vault_id) else {
                return Ok(None);
            };
            let needs_keys = {
                let mut g = v.lock().await;
                g.check_rollback(head_seq);
                g.synced(head_seq);
                g.last_sync = Some(notekpr_core::now_ms());
                if let Err(e) = g.save() {
                    ui::warn(&format!("saving the local copy: {e:#}"));
                }
                g.replica.needs_keys()
            };
            conn.synced.insert(vault_id.clone());
            spawn_push(hub, &vault_id);
            let h = hub.clone();
            tokio::spawn(async move {
                // Content kept for a key that could not be fetched before.
                if needs_keys && let Err(e) = h.session.refresh_keys(&v).await {
                    tracing::warn!("refreshing keys: {e:#}");
                }
                if let Err(e) = h.session.grant_missing_keys(&v).await {
                    tracing::warn!("granting keys: {e:#}");
                }
                renew_pending_keys(&h, &v).await;
                // Entries deleted for good that the server no longer keeps.
                if let Ok(days) = h.session.server_info().await.map(|i| i.purge_undo_days) {
                    let id = v.lock().await.id().to_string();
                    if let Err(e) = crate::ops::forget_erased(&h, &id, days).await {
                        tracing::warn!("tidying what was deleted for good: {e:#}");
                    }
                }
                // Caught up: files of the quick notes no note links to go.
                if v.lock().await.is_quick_notes()
                    && let Err(e) =
                        crate::ops::drop_unlinked_quick_files(&h, notekpr_core::now_ms()).await
                {
                    tracing::warn!("tidying the quick notes: {e:#}");
                }
            });
            if conn.subscribed.iter().all(|id| conn.synced.contains(id)) {
                announce(hub, conn).await;
            }
        }
        ServerMessage::Update { vault_id, update } => {
            let Some(v) = hub.get(&vault_id) else {
                return Ok(None);
            };
            let mut res = v.lock().await.apply_update(&update);
            if let Err(e) = &res
                && is_missing_epoch(e)
            {
                // The update is kept (deferred) and replayed once the keys
                // could be fetched; a failed fetch does not end live sync.
                match session.refresh_keys(&v).await {
                    Ok(()) => res = v.lock().await.apply_update(&update),
                    Err(e) => ui::warn(&format!(
                        "could not fetch keys (will retry; content is kept): {e:#}"
                    )),
                }
            }
            match res {
                Ok(a) => hub.emit_applied(&vault_id, &a, false),
                Err(e) => {
                    tracing::warn!(vault = %vault_id, "skipped an update that could not be applied: {e:#}")
                }
            }
            reload_log_if_needed(hub, &v);
        }
        ServerMessage::Snapshot { vault_id, snapshot } => {
            let Some(v) = hub.get(&vault_id) else {
                return Ok(None);
            };
            let mut res = v.lock().await.apply_snapshot(&snapshot);
            if let Err(e) = &res
                && is_missing_epoch(e)
            {
                match session.refresh_keys(&v).await {
                    Ok(()) => res = v.lock().await.apply_snapshot(&snapshot),
                    Err(e) => ui::warn(&format!(
                        "could not fetch keys (will retry; content is kept): {e:#}"
                    )),
                }
            }
            match res {
                Ok(a) => hub.emit_applied(&vault_id, &a, false),
                Err(e) => {
                    tracing::warn!(vault = %vault_id, "skipped a snapshot that could not be applied: {e:#}")
                }
            }
        }
        ServerMessage::SnapshotCreated {
            vault_id,
            object_id,
            ..
        } => {
            if let Some(v) = hub.get(&vault_id) {
                let mut g = v.lock().await;
                g.replica.snapshot_created(&object_id);
                g.mark_state_dirty();
            }
        }
        ServerMessage::Purged {
            vault_id,
            object_id,
            seq,
        } => {
            if let Some(v) = hub.get(&vault_id) {
                let (a, rescued) = {
                    let mut g = v.lock().await;
                    (g.apply_purge(&object_id, seq), g.take_rescued())
                };
                hub.emit_applied(&vault_id, &a, false);
                upload_rescued(hub, &vault_id, rescued).await;
            }
        }
        ServerMessage::Ack {
            vault_id,
            results,
            rejected,
        } => {
            if let Some(v) = hub.get(&vault_id) {
                let (reaction, rescued) = {
                    let mut g = v.lock().await;
                    (g.ack(&results, &rejected, true), g.take_rescued())
                };
                upload_rescued(hub, &vault_id, rescued).await;
                if reaction.reload_keys
                    && let Err(e) = session.refresh_keys(&v).await
                {
                    tracing::warn!("refreshing vault keys: {e:#}");
                }
                if reaction.resend || reaction.reload_keys {
                    spawn_push(hub, &vault_id);
                }
            }
        }
        ServerMessage::VaultEvent { vault_id, event } => {
            let Some(v) = hub.get(&vault_id) else {
                return Ok(None);
            };
            match event {
                VaultEventKind::KeyRotated => {
                    if let Err(e) = session.refresh_keys(&v).await {
                        ui::warn(&format!("could not fetch the rotated vault key: {e:#}"));
                    }
                }
                VaultEventKind::MetaChanged | VaultEventKind::RoleChanged => {
                    let h = hub.clone();
                    tokio::spawn(async move {
                        if let Err(e) = h.session.refresh_vault(&v).await {
                            tracing::warn!("refreshing vault: {e:#}");
                        }
                    });
                }
                VaultEventKind::MembersChanged => {
                    let h = hub.clone();
                    tokio::spawn(async move {
                        match h.session.grant_missing_keys(&v).await {
                            Ok(names) if !names.is_empty() => ui::status(&format!(
                                "shared the vault key with {}",
                                names.join(", ")
                            )),
                            Ok(_) => {}
                            Err(e) => tracing::warn!("granting keys: {e:#}"),
                        }
                        renew_pending_keys(&h, &v).await;
                    });
                }
                VaultEventKind::Deleted => {
                    close(hub, conn, &vault_id, "the vault was deleted by its owner").await
                }
                VaultEventKind::AccessRevoked => {
                    close(
                        hub,
                        conn,
                        &vault_id,
                        "your access to this vault was revoked",
                    )
                    .await
                }
            }
        }
        ServerMessage::ObjectEvent {
            vault_id,
            object_id,
            event,
        } => {
            // A note of a vault we sync got (or rotated) its own key: reload
            // that note's keys only.
            if matches!(event, ObjectEventKind::Shared | ObjectEventKind::KeyRotated)
                && let Some(v) = hub.get(&vault_id)
                && let Err(e) = session.refresh_note_keys(&v, &object_id).await
            {
                ui::warn(&format!("could not fetch the note's key: {e:#}"));
            }
            // Someone it was shared with may have gone: the owner renews its key.
            if event == ObjectEventKind::SharesChanged
                && let Some(v) = hub.get(&vault_id)
            {
                let h = hub.clone();
                tokio::spawn(async move { renew_pending_keys(&h, &v).await });
            }
        }
        ServerMessage::UserEvent { event } => match event {
            UserEventKind::VaultsChanged => {
                let opened = match session.api.vaults().await {
                    Ok(list) => hub.open_listed(&list).await,
                    Err(e) => Err(anyhow!(e)),
                };
                match opened {
                    Ok(ids) => {
                        for id in ids {
                            let name = match hub.get(&id) {
                                Some(v) => v.lock().await.name().to_string(),
                                None => id.clone(),
                            };
                            ui::status(&format!("vault '{name}' is now available"));
                            if subscribe(hub, sink, &id).await? {
                                conn.subscribed.insert(id);
                            }
                        }
                    }
                    Err(e) => tracing::warn!("refreshing the vault list: {e:#}"),
                }
            }
            UserEventKind::SessionRevoked => {
                if session.kind == PrincipalKind::Agent {
                    session.api.invalidate_token().await;
                    return Ok(Some(Exit::Reconnect("agent token expired".into())));
                }
                return Err(Fatal(
                    "this session was revoked or expired; run `notekpr login` again (local data is kept)".into(),
                )
                .into());
            }
            UserEventKind::DevicePending => {
                ui::status("a new device is waiting for approval: run `notekpr devices list`")
            }
            UserEventKind::DevicesChanged => {}
            UserEventKind::SharesChanged => {
                ui::status("a note was shared with you (or unshared): `notekpr shared list`")
            }
        },
        ServerMessage::Error {
            code,
            message,
            vault_id,
        } => {
            if code == "unauthorized" {
                if session.kind == PrincipalKind::Agent {
                    session.api.invalidate_token().await;
                    return Ok(Some(Exit::Reconnect("agent token expired".into())));
                }
                return Err(Fatal(format!("{message}; run `notekpr login` again")).into());
            }
            match vault_id {
                Some(v) if code == "stale_key" => {
                    if let Some(sv) = hub.get(&v) {
                        let _ = session.refresh_keys(&sv).await;
                    }
                }
                Some(v) if code == "not_found" => {
                    close(hub, conn, &v, "the vault is no longer accessible").await
                }
                Some(v) => tracing::warn!(vault = %v, code, "server error: {message}"),
                None => tracing::warn!(code, "server error: {message}"),
            }
        }
        ServerMessage::Mode { read_only } => {
            let was = hub.server_read_only();
            if hub.set_server_read_only(read_only) {
                match read_only {
                    Some(why) => ui::status(&format!(
                        "{}: local changes are kept and uploaded when it accepts changes again",
                        read_only_reason(why)
                    )),
                    None if was.is_some() => {
                        ui::status("the server accepts changes again: uploading local changes");
                        // Vaults still catching up push when they're synced.
                        for id in &conn.synced {
                            spawn_push(hub, id);
                        }
                    }
                    None => {}
                }
            }
        }
        ServerMessage::Ready { .. }
        | ServerMessage::Pong { .. }
        | ServerMessage::Awareness { .. } => {}
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notekpr_core::crypto::Purpose;
    use notekpr_core::engine::{VaultMeta, VaultReplica};
    use notekpr_core::keys::{AccountSecret, FIRST_DOC_EPOCH, VaultKey, VaultKeyring};

    #[test]
    fn only_a_revoked_session_ends_live_sync() {
        // Transient failures (a socket write, a key fetch) reconnect.
        assert!(matches!(
            after_session(Err(anyhow!("socket write failed"))),
            Next::Retry(_)
        ));
        let keys = anyhow!(crate::api::ApiError::Network("timed out".into()))
            .context("fetching vault keys");
        assert!(matches!(after_session(Err(keys)), Next::Retry(_)));
        assert!(matches!(
            after_session(Ok(Exit::Reconnect("closed".into()))),
            Next::Retry(_)
        ));
        assert!(matches!(after_session(Ok(Exit::Shutdown)), Next::Stop));
        assert!(matches!(
            after_session(Ok(Exit::Restarting)),
            Next::RetrySoon
        ));
        assert!(matches!(
            after_session(Err(Fatal("session revoked".into()).into())),
            Next::Fail(_)
        ));
    }

    #[test]
    fn a_restarting_server_is_retried_about_every_second() {
        use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
        let frame = |code: u16| {
            Some(CloseFrame {
                code: CloseCode::from(code),
                reason: "".into(),
            })
        };
        assert!(matches!(closed(frame(CLOSE_RESTARTING)), Exit::Restarting));
        assert!(matches!(closed(frame(1000)), Exit::Reconnect(_)));
        assert!(matches!(closed(None), Exit::Reconnect(_)));

        let mut b = Backoff {
            attempt: 0,
            quick_until: None,
        };
        // A failure backs off further each time…
        let failures: Vec<Duration> = (0..4).map(|_| b.next()).collect();
        assert!(failures[3] >= Duration::from_secs(6), "{failures:?}");
        // …a restart is retried quickly, however often it takes.
        b.restarting();
        for _ in 0..10 {
            let d = b.next();
            assert!(
                (RESTART_RETRY..RESTART_RETRY + RESTART_SPREAD).contains(&d),
                "{d:?}"
            );
        }
        b.reset();
        assert!(b.next() >= Duration::from_millis(750));
    }

    #[test]
    fn document_key_epochs_are_not_mistaken_for_rotations() {
        let d = tempfile::tempdir().unwrap();
        let account = AccountSecret::generate();
        let meta = VaultMeta::named("T");
        let (req, ring) = notekpr_core::engine::new_vault(&account, "u1", &meta, 1).unwrap();
        let key = Arc::new(crate::secrets::LocalKey::load_or_create(d.path()).unwrap());
        let store = crate::store::VaultStore::new(d.path().join("vaults"), key, &req.id).unwrap();
        let mut replica = VaultReplica::new(&req.id, Role::Owner, ring, meta);
        let mut doc = VaultKeyring::new(req.id.clone());
        doc.insert(FIRST_DOC_EPOCH, VaultKey(crypto::random_bytes::<32>()));
        replica.doc_keys.insert("shared".into(), doc);
        let v = crate::vault::Vault::new(store, replica, "u1", false);
        let shared = v
            .replica
            .encrypt_object(Purpose::Update, "shared", b"x")
            .unwrap();
        let plain = v
            .replica
            .encrypt_object(Purpose::Update, "plain", b"x")
            .unwrap();
        assert!(keys::is_doc_epoch(crypto::sym_epoch(&shared).unwrap()));
        // Keys we hold: nothing to fetch (this used to refetch every page).
        assert!(!missing_epoch(
            &v,
            [("shared", shared.as_slice()), ("plain", plain.as_slice())].into_iter()
        ));
        // A document key we lack, or a newer vault epoch: fetch.
        assert!(missing_epoch(
            &v,
            [("other", shared.as_slice())].into_iter()
        ));
        let mut rotated = plain.clone();
        rotated[1..5].copy_from_slice(&2u32.to_be_bytes());
        assert!(missing_epoch(
            &v,
            [("plain", rotated.as_slice())].into_iter()
        ));
    }
}
