//! Live synchronisation over the WebSocket (`docs/protocol.md` §7).
//!
//! The loop subscribes to every open vault with `since = local seq`, applies
//! catch-up pages and live updates as they arrive (the server guarantees
//! per-vault order without gaps), reacts to vault and user events, and
//! reconnects with exponential backoff plus jitter. Local edits are uploaded
//! over REST by [`Hub::push`]; they come back over the socket like any other
//! update and are merged idempotently.

use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use coolpsonotes_core::crypto::{self, CryptoError};
use coolpsonotes_core::engine::EngineError;
use coolpsonotes_core::keys;
use coolpsonotes_core::protocol::*;
use futures::{SinkExt, StreamExt};
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

use crate::hub::Hub;
use crate::session::random_duration;
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
}

/// An error that reconnecting cannot fix.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Fatal(pub String);

struct Backoff {
    attempt: u32,
}

impl Backoff {
    fn next(&mut self) -> Duration {
        let base = Duration::from_secs(1u64 << self.attempt.min(6)).min(Duration::from_secs(60));
        self.attempt += 1;
        // ±25% jitter so many clients do not reconnect in lockstep after an outage.
        base * 3 / 4 + random_duration(base / 2)
    }

    fn reset(&mut self) {
        self.attempt = 0;
    }
}

/// Run until `shutdown` becomes true or a fatal error (session revoked).
pub async fn run(hub: Arc<Hub>, mut shutdown: watch::Receiver<bool>) -> Result<()> {
    let mut backoff = Backoff { attempt: 0 };
    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        let reason = match connect(&hub).await {
            Ok(ws) => {
                backoff.reset();
                match session_loop(&hub, ws, &mut shutdown).await {
                    Ok(Exit::Shutdown) => {
                        hub.set_push_now(true);
                        return Ok(());
                    }
                    Ok(Exit::Reconnect(r)) => r,
                    Err(e) => {
                        hub.set_online(false);
                        hub.set_push_now(true);
                        return Err(e);
                    }
                }
            }
            Err(e) if e.is::<Fatal>() => {
                hub.set_online(false);
                hub.set_push_now(true);
                return Err(e);
            }
            Err(e) => format!("{e:#}"),
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
        Err(e) if e.is_network() => return Err(anyhow!(e)),
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
    let auth = serde_json::to_string(&ClientMessage::Auth { token })?;
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
                    "the session expired or was revoked; run `coolpsonotes login` again (local data is kept)".into(),
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

async fn subscribe(hub: &Hub, sink: &mut Sink, vault_id: &str) -> Result<()> {
    let Some(v) = hub.get(vault_id) else {
        return Ok(());
    };
    let since = {
        let g = v.lock().await;
        if g.read_only.is_some() {
            return Ok(());
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
    .await
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
    for id in hub.vault_ids() {
        subscribe(hub, &mut sink, &id).await?;
        conn.subscribed.insert(id);
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
                    Message::Close(_) => return Ok(Exit::Reconnect("connection closed by the server".into())),
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

fn spawn_maintenance(hub: Arc<Hub>) {
    tokio::spawn(async move {
        hub.snapshot_idle(SESSION_IDLE, SESSION_MIN_UPDATES).await;
        for v in hub.vaults() {
            if let Err(e) = hub.session.compact(&v, Some(Duration::from_secs(10))).await {
                tracing::warn!("compaction failed: {e:#}");
            }
        }
    });
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
            drop(g);
            hub.emit_applied(&vault_id, &a, false);
        }
        ServerMessage::Synced {
            vault_id, head_seq, ..
        } => {
            let Some(v) = hub.get(&vault_id) else {
                return Ok(None);
            };
            {
                let mut g = v.lock().await;
                g.check_rollback(head_seq);
                g.synced(head_seq);
                g.last_sync = Some(coolpsonotes_core::now_ms());
                if let Err(e) = g.save() {
                    ui::warn(&format!("saving the local copy: {e:#}"));
                }
            }
            conn.synced.insert(vault_id.clone());
            spawn_push(hub, &vault_id);
            let h = hub.clone();
            tokio::spawn(async move {
                if let Err(e) = h.session.grant_missing_keys(&v).await {
                    tracing::warn!("granting keys: {e:#}");
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
                session.refresh_keys(&v).await?;
                res = v.lock().await.apply_update(&update);
            }
            match res {
                Ok(a) => hub.emit_applied(&vault_id, &a, false),
                Err(e) => {
                    tracing::warn!(vault = %vault_id, "skipped an update that could not be applied: {e:#}")
                }
            }
        }
        ServerMessage::Snapshot { vault_id, snapshot } => {
            let Some(v) = hub.get(&vault_id) else {
                return Ok(None);
            };
            let mut res = v.lock().await.apply_snapshot(&snapshot);
            if let Err(e) = &res
                && is_missing_epoch(e)
            {
                session.refresh_keys(&v).await?;
                res = v.lock().await.apply_snapshot(&snapshot);
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
                let a = v.lock().await.apply_purge(&object_id, seq);
                hub.emit_applied(&vault_id, &a, false);
            }
        }
        ServerMessage::Ack { vault_id, results } => {
            if let Some(v) = hub.get(&vault_id) {
                v.lock().await.ack(&results, true);
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
                    });
                }
                VaultEventKind::Deleted => {
                    hub.close_vault(&vault_id, "the vault was deleted by its owner")
                        .await
                }
                VaultEventKind::AccessRevoked => {
                    hub.close_vault(&vault_id, "your access to this vault was revoked")
                        .await
                }
            }
        }
        ServerMessage::ObjectEvent {
            vault_id, event, ..
        } => {
            // A note of a vault we sync got (or rotated) its own key.
            if matches!(event, ObjectEventKind::Shared | ObjectEventKind::KeyRotated)
                && let Some(v) = hub.get(&vault_id)
                && let Err(e) = session.refresh_keys(&v).await
            {
                ui::warn(&format!("could not fetch the shared note's key: {e:#}"));
            }
        }
        ServerMessage::UserEvent { event } => match event {
            UserEventKind::VaultsChanged => match session.api.vaults().await {
                Ok(list) => {
                    for id in hub.open_listed(&list).await? {
                        let name = match hub.get(&id) {
                            Some(v) => v.lock().await.name().to_string(),
                            None => id.clone(),
                        };
                        ui::status(&format!("vault '{name}' is now available"));
                        subscribe(hub, sink, &id).await?;
                        conn.subscribed.insert(id);
                    }
                }
                Err(e) => tracing::warn!("refreshing the vault list: {e}"),
            },
            UserEventKind::SessionRevoked => {
                if session.kind == PrincipalKind::Agent {
                    session.api.invalidate_token().await;
                    return Ok(Some(Exit::Reconnect("agent token expired".into())));
                }
                return Err(Fatal(
                    "this session was revoked or expired; run `coolpsonotes login` again (local data is kept)".into(),
                )
                .into());
            }
            UserEventKind::DevicePending => {
                ui::status("a new device is waiting for approval: run `coolpsonotes devices list`")
            }
            UserEventKind::DevicesChanged => {}
            UserEventKind::SharesChanged => {
                ui::status("a note was shared with you (or unshared): `coolpsonotes shared list`")
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
                return Err(Fatal(format!("{message}; run `coolpsonotes login` again")).into());
            }
            match vault_id {
                Some(v) if code == "stale_key" => {
                    if let Some(sv) = hub.get(&v) {
                        let _ = session.refresh_keys(&sv).await;
                    }
                }
                Some(v) if code == "not_found" => {
                    hub.close_vault(&v, "the vault is no longer accessible")
                        .await
                }
                Some(v) => tracing::warn!(vault = %v, code, "server error: {message}"),
                None => tracing::warn!(code, "server error: {message}"),
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
    use coolpsonotes_core::crypto::Purpose;
    use coolpsonotes_core::engine::{VaultMeta, VaultReplica};
    use coolpsonotes_core::keys::{AccountSecret, FIRST_DOC_EPOCH, VaultKey, VaultKeyring};

    #[test]
    fn document_key_epochs_are_not_mistaken_for_rotations() {
        let d = tempfile::tempdir().unwrap();
        let account = AccountSecret::generate();
        let meta = VaultMeta::named("T");
        let (req, ring) = coolpsonotes_core::engine::new_vault(&account, "u1", &meta).unwrap();
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
