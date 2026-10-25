//! Shared core of notekpr: cryptography, key hierarchy, wire protocol and
//! the CRDT document model. Compiled natively for the server, desktop client
//! and agent gateway, and to WebAssembly for the browser client, so every
//! component encrypts and merges documents with exactly the same code.

pub mod b64;
pub mod comments;
pub mod crypto;
pub mod dates;
pub mod doc;
pub mod engine;
pub mod keys;
pub mod markdown;
pub mod memberlog;
pub mod moving;
pub mod order;
pub mod paths;
pub mod protocol;
pub mod quick;
pub mod recover;
pub mod search;
pub mod textdiff;

pub use yrs;

/// Current time in Unix milliseconds.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Random 128-bit identifier, hex encoded (used for update ids and object ids).
pub fn random_id() -> String {
    data_encoding::HEXLOWER.encode(&crypto::random_bytes::<16>())
}
