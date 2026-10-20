//! coolpsonotes desktop client library: profiles and local key storage, the
//! sync engine (REST catch-up + live WebSocket), the plaintext file mirror,
//! and the local agent gateway (HTTP). The `coolpsonotes`
//! binary is a thin CLI over these modules; integration tests drive them
//! directly against an in-process server.

pub mod account;
pub mod api;
pub mod audit;
pub mod cli;
pub mod commands;
pub mod control;
pub mod fsutil;
pub mod gateway;
pub mod hub;
pub mod livesync;
pub mod manage;
pub mod mirror;
pub mod note_shares;
pub mod ops;
pub mod profile;
pub mod recover;
pub mod secrets;
pub mod session;
pub mod store;
pub mod transfer;
pub mod ui;
pub mod vault;
