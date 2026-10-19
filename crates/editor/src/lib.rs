//! The live-preview Markdown editor of the coolpsonotes web client.
//!
//! The document model — pure Rust, compiled and tested natively:
//! [`text::Text`] (the source with a line index), [`change::ChangeSet`]
//! (changes that compose, transform and map positions),
//! [`state::EditorState`] + [`state::Transaction`] (every edit is a
//! transaction: changes, selection, origin, undo grouping), [`decorations`]
//! (Markdown blocks per line, updated incrementally), [`syntax`] (inline
//! styling and HTML per line), [`commands`] (lists, tables, tasks,
//! formatting) and [`vim`].

pub mod change;
pub mod commands;
pub mod decorations;
pub mod state;
pub mod syntax;
pub mod text;
pub mod vim;

pub use change::{ChangeSet, edits_fit, rebase};
pub use commands::Command;
pub use state::{Anchor, Mode, RemoteCursor};
