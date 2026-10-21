//! Markdown analysis (links, tags, headings, tasks) and link decoding never
//! panic, whatever the bytes, and stay within the text.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let info = notekpr_core::markdown::analyze(text);
    for h in &info.headings {
        assert!(h.offset <= text.len());
    }
    let _ = notekpr_core::markdown::percent_decode(text);
    for line in text.lines() {
        let _ = notekpr_core::markdown::heading(line);
    }
    let _ = notekpr_core::search::parse_query(text);
});
