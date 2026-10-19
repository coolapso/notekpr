//! Zip import (a file the user picks — possibly hostile): never panics and
//! respects its limits.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = coolpsonotes_web::zipfile::read(data);
});
