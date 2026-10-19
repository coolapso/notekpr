//! Terminal interaction. Status and warnings go to **stderr** so stdout stays
//! clean for command output and for the MCP protocol stream.

use std::io::{BufRead, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};

static QUIET: AtomicBool = AtomicBool::new(false);

/// Suppress informational status lines (warnings are still shown).
pub fn set_quiet(q: bool) {
    QUIET.store(q, Ordering::Relaxed);
}

pub fn status(msg: &str) {
    if !QUIET.load(Ordering::Relaxed) {
        eprintln!("{msg}");
    }
}

pub fn warn(msg: &str) {
    eprintln!("warning: {msg}");
}

pub fn stdin_is_tty() -> bool {
    std::io::stdin().is_terminal()
}

pub fn prompt_line(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let mut s = String::new();
    if std::io::stdin().lock().read_line(&mut s)? == 0 {
        bail!("no input");
    }
    Ok(s.trim().to_string())
}

/// Read a secret without echoing it (falls back to a plain line if stdin is
/// not a terminal).
pub fn prompt_secret(prompt: &str) -> Result<String> {
    if stdin_is_tty() {
        Ok(rpassword::prompt_password(prompt)?.trim().to_string())
    } else {
        prompt_line(prompt)
    }
}

/// Ask a yes/no question; `false` when not interactive.
pub fn confirm(prompt: &str) -> bool {
    if !stdin_is_tty() {
        return false;
    }
    matches!(
        prompt_line(&format!("{prompt} [y/N] ")).as_deref(),
        Ok("y" | "Y" | "yes" | "YES" | "Yes")
    )
}

pub fn fmt_time(ms: i64) -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
        .ok()
        .and_then(|t| t.replace_nanosecond(0).ok())
        .and_then(|t| t.format(&Rfc3339).ok())
        .unwrap_or_else(|| ms.to_string())
}

pub fn fmt_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn formatting() {
        assert_eq!(super::fmt_time(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::fmt_size(512), "512 B");
        assert_eq!(super::fmt_size(2048), "2.0 KiB");
    }
}
