//! The CLI's tests, as one test program: each file here is a module of it, so
//! everything is linked once rather than once per file (`autotests = false`
//! and `[[test]]` in Cargo.toml). A new file goes in the list below. One
//! file's tests: `cargo test -p notekpr-cli --test cli <file>::`.

mod common;

mod account;
mod adopt;
mod agent;
mod cli;
mod comments;
mod dates;
mod member_log;
mod mirror;
mod moving;
mod note_keys;
mod notes;
mod quick;
mod recover;
mod setup;
mod shapes;
mod sync;
mod transfer;

/// With `autotests = false`, a file in tests/ that isn't a module here would
/// never be built or run: this fails instead.
#[test]
fn every_test_file_is_a_module() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let main = std::fs::read_to_string(dir.join("main.rs")).unwrap();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if let Some(module) = name.strip_suffix(".rs")
            && module != "main"
        {
            assert!(
                main.contains(&format!("mod {module};")),
                "tests/{name} is not a module of tests/main.rs, so its tests never run"
            );
        }
    }
}
