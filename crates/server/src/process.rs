//! The server process itself: its open-files limit, and its own CPU time
//! and memory for `/metrics` (under the names Prometheus client libraries
//! use, so dashboards — and the load test against a server elsewhere — can
//! see them; Linux only, elsewhere they are left out).

/// Every connected person holds a WebSocket and an HTTP connection or two,
/// each an open file: the usual soft limit (1024, or 2048 in a container)
/// would turn people away at a few hundred. Raise it to the hard limit
/// (at most about a million); returns the limit now, if known.
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)] // rlim_t isn't u64 everywhere
pub fn raise_open_files_limit() -> Option<u64> {
    const MOST: libc::rlim_t = 1 << 20;
    let mut l = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit and setrlimit with a valid pointer to a local struct.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut l) != 0 {
            return None;
        }
        let want = l.rlim_max.min(MOST);
        if l.rlim_cur < want {
            let raised = libc::rlimit {
                rlim_cur: want,
                rlim_max: l.rlim_max,
            };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &raised) == 0 {
                l.rlim_cur = want;
            }
        }
    }
    Some(l.rlim_cur as u64)
}

#[cfg(not(unix))]
pub fn raise_open_files_limit() -> Option<u64> {
    None
}

/// Set `process_cpu_seconds_total` and `process_resident_memory_bytes`.
pub fn record() {
    #[cfg(target_os = "linux")]
    if let (Ok(stat), Ok(status)) = (
        std::fs::read_to_string("/proc/self/stat"),
        std::fs::read_to_string("/proc/self/status"),
    ) {
        if let Some(cpu) = cpu_seconds(&stat) {
            // A gauge: the exporter's counters only count whole numbers.
            metrics::gauge!("process_cpu_seconds_total").set(cpu);
        }
        if let Some(rss) = resident_bytes(&status) {
            metrics::gauge!("process_resident_memory_bytes").set(rss as f64);
        }
    }
}

/// User and system CPU time from `/proc/self/stat`, in seconds.
fn cpu_seconds(stat: &str) -> Option<f64> {
    // The fields after the command name, which is in parentheses and may
    // contain spaces; utime and stime are the 12th and 13th of them, in
    // clock ticks (USER_HZ: 100 on Linux for every common architecture).
    const TICKS_PER_SECOND: f64 = 100.0;
    let rest = stat.get(stat.rfind(')')? + 2..)?;
    let mut f = rest.split_whitespace().skip(11);
    let utime: f64 = f.next()?.parse().ok()?;
    let stime: f64 = f.next()?.parse().ok()?;
    Some((utime + stime) / TICKS_PER_SECOND)
}

/// The resident set size from `/proc/self/status`, in bytes.
fn resident_bytes(status: &str) -> Option<u64> {
    let kb: u64 = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some(kb * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_cpu_time_and_memory() {
        let stat = "4242 (notekpr (x)) S 1 4242 4242 0 -1 4194560 12 0 0 0 250 125 0 0 20 0 9 0";
        assert_eq!(cpu_seconds(stat), Some(3.75));
        assert_eq!(cpu_seconds("garbage"), None);
        let status = "Name:\tnotekpr\nVmPeak:\t  90000 kB\nVmRSS:\t   41000 kB\nThreads:\t9\n";
        assert_eq!(resident_bytes(status), Some(41_000 * 1024));
    }

    #[cfg(unix)]
    #[test]
    #[allow(clippy::unnecessary_cast)]
    fn the_open_files_limit_goes_up_to_the_hard_one() {
        let limit = raise_open_files_limit().unwrap();
        let mut l = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: as above.
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut l) }, 0);
        assert_eq!(l.rlim_cur as u64, limit);
        assert_eq!(limit, (l.rlim_max as u64).min(1 << 20));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reads_this_process() {
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        assert!(cpu_seconds(&stat).is_some());
        assert!(resident_bytes(&status).is_some_and(|b| b > 0));
    }
}
