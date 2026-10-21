//! notekpr browser client.
//!
//! Written in Rust and compiled to WebAssembly: the same core crate that the
//! server and desktop client use does all cryptography and CRDT merging here,
//! so plaintext and keys never leave the browser.

pub mod actions;
pub mod api;
pub mod cache;
pub mod editor;
pub mod graph;
pub mod knowledge;
pub mod quick;
pub mod render;
pub mod sizes;
pub mod state;
pub mod storage;
pub mod util;
pub mod views;
pub mod ws;
pub mod zipfile;

use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    if let Some(boot) = util::document().get_element_by_id("boot") {
        boot.remove();
    }
    leptos::mount::mount_to_body(|| {
        state::init_ui();
        state::listen_popstate();
        state::start_presence_heartbeat();
        leptos::task::spawn_local(state::boot());
        leptos::view! { <views::App/> }
    });
}
