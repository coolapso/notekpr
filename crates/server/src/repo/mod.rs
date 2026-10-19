//! The server's SQL, in one place: small, focused functions per table group.
//!
//! Handlers and [`crate::access`] call these instead of writing queries
//! inline, so every authorization-sensitive query (memberships, roles, note
//! shares, device ownership, sessions) can be read, reviewed and tested
//! here. *Decisions* stay outside: `access.rs` decides who may do what, the
//! routes validate requests and map results to API errors. The functions
//! here only read and write rows; they don't check permissions.
//!
//! The encrypted log itself (updates, snapshots, purges, catch-up pages)
//! lives in [`crate::sync`]; database setup, migrations and backups in
//! [`crate::db`]; attachment bytes go through [`crate::blobs`], whose SQLite
//! store uses [`attachments`].
//!
//! Conventions:
//! * Single-statement functions take any executor (`&Db` or `&mut *tx`), so
//!   a handler can run them inside its transaction. Functions that combine
//!   several reads take `&Db`.
//! * Lookups return `Option`; the caller decides whether a missing row is
//!   `404` (existence must not leak, see AGENTS.md).
//! * Inserts that can hit a uniqueness constraint return `Ok(false)` when
//!   the row already exists; the caller words the conflict.
//! * Updates and deletes return the number of rows affected where callers
//!   need to tell "nothing matched" apart.

pub mod agents;
pub mod attachments;
pub mod audit;
pub mod devices;
pub mod keys;
pub mod sessions;
pub mod shares;
pub mod stats;
pub mod users;
pub mod vaults;

#[cfg(test)]
mod tests;

use sqlx::sqlite::SqliteQueryResult;

use crate::error::AppResult;

/// `Ok(true)` if inserted, `Ok(false)` on a uniqueness violation.
fn inserted(r: Result<SqliteQueryResult, sqlx::Error>) -> AppResult<bool> {
    match r {
        Ok(_) => Ok(true),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Ok(false),
        Err(e) => Err(e.into()),
    }
}
