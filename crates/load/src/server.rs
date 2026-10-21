//! The server under load: the real image in a Docker container with CPU and
//! memory limits (or, with `--native`, a plain process without limits, or
//! with `--server`, one that runs elsewhere), and how much CPU and memory it
//! uses.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Flags for a server that thousands of people reach from one address.
fn load_flags(public_url: &str) -> Vec<String> {
    [
        "serve",
        "--dev-mode",
        "--public-url",
        public_url,
        // Everyone comes from this machine: per-address limits would
        // throttle the whole crowd as if it were one person.
        "--rate-limit-rps",
        "1000000",
        "--auth-rate-limit-per-min",
        "1000000",
        "--log-format",
        "json",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

pub enum Kind {
    Docker {
        name: String,
        image: String,
        cpus: f32,
        memory: String,
    },
    Native {
        bin: PathBuf,
        web: PathBuf,
        data: PathBuf,
        log: PathBuf,
        child: Option<Child>,
    },
    /// Elsewhere: started, limited and removed by whoever runs it; killed
    /// and started again for outages with these commands, if given.
    Remote {
        kill: Option<String>,
        start: Option<String>,
    },
}

/// The latest CPU and memory sample.
#[derive(Clone, Copy, Default, Debug)]
pub struct Usage {
    pub cpu_pct: f32,
    pub mem_bytes: u64,
}

pub struct Server {
    pub kind: Kind,
    pub port: u16,
    pub url: String,
    usage: Arc<Mutex<Usage>>,
    sampler: Option<tokio::task::JoinHandle<()>>,
}

impl Server {
    pub fn docker(image: &str, cpus: f32, memory: &str, port: u16) -> Server {
        Server::new(
            Kind::Docker {
                name: format!("notekpr-load-{}", std::process::id()),
                image: image.to_string(),
                cpus,
                memory: memory.to_string(),
            },
            port,
        )
    }

    pub fn native(bin: PathBuf, web: PathBuf, data: PathBuf, port: u16) -> Server {
        let log = data.with_extension("log");
        Server::new(
            Kind::Native {
                bin,
                web,
                data,
                log,
                child: None,
            },
            port,
        )
    }

    pub fn remote(url: &str, kill: Option<String>, start: Option<String>) -> Server {
        let mut s = Server::new(Kind::Remote { kill, start }, 0);
        s.url = url.trim_end_matches('/').to_string();
        s
    }

    fn new(kind: Kind, port: u16) -> Server {
        Server {
            kind,
            port,
            url: format!("http://localhost:{port}"),
            usage: Arc::default(),
            sampler: None,
        }
    }

    pub fn mode(&self) -> &'static str {
        match self.kind {
            Kind::Docker { .. } => "docker",
            Kind::Native { .. } => "native",
            Kind::Remote { .. } => "remote",
        }
    }

    /// Whether it runs with this run's CPU and memory limits.
    pub fn limited(&self) -> bool {
        matches!(self.kind, Kind::Docker { .. })
    }

    /// Whether its CPU and memory are measured from here (otherwise: from
    /// its own metrics).
    pub fn measures_usage(&self) -> bool {
        !matches!(self.kind, Kind::Remote { .. })
    }

    /// Where the latest CPU and memory sample is kept (across restarts).
    pub fn usage_handle(&self) -> Arc<Mutex<Usage>> {
        self.usage.clone()
    }

    /// Start it the first time.
    pub async fn start(&mut self) -> Result<()> {
        let flags = load_flags(&self.url);
        match &mut self.kind {
            Kind::Docker {
                name,
                image,
                cpus,
                memory,
            } => {
                let ok = Command::new("docker")
                    .args(["image", "inspect", image.as_str()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await
                    .context("running docker (is it installed and running?)")?;
                if !ok.success() {
                    bail!("there is no image {image}: build it with `task docker:build`");
                }
                let out = Command::new("docker")
                    .args(["run", "-d", "--name", name.as_str()])
                    .args(["--cpus", &cpus.to_string()])
                    .args([
                        "--memory",
                        memory.as_str(),
                        "--memory-swap",
                        memory.as_str(),
                    ])
                    .args(["-p", &format!("127.0.0.1:{}:8080", self.port)])
                    // Its data in an anonymous volume, removed with it.
                    .args(["-v", "/data"])
                    // Development mode inside a container listens on all of
                    // its interfaces; the port is published on loopback only.
                    .args(["-e", "NOTEKPR_DEV_MODE_ANYWHERE=true"])
                    .arg(image.as_str())
                    .args(&flags)
                    .output()
                    .await?;
                if !out.status.success() {
                    bail!(
                        "docker run failed: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                }
            }
            Kind::Native { .. } => self.spawn_native()?,
            Kind::Remote { .. } => {
                return self
                    .wait_healthy(Duration::from_secs(20))
                    .await
                    .with_context(|| format!("reaching {}", self.url));
            }
        }
        self.wait_healthy(Duration::from_secs(60)).await?;
        self.start_sampler();
        Ok(())
    }

    fn spawn_native(&mut self) -> Result<()> {
        let flags = load_flags(&self.url);
        let Kind::Native {
            bin,
            web,
            data,
            log,
            child,
        } = &mut self.kind
        else {
            unreachable!()
        };
        std::fs::create_dir_all(&*data)?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&*log)?;
        let c = Command::new(&*bin)
            .args(&flags)
            .arg("--web-dir")
            .arg(&*web)
            .arg("--data-dir")
            .arg(&*data)
            .args(["--listen", &format!("127.0.0.1:{}", self.port)])
            .stdout(log.try_clone()?)
            .stderr(log)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting {}", bin.display()))?;
        *child = Some(c);
        Ok(())
    }

    /// Kill it the hard way (SIGKILL): no shutdown, no last writes.
    pub async fn crash(&mut self) -> Result<()> {
        match &mut self.kind {
            Kind::Docker { name, .. } => {
                Command::new("docker")
                    .args(["kill", name.as_str()])
                    .stdout(Stdio::null())
                    .status()
                    .await?;
            }
            Kind::Native { child, .. } => {
                if let Some(c) = child.as_mut() {
                    c.kill().await?;
                }
            }
            Kind::Remote { kill, .. } => {
                let kill = kill.as_deref().context("no --kill-command")?;
                shell(kill).await.context("--kill-command")?;
            }
        }
        *self.usage.lock().unwrap() = Usage::default();
        Ok(())
    }

    /// Start it again on the same data.
    pub async fn restart(&mut self) -> Result<()> {
        match &mut self.kind {
            Kind::Docker { name, .. } => {
                let st = Command::new("docker")
                    .args(["start", name.as_str()])
                    .stdout(Stdio::null())
                    .status()
                    .await?;
                if !st.success() {
                    bail!("docker start failed");
                }
            }
            Kind::Native { .. } => self.spawn_native()?,
            Kind::Remote { start, .. } => {
                let start = start.as_deref().context("no --start-command")?;
                shell(start).await.context("--start-command")?;
            }
        }
        self.wait_healthy(Duration::from_secs(60)).await?;
        self.start_sampler();
        Ok(())
    }

    /// For the check after a game over: raise the container's limits (to 4
    /// CPUs and 4 GB, or four times what it had if that is more), so it can
    /// take everyone's uploads. False if it has no limits of ours.
    pub async fn lift_limits(&mut self) -> Result<bool> {
        let Kind::Docker {
            name, cpus, memory, ..
        } = &self.kind
        else {
            return Ok(false);
        };
        let mem = docker_memory(memory).unwrap_or(0).max(1 << 30) * 4;
        let mem = format!("{}m", mem >> 20);
        let cpus = (*cpus * 4.0).max(4.0).to_string();
        let st = Command::new("docker")
            .args([
                "update",
                "--cpus",
                &cpus,
                "--memory",
                &mem,
                "--memory-swap",
                &mem,
            ])
            .arg(name.as_str())
            .stdout(Stdio::null())
            .status()
            .await?;
        if !st.success() {
            bail!("docker update failed");
        }
        Ok(true)
    }

    /// Remove it (container and data).
    pub fn remove(&mut self) {
        if let Some(s) = self.sampler.take() {
            s.abort();
        }
        match &mut self.kind {
            Kind::Docker { name, .. } => {
                let _ = std::process::Command::new("docker")
                    .args(["rm", "-f", "-v", name.as_str()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            Kind::Native { child, .. } => {
                if let Some(mut c) = child.take() {
                    let _ = c.start_kill();
                }
            }
            // Not ours to remove.
            Kind::Remote { .. } => {}
        }
    }

    /// Whether it stopped by itself (not crashed by us), and why.
    pub async fn died(&mut self) -> Option<String> {
        match &mut self.kind {
            Kind::Docker { name, .. } => {
                let out = Command::new("docker")
                    .args([
                        "inspect",
                        "-f",
                        "{{.State.Running}} {{.State.OOMKilled}} {{.State.ExitCode}}",
                        name.as_str(),
                    ])
                    .output()
                    .await
                    .ok()?;
                let state = String::from_utf8_lossy(&out.stdout);
                let mut f = state.split_whitespace();
                let (running, oom, code) = (f.next()?, f.next()?, f.next().unwrap_or("?"));
                match (running, oom) {
                    ("true", _) => None,
                    (_, "true") => Some("Docker killed it: it ran out of memory".into()),
                    _ => Some(format!("it stopped by itself (exit code {code})")),
                }
            }
            Kind::Native { child, .. } => match child.as_mut()?.try_wait() {
                Ok(Some(status)) => Some(format!("it stopped by itself ({status})")),
                _ => None,
            },
            // Only that it stops answering shows.
            Kind::Remote { .. } => None,
        }
    }

    pub async fn is_healthy(&self) -> bool {
        let url = format!("{}/healthz", self.url);
        matches!(
            reqwest::Client::new()
                .get(url)
                .timeout(Duration::from_secs(2))
                .send()
                .await,
            Ok(r) if r.status().is_success()
        )
    }

    async fn wait_healthy(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.is_healthy().await {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        bail!("the server didn't come up within {} s", timeout.as_secs())
    }

    fn start_sampler(&mut self) {
        if let Some(s) = self.sampler.take() {
            s.abort();
        }
        let usage = self.usage.clone();
        self.sampler = Some(match &self.kind {
            Kind::Docker { name, .. } => tokio::spawn(docker_stats(name.clone(), usage)),
            Kind::Native { child, .. } => {
                let pid = child.as_ref().and_then(|c| c.id()).unwrap_or(0);
                tokio::spawn(proc_stats(pid, usage))
            }
            Kind::Remote { .. } => return,
        });
    }
}

/// Run a command line with `sh -c`; an error if it fails.
async fn shell(cmd: &str) -> Result<()> {
    let out = Command::new("sh")
        .args(["-c", cmd])
        .stdin(Stdio::null())
        .output()
        .await?;
    if !out.status.success() {
        bail!(
            "`{cmd}` failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

impl Drop for Server {
    fn drop(&mut self) {
        self.remove();
    }
}

/// Follow `docker stats` (one JSON line per second).
async fn docker_stats(name: String, usage: Arc<Mutex<Usage>>) {
    let Ok(mut child) = Command::new("docker")
        .args(["stats", "--format", "{{json .}}", &name])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    else {
        return;
    };
    let Some(out) = child.stdout.take() else {
        return;
    };
    let mut lines = BufReader::new(out).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        // Lines come wrapped in terminal control codes.
        let (Some(a), Some(b)) = (line.find('{'), line.rfind('}')) else {
            continue;
        };
        let json = &line[a..=b];
        let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
            continue;
        };
        let cpu = v["CPUPerc"]
            .as_str()
            .and_then(|s| s.trim_end_matches('%').parse::<f32>().ok());
        let mem = v["MemUsage"]
            .as_str()
            .and_then(|s| s.split('/').next())
            .and_then(parse_size);
        if let (Some(cpu_pct), Some(mem_bytes)) = (cpu, mem) {
            *usage.lock().unwrap() = Usage { cpu_pct, mem_bytes };
        }
    }
}

/// CPU and memory of a local process, from /proc (Linux).
async fn proc_stats(pid: u32, usage: Arc<Mutex<Usage>>) {
    let tick = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
    let read = || -> Option<(f64, u64)> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // Fields after the command name, which is in parentheses.
        let rest = &stat[stat.rfind(')')? + 2..];
        let f: Vec<&str> = rest.split_whitespace().collect();
        let cpu = (f.get(11)?.parse::<f64>().ok()? + f.get(12)?.parse::<f64>().ok()?) / tick;
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        let rss_kb = status
            .lines()
            .find(|l| l.starts_with("VmRSS:"))?
            .split_whitespace()
            .nth(1)?
            .parse::<u64>()
            .ok()?;
        Some((cpu, rss_kb * 1024))
    };
    let mut last = read().map(|(c, _)| (c, Instant::now()));
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let Some((cpu, mem)) = read() else { return };
        let now = Instant::now();
        if let Some((c0, t0)) = last {
            let pct = (cpu - c0) / now.duration_since(t0).as_secs_f64() * 100.0;
            *usage.lock().unwrap() = Usage {
                cpu_pct: pct as f32,
                mem_bytes: mem,
            };
        }
        last = Some((cpu, now));
    }
}

/// "123.4MiB", "1.2GiB", "512kB" → bytes.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let i = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let n: f64 = s[..i].trim().parse().ok()?;
    let unit = match s[i..].trim() {
        "" | "B" | "b" => 1.0,
        "k" | "K" | "kB" | "KB" => 1e3,
        "KiB" => 1024.0,
        "m" | "M" | "MB" => 1e6,
        "MiB" => 1024.0 * 1024.0,
        "g" | "G" | "GB" => 1e9,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((n * unit) as u64)
}

/// A Docker memory size ("512m", "1g") → bytes.
pub fn docker_memory(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    let (n, mul) = match s.chars().last()? {
        'k' => (&s[..s.len() - 1], 1024u64),
        'm' => (&s[..s.len() - 1], 1024 * 1024),
        'g' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        'b' => (&s[..s.len() - 1], 1),
        _ => (&s[..], 1),
    };
    Some((n.parse::<f64>().ok()? * mul as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("123.5MiB"), Some(129_499_136));
        assert_eq!(parse_size("1GiB "), Some(1 << 30));
        assert_eq!(parse_size("512kB"), Some(512_000));
        assert_eq!(parse_size("0B"), Some(0));
        assert_eq!(docker_memory("512m"), Some(512 << 20));
        assert_eq!(docker_memory("2g"), Some(2 << 30));
    }
}
