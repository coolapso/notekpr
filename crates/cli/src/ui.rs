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

/// The settings of a terminal, put back when this is dropped: echo after
/// [`HiddenLines`] turned it off. (Unix; elsewhere this does nothing.)
pub struct TerminalSettings {
    #[cfg(unix)]
    saved: Option<(i32, libc::termios)>,
}

impl TerminalSettings {
    #[cfg(unix)]
    fn of_fd(fd: i32) -> TerminalSettings {
        let mut t = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: tcgetattr only writes to `t`; it is read only if that worked.
        let ok = unsafe { libc::tcgetattr(fd, t.as_mut_ptr()) } == 0;
        TerminalSettings {
            // SAFETY: initialised by the successful tcgetattr above.
            saved: ok.then(|| (fd, unsafe { t.assume_init() })),
        }
    }
}

impl Drop for TerminalSettings {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some((fd, t)) = &self.saved {
            // SAFETY: `t` came from tcgetattr on the same descriptor.
            unsafe { libc::tcsetattr(*fd, libc::TCSANOW, t) };
        }
    }
}

/// Lines typed at the terminal without echo, read on a thread of their own
/// while the program waits for something else (a recovery key typed while
/// another device is to approve this one). Dropping this stops the reading
/// before it returns: a reader still waiting on the terminal would take the
/// answer to whatever is asked next. So the thread never blocks in a read:
/// it reads only what is there, a whole line (the terminal hands over
/// nothing before Enter), and looks every 100 ms whether to stop. Unix
/// only: elsewhere a read can't be called off.
pub struct HiddenLines {
    #[cfg(unix)]
    stop: Arc<AtomicBool>,
    #[cfg(unix)]
    reader: Option<std::thread::JoinHandle<()>>,
    /// Dropped after the reader stopped: echo back on.
    #[cfg(unix)]
    _terminal: TerminalSettings,
}

impl HiddenLines {
    /// Each line typed (without its line end) goes to `each` until it
    /// returns false or this is dropped. `None` if stdin is not a terminal,
    /// or not on Unix.
    pub fn start(each: impl FnMut(String) -> bool + Send + 'static) -> Option<HiddenLines> {
        #[cfg(unix)]
        return HiddenLines::on_fd(libc::STDIN_FILENO, each);
        #[cfg(not(unix))]
        {
            drop(each);
            None
        }
    }

    #[cfg(unix)]
    fn on_fd(fd: i32, mut each: impl FnMut(String) -> bool + Send + 'static) -> Option<Self> {
        let terminal = TerminalSettings::of_fd(fd);
        let mut t = terminal.saved?.1;
        // Whole lines, edited and interrupted (Ctrl-C) as usual, not shown;
        // the Enter is, so what follows starts on a line of its own.
        t.c_lflag = (t.c_lflag | libc::ICANON | libc::ISIG | libc::ECHONL) & !libc::ECHO;
        // SAFETY: settings read from the same descriptor.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &t) } != 0 {
            return None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let reader = std::thread::spawn(move || {
            let mut buf = zeroize::Zeroizing::new([0u8; 1024]);
            let mut line = zeroize::Zeroizing::new(Vec::new());
            while !stopped.load(Ordering::Relaxed) {
                let mut p = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one pollfd, ours.
                match unsafe { libc::poll(&mut p, 1, 100) } {
                    0 => continue,
                    n if n < 0 => {
                        if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                        {
                            continue;
                        }
                        return;
                    }
                    _ => {}
                }
                // SAFETY: writes at most `buf.len()` bytes into `buf`.
                let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    // The end (Ctrl-D), or the terminal is gone.
                    return;
                }
                line.extend_from_slice(&buf[..n as usize]);
                while let Some(end) = line.iter().position(|&b| b == b'\n') {
                    let text = String::from_utf8_lossy(&line[..end])
                        .trim_end_matches('\r')
                        .to_string();
                    line.drain(..=end);
                    if !each(text) {
                        return;
                    }
                }
            }
        });
        Some(HiddenLines {
            stop,
            reader: Some(reader),
            _terminal: terminal,
        })
    }
}

#[cfg(unix)]
impl Drop for HiddenLines {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
    }
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

/// How many columns a terminal gives `s`: emoji and wide (East Asian)
/// characters two, variation selectors, joiners and combining marks none —
/// an emoji sequence joined with U+200D counts once. Enough for icons and
/// names; not a full implementation of Unicode widths.
pub fn display_width(s: &str) -> usize {
    let wide = |c: char| {
        matches!(c as u32,
            0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF | 0xA000..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F000..=0x1FAFF
            | 0x20000..=0x3FFFD)
    };
    let zero = |c: char| {
        matches!(c as u32, 0x0300..=0x036F | 0x200B..=0x200F | 0x20D0..=0x20FF
            | 0xFE00..=0xFE0F | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F)
    };
    let mut w = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{200D}' {
            // Joined to the one before: skip what it joins.
            chars.next();
            continue;
        }
        if zero(c) {
            continue;
        }
        // A symbol shown as an emoji (U+FE0F after it) is wide too.
        let emoji = chars.peek() == Some(&'\u{FE0F}');
        w += if wide(c) || emoji { 2 } else { 1 };
    }
    w
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
    #[cfg(unix)]
    use super::{HiddenLines, TerminalSettings};

    /// A pseudo-terminal: (the keyboard's end, the program's).
    #[cfg(unix)]
    fn pty() -> (i32, i32) {
        let (mut master, mut slave) = (0, 0);
        // SAFETY: openpty writes two descriptors; no name, settings or size.
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        assert_eq!(rc, 0, "openpty");
        (master, slave)
    }

    #[cfg(unix)]
    fn get(fd: i32) -> libc::termios {
        let mut t = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: as in TerminalSettings::of_fd.
        assert_eq!(unsafe { libc::tcgetattr(fd, t.as_mut_ptr()) }, 0);
        unsafe { t.assume_init() }
    }

    #[cfg(unix)]
    fn echo(fd: i32) -> bool {
        get(fd).c_lflag & libc::ECHO != 0
    }

    /// A terminal whose echo a hidden prompt turned off gets it back.
    #[cfg(unix)]
    #[test]
    fn terminal_settings_are_put_back() {
        let (master, slave) = pty();
        assert!(echo(slave), "a new terminal echoes");
        {
            let _restore = TerminalSettings::of_fd(slave);
            // What a hidden prompt does.
            let mut t = get(slave);
            t.c_lflag &= !libc::ECHO;
            // SAFETY: settings read from the same descriptor.
            unsafe { libc::tcsetattr(slave, libc::TCSANOW, &t) };
            assert!(!echo(slave));
        }
        assert!(echo(slave), "echo is back");
        // Not a terminal: nothing to save or put back.
        drop(TerminalSettings::of_fd(-1));
        // SAFETY: both descriptors are ours.
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
    }

    /// Lines typed while waiting go to the reader, unseen; once it is
    /// dropped, the next line is there for the next question (a reader left
    /// blocked on the terminal took it: `setup` waited for an answer given).
    #[cfg(unix)]
    #[test]
    fn hidden_lines_stop_when_dropped() {
        use std::time::Duration;
        let (master, slave) = pty();
        let typed = |s: &str| {
            // SAFETY: writes `s` to our descriptor.
            let n = unsafe { libc::write(master, s.as_ptr().cast(), s.len()) };
            assert_eq!(n, s.len() as isize);
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let lines = HiddenLines::on_fd(slave, move |l| tx.send(l).is_ok()).expect("a terminal");
        assert!(!echo(slave), "what is typed is not shown");
        typed("AAAA-BBBB\n");
        typed("\n");
        typed("second\n");
        let next = || rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(next(), "AAAA-BBBB");
        assert_eq!(next(), "");
        assert_eq!(next(), "second");
        drop(lines);
        assert!(echo(slave), "echo is back");
        typed("~/Notes\n");
        // Disconnected at once; a reader still there would take the line.
        let after = rx.recv_timeout(Duration::from_millis(500));
        assert!(after.is_err(), "the reader is gone, got {after:?}");
        let mut p = libc::pollfd {
            fd: slave,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one pollfd, ours.
        let ready = unsafe { libc::poll(&mut p, 1, 2000) };
        assert_eq!(ready, 1, "the line is there, not taken by the reader");
        let mut buf = [0u8; 64];
        // SAFETY: reads at most `buf.len()` bytes into `buf`.
        let n = unsafe { libc::read(slave, buf.as_mut_ptr().cast(), buf.len()) };
        assert_eq!(&buf[..n as usize], b"~/Notes\n");
        // Not a terminal: nothing to read from.
        assert!(HiddenLines::on_fd(-1, |_| true).is_none());
        // SAFETY: both descriptors are ours.
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
    }

    #[test]
    fn display_widths() {
        use super::display_width as w;
        assert_eq!(w("Garden"), 6);
        assert_eq!(w("🌱"), 2);
        assert_eq!(w("✈️"), 2, "a symbol with the emoji selector");
        assert_eq!(w("✈"), 1);
        assert_eq!(w("👨‍👩‍👧"), 2, "a family: one emoji");
        assert_eq!(w("👋🏽"), 2, "with a skin tone");
        assert_eq!(w("日本"), 4);
        assert_eq!(w("e\u{301}"), 1, "a combining accent");
    }

    #[test]
    fn formatting() {
        assert_eq!(super::fmt_time(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::fmt_size(512), "512 B");
        assert_eq!(super::fmt_size(2048), "2.0 KiB");
    }
}
