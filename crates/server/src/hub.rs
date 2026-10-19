//! In-process fan-out of vault events to WebSocket connections.
//!
//! Every write that changes a vault's update log happens while holding the
//! global write lock, and the corresponding event is published before the lock
//! is released. Subscribers therefore observe events in strictly increasing
//! sequence order, which lets clients treat "last sequence received" as "has
//! everything up to here".

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use coolpsonotes_core::protocol::{ObjectEventKind, Snapshot, Update, UserEventKind, VaultEventKind};
use dashmap::DashMap;
use tokio::sync::{broadcast, mpsc};

const VAULT_CHANNEL_CAPACITY: usize = 2048;

#[derive(Debug, Clone)]
pub enum VaultEvent {
    Update { update: Update, client_update_id: String, origin_conn: u64 },
    Snapshot { snapshot: Snapshot },
    SnapshotCreated { object_id: String, snapshot_id: String, covers_seq: i64 },
    Purged { object_id: String, seq: i64 },
    Awareness { object_id: String, from_user: String, from_conn: u64, payload: Vec<u8> },
    Event(VaultEventKind),
    Object { object_id: String, event: ObjectEventKind },
}

impl VaultEvent {
    /// The object an event concerns (`None` for vault-wide events).
    pub fn object_id(&self) -> Option<&str> {
        match self {
            VaultEvent::Update { update, .. } => Some(&update.object_id),
            VaultEvent::Snapshot { snapshot } => Some(&snapshot.object_id),
            VaultEvent::SnapshotCreated { object_id, .. }
            | VaultEvent::Purged { object_id, .. }
            | VaultEvent::Awareness { object_id, .. }
            | VaultEvent::Object { object_id, .. } => Some(object_id),
            VaultEvent::Event(_) => None,
        }
    }

    /// Sequence number for log events (used to de-duplicate against catch-up).
    pub fn seq(&self) -> Option<i64> {
        match self {
            VaultEvent::Update { update, .. } => Some(update.seq),
            VaultEvent::Purged { seq, .. } => Some(*seq),
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

#[derive(Default)]
pub struct Hub {
    vaults: DashMap<String, broadcast::Sender<Arc<VaultEvent>>>,
    users: DashMap<String, Vec<(u64, mpsc::UnboundedSender<Control>)>>,
    next_conn: AtomicU64,
}

impl Hub {
    pub fn new() -> Self {
        Hub { next_conn: AtomicU64::new(1), ..Default::default() }
    }

    pub fn next_conn_id(&self) -> u64 {
        self.next_conn.fetch_add(1, Ordering::Relaxed)
    }

    pub fn subscribe(&self, vault_id: &str) -> broadcast::Receiver<Arc<VaultEvent>> {
        self.vaults
            .entry(vault_id.to_string())
            .or_insert_with(|| broadcast::channel(VAULT_CHANNEL_CAPACITY).0)
            .subscribe()
    }

    pub fn publish(&self, vault_id: &str, event: VaultEvent) {
        let mut empty = false;
        if let Some(tx) = self.vaults.get(vault_id) {
            empty = tx.send(Arc::new(event)).is_err();
        }
        if empty {
            self.vaults.remove_if(vault_id, |_, tx| tx.receiver_count() == 0);
        }
    }

    pub fn register_user(&self, user_id: &str, conn_id: u64) -> mpsc::UnboundedReceiver<Control> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.users.entry(user_id.to_string()).or_default().push((conn_id, tx));
        rx
    }

    pub fn unregister_user(&self, user_id: &str, conn_id: u64) {
        let mut remove = false;
        if let Some(mut v) = self.users.get_mut(user_id) {
            v.retain(|(c, _)| *c != conn_id);
            remove = v.is_empty();
        }
        if remove {
            self.users.remove_if(user_id, |_, v| v.is_empty());
        }
    }

    pub fn control(&self, user_id: &str, c: Control) {
        if let Some(v) = self.users.get(user_id) {
            for (_, tx) in v.iter() {
                let _ = tx.send(c.clone());
            }
        }
    }

    pub fn connection_count(&self) -> usize {
        self.users.iter().map(|e| e.value().len()).sum()
    }
}
