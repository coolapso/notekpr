//! Live sync over WebSocket.
//!
//! Protocol (JSON text frames, see `coolpsonotes_core::protocol`):
//! 1. client sends `auth` (browsers cannot set headers on WebSockets);
//! 2. client `subscribe`s to vaults, optionally with `since` to catch up;
//! 3. server streams `changes` pages, then `synced`, then live events in
//!    sequence order; the client's own pushes come back as `ack` in that order.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use coolpsonotes_core::protocol::{
    ClientMessage, ObjectEventKind, PushResult, ServerMessage, UserEventKind, VaultEventKind,
};
use futures::{SinkExt, StreamExt};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::access;
use crate::auth::{self, Principal};
use crate::error::AppError;
use crate::hub::{Control, VaultEvent};
use crate::state::AppState;
use crate::sync;

const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_SUBSCRIPTIONS: usize = 200;
/// Sustained pushes + subscriptions per connection and second, and burst
/// (a reconnect subscribes to every vault at once).
const REQUESTS_PER_SEC: f64 = 20.0;
const REQUEST_BURST: f64 = 400.0;
const MAX_AWARENESS_BYTES: usize = 16 * 1024;
const OUTBOUND_QUEUE: usize = 1024;

enum Out {
    Msg(ServerMessage),
    Ping,
    Close,
}

pub async fn handler(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    let max = (state.config.max_update_bytes() as usize) * 2 + 1024 * 1024;
    ws.max_message_size(max)
        .max_frame_size(max)
        .on_upgrade(move |socket| connection(socket, state))
}

fn error_msg(e: &AppError, vault_id: Option<&str>) -> ServerMessage {
    let body = e.body();
    ServerMessage::Error {
        code: body.error,
        message: body.message,
        vault_id: vault_id.map(str::to_string),
    }
}

async fn connection(socket: WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();

    // Authenticate with the first message.
    let token = match tokio::time::timeout(AUTH_TIMEOUT, stream.next()).await {
        Ok(Some(Ok(Message::Text(t)))) => match serde_json::from_str::<ClientMessage>(&t) {
            Ok(ClientMessage::Auth { token }) => token,
            _ => return,
        },
        _ => return,
    };
    let who = match auth::authenticate(&state, &token).await {
        Ok(p) => p,
        Err(e) => {
            let m = serde_json::to_string(&error_msg(&e, None)).unwrap();
            let _ = sink.send(Message::Text(m.into())).await;
            let _ = sink.close().await;
            return;
        }
    };

    let conn_id = state.hub.next_conn_id();
    let mut control = state.hub.register_user(&who.user_id, conn_id);
    let (out_tx, mut out_rx) = mpsc::channel::<Out>(OUTBOUND_QUEUE);
    metrics::gauge!("coolpsonotes_ws_connections").increment(1.0);

    let writer = tokio::spawn(async move {
        while let Some(o) = out_rx.recv().await {
            let r = match o {
                Out::Msg(m) => match serde_json::to_string(&m) {
                    Ok(s) => sink.send(Message::Text(s.into())).await,
                    Err(_) => continue,
                },
                Out::Ping => sink.send(Message::Ping(Vec::new().into())).await,
                Out::Close => {
                    let _ = sink.close().await;
                    break;
                }
            };
            if r.is_err() {
                break;
            }
        }
    });

    let _ = out_tx
        .send(Out::Msg(ServerMessage::Ready {
            user_id: who.user_id.clone(),
            session_id: who.session_id.clone(),
            conn_id,
        }))
        .await;

    let mut conn = Conn {
        state: state.clone(),
        who,
        token,
        conn_id,
        out: out_tx.clone(),
        subs: HashMap::new(),
        via_membership: Default::default(),
        announced: Default::default(),
        awareness: Bucket::new(30.0, 60.0),
        requests: Bucket::new(REQUESTS_PER_SEC, REQUEST_BURST),
    };
    let mut keepalive = tokio::time::interval(Duration::from_secs(30));
    let mut revalidate = tokio::time::interval(Duration::from_secs(60));
    keepalive.tick().await;
    revalidate.tick().await;

    loop {
        tokio::select! {
            msg = stream.next() => match msg {
                Some(Ok(Message::Text(t))) => {
                    if !conn.handle_text(&t).await {
                        break;
                    }
                }
                Some(Ok(Message::Binary(_))) => {
                    conn.send(error_msg(&AppError::BadRequest("binary frames are not supported".into()), None)).await;
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            Some(c) = control.recv() => match c {
                Control::RevokeVault(v) => {
                    // Per-note subscriptions opened as a member end too;
                    // those of notes shared individually stay.
                    let member_notes: Vec<String> = conn.via_membership.iter().filter(|(x, _)| *x == v).map(|(_, o)| o.clone()).collect();
                    for o in member_notes {
                        conn.via_membership.remove(&(v.clone(), o.clone()));
                        if let Some(h) = conn.subs.remove(&(v.clone(), Some(o.clone()))) {
                            h.abort();
                            conn.send(ServerMessage::ObjectEvent { vault_id: v.clone(), object_id: o, event: ObjectEventKind::AccessRevoked }).await;
                        }
                    }
                    if let Some(h) = conn.subs.remove(&(v.clone(), None)) {
                        h.abort();
                        conn.send(ServerMessage::VaultEvent { vault_id: v, event: VaultEventKind::AccessRevoked }).await;
                    }
                }
                Control::RevokeObject(v, o) => {
                    if let Some(h) = conn.subs.remove(&(v.clone(), Some(o.clone()))) {
                        h.abort();
                    }
                    conn.send(ServerMessage::ObjectEvent { vault_id: v, object_id: o, event: ObjectEventKind::AccessRevoked }).await;
                }
                Control::SessionRevoked(s) if s == conn.who.session_id => {
                    conn.send(ServerMessage::UserEvent { event: UserEventKind::SessionRevoked }).await;
                    break;
                }
                Control::SessionRevoked(_) => {}
                Control::User(event) => conn.send(ServerMessage::UserEvent { event }).await,
            },
            _ = keepalive.tick() => {
                if out_tx.try_send(Out::Ping).is_err() {
                    break;
                }
            }
            _ = revalidate.tick() => {
                if auth::authenticate(&state, &conn.token).await.is_err() {
                    conn.send(ServerMessage::UserEvent { event: UserEventKind::SessionRevoked }).await;
                    break;
                }
            }
        }
    }

    // Tell collaborators this connection's presence is gone.
    let mut vaults = std::collections::BTreeSet::new();
    for ((vault_id, _), h) in conn.subs.drain() {
        h.abort();
        vaults.insert(vault_id);
    }
    let notes = std::mem::take(&mut conn.announced);
    for (vault_id, object_id) in vaults.into_iter().map(|v| (v, String::new())).chain(notes) {
        state.hub.publish(
            &vault_id,
            VaultEvent::Awareness {
                object_id,
                from_user: conn.who.user_id.clone(),
                from_conn: conn_id,
                payload: vec![],
            },
        );
    }
    state.hub.unregister_user(&conn.who.user_id, conn_id);
    metrics::gauge!("coolpsonotes_ws_connections").decrement(1.0);
    let _ = out_tx.send(Out::Close).await;
    drop(out_tx);
    let _ = writer.await;
}

struct Conn {
    state: AppState,
    who: Principal,
    token: String,
    conn_id: u64,
    out: mpsc::Sender<Out>,
    /// Subscriptions keyed by (vault, note): `None` = the whole vault.
    subs: HashMap<(String, Option<String>), JoinHandle<()>>,
    /// Per-note subscriptions granted through vault membership (not an
    /// individual share); they end when the membership does.
    via_membership: std::collections::HashSet<(String, String)>,
    /// Notes this connection announced presence on (to say goodbye there too).
    announced: std::collections::HashSet<(String, String)>,
    awareness: Bucket,
    /// Pushes and (un)subscriptions: generous for real clients (which
    /// batch edits and subscribe once per vault), but bounded.
    requests: Bucket,
}

impl Conn {
    async fn send(&self, m: ServerMessage) {
        let _ = self.out.send(Out::Msg(m)).await;
    }

    /// Returns false if the connection should close.
    async fn handle_text(&mut self, text: &str) -> bool {
        let msg: ClientMessage = match serde_json::from_str(text) {
            Ok(m) => m,
            Err(e) => {
                self.send(error_msg(
                    &AppError::BadRequest(format!("invalid message: {e}")),
                    None,
                ))
                .await;
                return true;
            }
        };
        if matches!(
            msg,
            ClientMessage::Push { .. }
                | ClientMessage::Subscribe { .. }
                | ClientMessage::Unsubscribe { .. }
        ) && !self.requests.take()
        {
            metrics::counter!("coolpsonotes_rate_limited_total", "scope" => "ws").increment(1);
            self.send(error_msg(&AppError::RateLimited, None)).await;
            return true;
        }
        match msg {
            ClientMessage::Auth { .. } => {}
            ClientMessage::Ping { nonce } => self.send(ServerMessage::Pong { nonce }).await,
            ClientMessage::Subscribe {
                vault_id,
                since,
                object_id,
            } => {
                // Returns whether the caller is a vault member (not only a note recipient).
                let allowed = match &object_id {
                    None => access::member_role(&self.state.db, &vault_id, &self.who.user_id)
                        .await
                        .map(|_| true),
                    Some(o) => match sync::validate_object_id(o) {
                        Ok(()) => {
                            access::object_access(&self.state.db, &vault_id, o, &self.who.user_id)
                                .await
                                .map(|(_, m)| m)
                        }
                        Err(e) => Err(e),
                    },
                };
                let member = match allowed {
                    Ok(m) => m,
                    Err(e) => {
                        self.send(error_msg(&e, Some(&vault_id))).await;
                        return true;
                    }
                };
                let key = (vault_id.clone(), object_id.clone());
                if let Some(h) = self.subs.remove(&key) {
                    h.abort();
                }
                if let Some(o) = &object_id {
                    self.via_membership.remove(&(vault_id.clone(), o.clone()));
                }
                if self.subs.len() >= MAX_SUBSCRIPTIONS {
                    self.send(error_msg(
                        &AppError::BadRequest("too many subscriptions".into()),
                        Some(&vault_id),
                    ))
                    .await;
                    return true;
                }
                if let (Some(o), true) = (&object_id, member) {
                    self.via_membership.insert((vault_id.clone(), o.clone()));
                }
                let rx = self.state.hub.subscribe(&vault_id);
                let task = tokio::spawn(forward(
                    self.state.clone(),
                    vault_id,
                    object_id,
                    !member,
                    since,
                    self.conn_id,
                    rx,
                    self.out.clone(),
                ));
                self.subs.insert(key, task);
            }
            ClientMessage::Unsubscribe {
                vault_id,
                object_id,
            } => {
                if let Some(o) = &object_id {
                    self.via_membership.remove(&(vault_id.clone(), o.clone()));
                }
                if let Some(h) = self.subs.remove(&(vault_id, object_id)) {
                    h.abort();
                }
            }
            ClientMessage::Push { vault_id, updates } => {
                if !self.subs.keys().any(|(v, _)| *v == vault_id) {
                    self.send(error_msg(
                        &AppError::BadRequest("subscribe to the vault before pushing".into()),
                        Some(&vault_id),
                    ))
                    .await;
                    return true;
                }
                match sync::push(&self.state, &self.who, &vault_id, updates, self.conn_id).await {
                    // Fresh updates are acknowledged in log order by the
                    // subscription; retries of known updates and refusals
                    // of single updates are answered here.
                    Ok(out) if !out.duplicates.is_empty() || !out.rejected.is_empty() => {
                        self.send(ServerMessage::Ack {
                            vault_id,
                            results: out.duplicates,
                            rejected: out.rejected,
                        })
                        .await;
                    }
                    Ok(_) => {}
                    Err(e) => self.send(error_msg(&e, Some(&vault_id))).await,
                }
            }
            ClientMessage::Awareness {
                vault_id,
                object_id,
                payload,
            } => {
                // Vault subscribers may announce anywhere in the vault; people a
                // single note is shared with only on that note.
                let allowed = self.subs.contains_key(&(vault_id.clone(), None))
                    || (!object_id.is_empty()
                        && self
                            .subs
                            .contains_key(&(vault_id.clone(), Some(object_id.clone()))));
                if !allowed || payload.len() > MAX_AWARENESS_BYTES || object_id.len() > 64 {
                    return true;
                }
                if !self.awareness.take() {
                    return true;
                }
                if !object_id.is_empty() && self.announced.len() < MAX_SUBSCRIPTIONS {
                    self.announced.insert((vault_id.clone(), object_id.clone()));
                }
                self.state.hub.publish(
                    &vault_id,
                    VaultEvent::Awareness {
                        object_id,
                        from_user: self.who.user_id.clone(),
                        from_conn: self.conn_id,
                        payload,
                    },
                );
            }
        }
        true
    }
}

/// Send all changes after `since`; returns the head sequence reached.
async fn catch_up(
    state: &AppState,
    vault_id: &str,
    object: Option<&str>,
    doc_only: bool,
    mut since: i64,
    out: &mpsc::Sender<Out>,
) -> Result<i64, AppError> {
    let mut first = true;
    loop {
        let changes = sync::changes_since(
            &state.db,
            vault_id,
            object,
            since,
            sync::DEFAULT_PAGE,
            first,
            doc_only,
        )
        .await?;
        first = false;
        since = changes.next_since;
        let more = changes.has_more;
        out.send(Out::Msg(ServerMessage::Changes {
            vault_id: vault_id.to_string(),
            changes,
        }))
        .await
        .map_err(|_| AppError::Internal(anyhow::anyhow!("connection closed")))?;
        if !more {
            return Ok(since);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn forward(
    state: AppState,
    vault_id: String,
    object: Option<String>,
    doc_only: bool,
    since: Option<i64>,
    conn_id: u64,
    mut rx: broadcast::Receiver<std::sync::Arc<VaultEvent>>,
    out: mpsc::Sender<Out>,
) {
    let send = |m: ServerMessage| {
        let out = out.clone();
        async move { out.send(Out::Msg(m)).await.is_ok() }
    };
    let start = match since {
        Some(s) => {
            catch_up(
                &state,
                &vault_id,
                object.as_deref(),
                doc_only,
                s.max(0),
                &out,
            )
            .await
        }
        None => sync::head_seq(&state.db, &vault_id).await,
    };
    let mut last = match start {
        Ok(h) => h,
        Err(e) => {
            send(error_msg(&e, Some(&vault_id))).await;
            return;
        }
    };
    if !send(ServerMessage::Synced {
        vault_id: vault_id.clone(),
        head_seq: last,
        object_id: object.clone(),
    })
    .await
    {
        return;
    }
    loop {
        let ev = match rx.recv().await {
            Ok(ev) => ev,
            Err(broadcast::error::RecvError::Lagged(_)) => {
                // Too slow to keep up live: replay from the log instead.
                match catch_up(&state, &vault_id, object.as_deref(), doc_only, last, &out).await {
                    Ok(h) => {
                        last = h;
                        if !send(ServerMessage::Synced {
                            vault_id: vault_id.clone(),
                            head_seq: last,
                            object_id: object.clone(),
                        })
                        .await
                        {
                            return;
                        }
                        continue;
                    }
                    Err(_) => return,
                }
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        // Per-note subscriptions only see that note (no vault-wide activity).
        if let Some(o) = &object {
            match (&*ev, ev.object_id()) {
                (VaultEvent::Event(VaultEventKind::Deleted), _) => {
                    send(ServerMessage::ObjectEvent {
                        vault_id: vault_id.clone(),
                        object_id: o.clone(),
                        event: ObjectEventKind::Deleted,
                    })
                    .await;
                    return;
                }
                (_, Some(id)) if id == o => {}
                _ => continue,
            }
        }
        if let Some(seq) = ev.seq() {
            if seq <= last {
                continue;
            }
            last = seq;
        }
        let v = vault_id.clone();
        let msg = match &*ev {
            VaultEvent::Update {
                update,
                client_update_id,
                origin_conn,
            } => {
                if *origin_conn == conn_id {
                    ServerMessage::Ack {
                        vault_id: v,
                        results: vec![PushResult {
                            client_update_id: client_update_id.clone(),
                            seq: update.seq,
                        }],
                        rejected: vec![],
                    }
                } else {
                    ServerMessage::Update {
                        vault_id: v,
                        update: update.clone(),
                    }
                }
            }
            VaultEvent::Snapshot { snapshot } => ServerMessage::Snapshot {
                vault_id: v,
                snapshot: snapshot.clone(),
            },
            VaultEvent::SnapshotCreated {
                object_id,
                snapshot_id,
                covers_seq,
            } => ServerMessage::SnapshotCreated {
                vault_id: v,
                object_id: object_id.clone(),
                snapshot_id: snapshot_id.clone(),
                covers_seq: *covers_seq,
            },
            VaultEvent::Purged { object_id, seq } => ServerMessage::Purged {
                vault_id: v,
                object_id: object_id.clone(),
                seq: *seq,
            },
            VaultEvent::Awareness {
                object_id,
                from_user,
                from_conn,
                payload,
            } => {
                if *from_conn == conn_id {
                    continue;
                }
                ServerMessage::Awareness {
                    vault_id: v,
                    object_id: object_id.clone(),
                    from_user: from_user.clone(),
                    from_conn: *from_conn,
                    payload: payload.clone(),
                }
            }
            VaultEvent::Event(event) => ServerMessage::VaultEvent {
                vault_id: v,
                event: *event,
            },
            VaultEvent::Object { object_id, event } => ServerMessage::ObjectEvent {
                vault_id: v,
                object_id: object_id.clone(),
                event: *event,
            },
        };
        if !send(msg).await {
            return;
        }
    }
}

/// Small token bucket for per-connection message rates.
struct Bucket {
    rate: f64,
    cap: f64,
    tokens: f64,
    at: Instant,
}

impl Bucket {
    fn new(rate: f64, cap: f64) -> Self {
        Bucket {
            rate,
            cap,
            tokens: cap,
            at: Instant::now(),
        }
    }

    fn take(&mut self) -> bool {
        let now = Instant::now();
        self.tokens =
            (self.tokens + now.duration_since(self.at).as_secs_f64() * self.rate).min(self.cap);
        self.at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}
