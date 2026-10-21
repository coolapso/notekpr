//! Every message a client or server parses from the network: arbitrary JSON
//! never panics.
#![no_main]
use notekpr_core::protocol::{ChangesResponse, ClientMessage, ServerMessage};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = serde_json::from_slice::<ServerMessage>(data);
    let _ = serde_json::from_slice::<ClientMessage>(data);
    let _ = serde_json::from_slice::<ChangesResponse>(data);
});
