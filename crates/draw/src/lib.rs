//! Drawings: notekpr's own Excalidraw-compatible drawing model.
//!
//! Pure Rust (native tests, also built for wasm32): Excalidraw's file
//! formats, and its hand-drawn look — ports of rough.js 4.6.4 and
//! perfect-freehand 1.2.0, the versions Excalidraw uses, so the same file
//! draws the same picture.

pub mod bounds;
pub mod color;
pub mod file;
pub mod fonts;
pub mod freehand;
pub mod geom;
pub mod js;
pub mod lzstring;
pub mod obsidian;
pub mod order;
pub mod restore;
pub mod rough;
pub mod scene;
pub mod shape;
pub mod svg;
