//! Terminal interaction. Status and warnings go to **stderr** so stdout stays
//! clean for command output.

use std::io::{BufRead, IsTerminal, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};

static QUIET: AtomicBool = AtomicBool::new(false);
static SILENT: AtomicBool = AtomicBool::new(false);
static NO_PROMPTS: AtomicBool = AtomicBool::new(false);

/// Suppress informational status lines (warnings are still shown).
pub fn set_quiet(q: bool) {
    QUIET.store(q, Ordering::Relaxed);
}

/// Print no status lines and no warnings at all: for programs that use
/// this library for many clients at once (the load test) and show what
/// happens their own way.
pub fn set_silent(on: bool) {
    SILENT.store(on, Ordering::Relaxed);
}

/// Never prompt, as if stdin were not a terminal (tests).
pub fn set_non_interactive(on: bool) {
    NO_PROMPTS.store(on, Ordering::Relaxed);
}

/// What a command run for another process prints (a one-shot command
/// handed to the running daemon, see `control`): collected here instead of
/// going to this process's terminal.
#[derive(Debug, Default)]
pub struct Captured {
    pub stdout: String,
    pub stderr: String,
    quiet: bool,
    stdin: Option<Vec<u8>>,
}

tokio::task_local! {
    static CAPTURE: Arc<Mutex<Captured>>;
}

/// Run `f` with what it prints collected, `stdin` as what it reads from
/// stdin, never prompting, and its own `quiet` setting.
pub async fn capture<F: std::future::Future>(
    stdin: Option<Vec<u8>>,
    quiet: bool,
    f: F,
) -> (F::Output, Captured) {
    let cell = Arc::new(Mutex::new(Captured {
        quiet,
        stdin,
        ..Default::default()
    }));
    let out = CAPTURE.scope(cell.clone(), f).await;
    let c = std::mem::take(&mut *cell.lock().unwrap());
    (out, c)
}

fn captured<R>(f: impl FnOnce(&mut Captured) -> R) -> Option<R> {
    CAPTURE.try_with(|c| f(&mut c.lock().unwrap())).ok()
}

/// One line of command output (stdout); [`crate::out!`] formats it.
pub fn out_line(line: &str) {
    let kept = captured(|c| {
        c.stdout.push_str(line);
        c.stdout.push('\n');
    });
    if kept.is_none() {
        println!("{line}");
    }
}

/// Command output without a line break of its own (text that has one).
pub fn out_text(text: &str) {
    if captured(|c| c.stdout.push_str(text)).is_none() {
        print!("{text}");
    }
}

/// `println!` for command output, which may be collected ([`capture`]).
#[macro_export]
macro_rules! out {
    () => {
        $crate::ui::out_line("")
    };
    ($($t:tt)*) => {
        $crate::ui::out_line(&format!($($t)*))
    };
}

pub fn status(msg: &str) {
    let kept = captured(|c| {
        if !c.quiet {
            c.stderr.push_str(msg);
            c.stderr.push('\n');
        }
    });
    if kept.is_none() && !QUIET.load(Ordering::Relaxed) && !SILENT.load(Ordering::Relaxed) {
        eprintln!("{msg}");
    }
}

pub fn warn(msg: &str) {
    let kept = captured(|c| {
        c.stderr.push_str(&format!("warning: {msg}\n"));
    });
    if kept.is_none() && !SILENT.load(Ordering::Relaxed) {
        eprintln!("warning: {msg}");
    }
}

pub fn stdin_is_tty() -> bool {
    captured(|_| ()).is_none()
        && !NO_PROMPTS.load(Ordering::Relaxed)
        && std::io::stdin().is_terminal()
}

/// All of stdin (for a collected command: what came with it).
pub fn read_stdin() -> std::io::Result<Vec<u8>> {
    if let Some(given) = captured(|c| c.stdin.take()) {
        return Ok(given.unwrap_or_default());
    }
    let mut data = Vec::new();
    std::io::stdin().read_to_end(&mut data)?;
    Ok(data)
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
