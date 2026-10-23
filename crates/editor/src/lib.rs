//! The live-preview Markdown editor of the notekpr web client.
//!
//! Two layers:
//!
//! * **The document model** — pure Rust, compiled and tested natively:
//!   [`text::Text`] (the source with a line index), [`change::ChangeSet`]
//!   (changes that compose, transform and map positions),
//!   [`state::EditorState`] + [`state::Transaction`] (every edit is a
//!   transaction: changes, selection, origin, undo grouping),
//!   [`decorations`] (Markdown blocks per line, updated incrementally),
//!   [`syntax`] (inline styling and HTML per line), [`code`] (code block
//!   options and syntax highlighting), [`color`] (text and background
//!   colors), [`commands`] (lists, tables, tasks, formatting) and [`vim`].
//! * **The DOM view** — [`view::Editor`], a `contenteditable` element whose
//!   text is always exactly the Markdown source, one `div.ln` per line. A
//!   transaction re-renders only the lines it touched.
//!
//! The rules that keep the view safe are spelled out in [`view`]: the state
//! is only borrowed inside pure closures, DOM and host calls happen after the
//! borrow ends, and anything that would run while an operation is in
//! progress (a re-entrant API call, an event fired synchronously by a DOM
//! call) is queued instead.

pub mod change;
pub mod code;
pub mod color;
pub mod commands;
pub mod decorations;
pub mod html;
pub mod state;
pub mod syntax;
pub mod text;
pub mod view;
pub mod vim;

pub use change::{ChangeSet, edits_fit, rebase};
pub use commands::Command;
pub use state::{Anchor, Mark, Mode, RemoteCursor};
pub use view::{Editor, Host};
