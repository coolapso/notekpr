//! CRDT updates from the network (after decryption, so from a member):
//! applying arbitrary bytes as an update never panics.
#![no_main]
use notekpr_core::doc::{IndexDoc, NoteDoc};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let n = NoteDoc::new();
    let _ = n.apply_update(data);
    let _ = n.text();
    let i = IndexDoc::new();
    let _ = i.apply_update(data);
    let _ = i.resolved_paths();
});
