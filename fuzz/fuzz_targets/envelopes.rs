//! Ciphertext parsing: malformed or tampered envelopes are refused, never
//! panic, and never decrypt.
#![no_main]
use notekpr_core::crypto::{self, Purpose};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let key = [7u8; 32];
    let _ = crypto::sym_epoch(data);
    assert!(crypto::open_sym(&key, Purpose::Update, &[b"v", b"o"], data).is_err());
    // A real envelope with one byte changed is refused too.
    if let Some((&first, rest)) = data.split_first() {
        let mut ct = crypto::seal_sym(&key, 1, Purpose::Update, &[b"v", b"o"], rest);
        let i = first as usize % ct.len();
        ct[i] ^= 0x5a;
        assert!(crypto::open_sym(&key, Purpose::Update, &[b"v", b"o"], &ct).is_err());
    }
});
