//! The live Markdown editor lives in its own crate, `notekpr-editor`
//! (`crates/editor`: document model, transactions, decorations and the DOM
//! view). This module re-exports it under the paths the web client uses.

pub use notekpr_editor::*;
