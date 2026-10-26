//! Fan-out of vault events and per-user control messages, behind one small
//! interface ([`EventBus`]).
//!
//! Today everything runs in one process ([`EventBus::Local`]: a broadcast
//! channel per vault and a queue per WebSocket connection). A backend that
//! connects several server processes sharing one database (polling the log,
//! or a message broker) can be added as another variant without touching the
//! callers: routes and `sync.rs` only call [`EventBus::publish`] and
//! [`EventBus::control`], and `ws.rs` only [`EventBus::connect`],
//! [`EventBus::subscribe`] and the receive halves returned by them.
//!
//! # Guarantees every backend must give
//!
//! `ws.rs` builds the gapless, in-order delivery of each vault's log on these
//! (see its `forward`); they are part of the interface, not of one backend.
//!
//! 1. **Per-vault order.** Events published for one vault reach each
//!    subscription in the order they were published. Writers of log events
//!    ([`VaultEvent::Update`], [`VaultEvent::Purged`],
//!    [`VaultEvent::Restored`]) commit and publish
//!    while holding the server's write lock, so publish order is sequence
//!    order. A multi-process backend must order a vault's log events by
//!    their sequence number, whichever process wrote them.
//! 2. **Nothing published after `subscribe` returns is lost silently.**
//!    `ws.rs` subscribes *first*, then reads the log from the client's
//!    position (catch-up), then forwards live events, skipping log events at
//!    or below the head it read. Every update committed after the catch-up
//!    read therefore arrives live. A polling backend must start no later
//!    than the moment `subscribe` returns.
//! 3. **Gaps are reported.** A subscriber that falls too far behind gets
//!    [`RecvError::Lagged`] (once) instead of silently missing events, then
//!    continues with newer ones; `ws.rs` replays the gap from the log. Other
//!    events missed in a gap (presence, vault and object notices) are not
//!    replayed: presence is ephemeral and clients refresh on reconnect.
//! 4. **Duplicates.** Log events may be delivered more than once; subscribers
//!    drop sequence numbers they already passed. Other events should be
//!    delivered at most once.
//! 5. **Control messages** ([`Control`]) reach every connection the user has
//!    open at the time of the call, on any process, in call order. They are
//!    never dropped while the connection lives (they revoke access).
//! 6. **Connection ids** are unique among all connections sharing the bus and
//!    never `0` (`origin_conn = 0` marks a REST push without a connection).
//! 7. **Publishing never blocks or fails** for the caller: it happens while
//!    the write lock is held. A remote backend queues internally.
//!
//! Events carry only what the server already holds: ciphertext, ids, sequence
//! numbers and encrypted presence payloads — never plaintext.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use notekpr_core::protocol::{ObjectEventKind, Snapshot, Update, UserEventKind, VaultEventKind};
use tokio::sync::{broadcast, mpsc};

/// Live events kept per vault for subscribers that fall behind. A
/// subscriber more than this behind is told it lagged and catches up from
/// the log instead (`ws::forward`), so this only needs to cover bursts. Each
/// slot keeps its last event until it is overwritten: with thousands of
/// vaults, a bigger number costs memory (2048 cost about a quarter more per
/// connected person in the load test, for no gain).
const VAULT_CHANNEL_CAPACITY: usize = 256;

#[derive(Debug, Clone)]
pub enum VaultEvent {
    Update {
        update: Update,
        client_update_id: String,
        origin_conn: u64,
    },
    Snapshot {
        snapshot: Snapshot,
    },
    SnapshotCreated {
        object_id: String,
        snapshot_id: String,
        covers_seq: i64,
    },
    Purged {
        object_id: String,
        seq: i64,
    },
    /// Deleting an object for good was undone.
    Restored {
        object_id: String,
        seq: i64,
    },
    Awareness {
        object_id: String,
        from_user: String,
        from_conn: u64,
        payload: Vec<u8>,
    },
    Event(VaultEventKind),
    Object {
        object_id: String,
        event: ObjectEventKind,
    },
}

impl VaultEvent {
    /// The object an event concerns (`None` for vault-wide events).
    pub fn object_id(&self) -> Option<&str> {
        match self {
            VaultEvent::Update { update, .. } => Some(&update.object_id),
            VaultEvent::Snapshot { snapshot } => Some(&snapshot.object_id),
            VaultEvent::SnapshotCreated { object_id, .. }
            | VaultEvent::Purged { object_id, .. }
            | VaultEvent::Restored { object_id, .. }
            | VaultEvent::Awareness { object_id, .. }
            | VaultEvent::Object { object_id, .. } => Some(object_id),
            VaultEvent::Event(_) => None,
        }
    }

    /// Sequence number for log events (used to de-duplicate against catch-up).
    pub fn seq(&self) -> Option<i64> {
        match self {
            VaultEvent::Update { update, .. } => Some(update.seq),
            VaultEvent::Purged { seq, .. } | VaultEvent::Restored { seq, .. } => Some(*seq),
            _ => None,
        }
    }
}

/// Out-of-band instructions for a specific user's connections.
#[derive(Debug, Clone)]
pub enum Control {
    /// The user lost access to a vault: drop the subscription.
    RevokeVault(String),
    /// The user lost access to an individually shared note.
    RevokeObject(String, String),
    /// Session revoked (logout, device removal): close the connection.
    SessionRevoked(String),
    User(UserEventKind),
}

/// Why [`VaultSubscription::recv`] returned no event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvError {
    /// Events were dropped because this subscriber fell behind; replay the
    /// log from the last sequence number seen, then continue receiving.
    Lagged,
    /// The bus shut down; no further events will arrive.
    Closed,
}

/// The event fan-out of this server. See the module docs for the guarantees
/// every variant gives.
pub enum EventBus {
    /// Single process: in-memory channels.
    Local(LocalBus),
}

impl Default for EventBus {
    fn default() -> Self {
        EventBus::Local(LocalBus::new())
    }
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a live connection of `user_id`: a fresh connection id and
    /// the receiver of the user's [`Control`] messages. Call
    /// [`EventBus::disconnect`] when the connection ends.
    pub fn connect(&self, user_id: &str) -> (u64, ControlReceiver) {
        match self {
            EventBus::Local(b) => b.connect(user_id),
        }
    }

    pub fn disconnect(&self, user_id: &str, conn_id: u64) {
        match self {
            EventBus::Local(b) => b.disconnect(user_id, conn_id),
        }
    }

    /// Start receiving a vault's events. Everything published after this
    /// returns is delivered (or reported as [`RecvError::Lagged`]).
    pub fn subscribe(&self, vault_id: &str) -> VaultSubscription {
        match self {
            EventBus::Local(b) => b.subscribe(vault_id),
        }
    }

    /// Deliver an event to the vault's subscribers. Log events must be
    /// published after their transaction committed, under the write lock.
    pub fn publish(&self, vault_id: &str, event: VaultEvent) {
        match self {
            EventBus::Local(b) => b.publish(vault_id, event),
        }
    }

    /// Deliver a control message to every open connection of a user.
    pub fn control(&self, user_id: &str, c: Control) {
        match self {
            EventBus::Local(b) => b.control(user_id, c),
        }
    }

    /// Live connections on this process (for metrics).
    pub fn connection_count(&self) -> usize {
        match self {
            EventBus::Local(b) => b.connection_count(),
        }
    }

    /// People with at least one live connection on this process (metrics).
    pub fn connected_users(&self) -> usize {
        match self {
            EventBus::Local(b) => b.users.len(),
        }
    }
}

/// The receiving half of [`EventBus::subscribe`].
pub struct VaultSubscription(SubscriptionInner);

enum SubscriptionInner {
    Local(broadcast::Receiver<Arc<VaultEvent>>),
}

impl VaultSubscription {
    /// The next event, in publish order.
    pub async fn recv(&mut self) -> Result<Arc<VaultEvent>, RecvError> {
        match &mut self.0 {
            SubscriptionInner::Local(rx) => rx.recv().await.map_err(|e| match e {
                broadcast::error::RecvError::Lagged(_) => RecvError::Lagged,
                broadcast::error::RecvError::Closed => RecvError::Closed,
            }),
        }
    }
}

/// The receiving half of [`EventBus::connect`].
pub struct ControlReceiver(ControlInner);

enum ControlInner {
    Local(mpsc::UnboundedReceiver<Control>),
}

impl ControlReceiver {
    /// The next control message; `None` once the bus dropped the connection.
    pub async fn recv(&mut self) -> Option<Control> {
        match &mut self.0 {
            ControlInner::Local(rx) => rx.recv().await,
        }
    }
}

/// In-process fan-out: one broadcast channel per vault with subscribers,
/// one unbounded queue per connection.
pub struct LocalBus {
    vaults: DashMap<String, broadcast::Sender<Arc<VaultEvent>>>,
    users: DashMap<String, Vec<(u64, mpsc::UnboundedSender<Control>)>>,
    next_conn: AtomicU64,
}

impl Default for LocalBus {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalBus {
    pub fn new() -> Self {
        LocalBus {
            vaults: DashMap::new(),
            users: DashMap::new(),
            next_conn: AtomicU64::new(1),
        }
    }

    fn connect(&self, user_id: &str) -> (u64, ControlReceiver) {
        let conn_id = self.next_conn.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        self.users
            .entry(user_id.to_string())
            .or_default()
            .push((conn_id, tx));
        (conn_id, ControlReceiver(ControlInner::Local(rx)))
    }

    fn disconnect(&self, user_id: &str, conn_id: u64) {
        let mut remove = false;
        if let Some(mut v) = self.users.get_mut(user_id) {
            v.retain(|(c, _)| *c != conn_id);
            remove = v.is_empty();
        }
        if remove {
            self.users.remove_if(user_id, |_, v| v.is_empty());
        }
    }

    fn subscribe(&self, vault_id: &str) -> VaultSubscription {
        let rx = self
            .vaults
            .entry(vault_id.to_string())
            .or_insert_with(|| broadcast::channel(VAULT_CHANNEL_CAPACITY).0)
            .subscribe();
        VaultSubscription(SubscriptionInner::Local(rx))
    }

    fn publish(&self, vault_id: &str, event: VaultEvent) {
        let mut empty = false;
        if let Some(tx) = self.vaults.get(vault_id) {
            empty = tx.send(Arc::new(event)).is_err();
        }
        if empty {
            self.vaults
                .remove_if(vault_id, |_, tx| tx.receiver_count() == 0);
        }
    }

    fn control(&self, user_id: &str, c: Control) {
        if let Some(v) = self.users.get(user_id) {
            for (_, tx) in v.iter() {
                let _ = tx.send(c.clone());
            }
        }
    }

    fn connection_count(&self) -> usize {
        self.users.iter().map(|e| e.value().len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn purged(seq: i64) -> VaultEvent {
        VaultEvent::Purged {
            object_id: "o".into(),
            seq,
        }
    }

    #[tokio::test]
    async fn subscribers_get_everything_published_after_subscribing_in_order() {
        let bus = EventBus::new();
        bus.publish("v", purged(1)); // nobody listening: dropped
        let mut a = bus.subscribe("v");
        let mut other = bus.subscribe("w");
        for s in 2..=5 {
            bus.publish("v", purged(s));
        }
        let mut b = bus.subscribe("v");
        bus.publish("v", purged(6));
        for s in 2..=6 {
            assert_eq!(a.recv().await.unwrap().seq(), Some(s));
        }
        assert_eq!(b.recv().await.unwrap().seq(), Some(6));
        bus.publish("w", purged(9));
        assert_eq!(other.recv().await.unwrap().seq(), Some(9));
    }

    #[tokio::test]
    async fn slow_subscribers_are_told_they_lagged() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe("v");
        let n = VAULT_CHANNEL_CAPACITY as i64 + 10;
        for s in 1..=n {
            bus.publish("v", purged(s));
        }
        assert_eq!(rx.recv().await.unwrap_err(), RecvError::Lagged);
        // Delivery continues with the newest events that were kept, in order.
        let first = rx.recv().await.unwrap().seq().unwrap();
        assert!(first > 1);
        assert_eq!(rx.recv().await.unwrap().seq(), Some(first + 1));
    }

    #[tokio::test]
    async fn control_messages_reach_every_open_connection_of_the_user() {
        let bus = EventBus::new();
        let (c1, mut r1) = bus.connect("alice");
        let (c2, mut r2) = bus.connect("alice");
        let (c3, mut r3) = bus.connect("bob");
        assert!(c1 != 0 && c2 != 0 && c3 != 0);
        assert!(c1 != c2 && c2 != c3 && c1 != c3);
        assert_eq!(bus.connection_count(), 3);
        assert_eq!(bus.connected_users(), 2, "alice twice and bob");

        bus.control("alice", Control::RevokeVault("v".into()));
        assert!(matches!(r1.recv().await, Some(Control::RevokeVault(v)) if v == "v"));
        assert!(matches!(r2.recv().await, Some(Control::RevokeVault(v)) if v == "v"));

        bus.disconnect("alice", c1);
        assert_eq!(bus.connection_count(), 2);
        assert_eq!(bus.connected_users(), 2, "alice still has one");
        bus.control("alice", Control::SessionRevoked("s".into()));
        assert!(r1.recv().await.is_none());
        assert!(matches!(r2.recv().await, Some(Control::SessionRevoked(s)) if s == "s"));

        bus.control("bob", Control::User(UserEventKind::VaultsChanged));
        assert!(matches!(
            r3.recv().await,
            Some(Control::User(UserEventKind::VaultsChanged))
        ));
    }
}
