//! The client sync engine shared by the browser (wasm) and desktop clients.
//!
//! Both clients speak the same protocol; this crate holds the decisions they
//! must make the same way, so a fix lands once for both:
//!
//! * [`outbox`]: what to upload next — merging unsent updates per object,
//!   holding back what the server can't store, splitting into pushes — and
//!   what to do with acknowledgements and refusals.
//! * [`edits`]: turning local text changes into updates within the server's
//!   size limit.
//! * [`snapshot`]: when a snapshot may be stored.
//! * [`recover_from_rollback`]: re-uploading after the server lost history.
//!
//! It is *sans-IO*: no sockets, timers, storage or async runtime. Clients feed
//! it what happened (a local edit, a server message, an HTTP answer) and do
//! what it returns. That keeps it the same on wasm and tokio (no `Send`
//! questions, no async traits) and lets one test suite cover the logic
//! without a network. Replica state itself (CRDT documents, keys, deferred
//! content, acknowledgements) stays in `coolpsonotes_core::engine::VaultReplica`.

pub mod edits;
pub mod outbox;
pub mod snapshot;

pub use outbox::{
    Held, HoldReason, Limits, Outbox, Plan, Reaction, recover_from_rollback, stored_copies,
};
pub use snapshot::snapshot_now;

#[cfg(test)]
mod tests;
