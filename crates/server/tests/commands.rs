//! The commands a deployment runs unattended report through the log, so with
//! `--log-format json` every line they write is JSON with a level, as log
//! shippers read it: a line they can't read a level from is shipped whatever
//! it says.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

/// Runs `notekpr-server` on `data` with JSON logs and returns every line it
/// wrote, on either stream, each parsed as JSON.
fn run_json(data: &Path, args: &[&str], stdin: &str) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_notekpr-server"))
        .arg("--data-dir")
        .arg(data)
        .args(args)
        .env("NOTEKPR_LOG_FORMAT", "json")
        .env_remove("NOTEKPR_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{args:?} failed:\n{text}");
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l)
                .unwrap_or_else(|e| panic!("{args:?} wrote a line that isn't JSON ({e}): {l}"))
        })
        .collect()
}

/// The line whose message is `message`, at INFO.
fn line<'a>(lines: &'a [Value], message: &str) -> &'a Value {
    let found = lines
        .iter()
        .find(|l| l["message"] == message)
        .unwrap_or_else(|| panic!("no {message:?} in {lines:#?}"));
    assert_eq!(found["level"], "INFO", "{found}");
    found
}

#[test]
fn unattended_commands_log_json() {
    let data = tempfile::tempdir().unwrap();
    let backups = tempfile::tempdir().unwrap();
    let dir = backups.path().to_str().unwrap();

    let lines = run_json(data.path(), &["migrate"], "");
    line(&lines, "database is up to date");

    let token = format!("notekpr_a_{}", "t".repeat(32));
    let lines = run_json(
        data.path(),
        &[
            "admin-client",
            "ensure",
            "--name",
            "billing",
            "--scopes",
            "accounts",
            "--tokens-file",
            "-",
        ],
        &token,
    );
    let ensured = line(&lines, "admin client ensured");
    assert_eq!(ensured["name"], "billing");
    assert_eq!(ensured["created"], true);
    assert_eq!(ensured["tokens_added"], 1);
    assert!(
        !lines.iter().any(|l| l.to_string().contains(&token)),
        "the token was logged"
    );

    // Two backups, keeping one: the second removes the first. Their names go
    // by the second, so they're a second apart.
    let lines = run_json(
        data.path(),
        &["backup", "--out-dir", dir, "--keep", "1"],
        "",
    );
    let first = line(&lines, "backup written")["path"]
        .as_str()
        .unwrap()
        .to_string();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let lines = run_json(
        data.path(),
        &["backup", "--out-dir", dir, "--keep", "1"],
        "",
    );
    line(&lines, "backup written");
    assert_eq!(
        line(&lines, "removed an old backup")["path"],
        first.as_str()
    );
    assert!(!Path::new(&first).exists());
}
