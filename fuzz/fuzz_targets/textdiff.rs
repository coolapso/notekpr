//! The text diff behind every "write this text" (mirror, imports, CLI):
//! applying the edits it produces always gives the new text.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    // Split the input into an old and a new text at a char boundary.
    let mid = (0..=s.len() / 2).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0);
    let (old, new) = s.split_at(mid);
    let edits = notekpr_core::textdiff::diff(old, new);
    assert_eq!(notekpr_core::textdiff::apply(old, &edits), new);
});
