//! `coolpsonotes-load`: a crowd of simulated people — real clients with real
//! keys and end-to-end encryption — against the real server image with CPU
//! and memory limits, with a live dashboard of everything that happens.
//!
//! A run has one or more levels. In each, more people arrive (sign up, a
//! vault of their own) and form teams (shared vaults), and everyone works:
//! types together in shared notes, starts notes, uploads pictures, jots
//! quick notes, renames, shares. Midway through each level the server is
//! killed (SIGKILL) for a while — longer each level — and started again;
//! people keep writing offline and catch up. At the end every copy of every
//! shared note is compared, and every sentence anyone finished must be there.

mod bus;
mod check;
mod dashboard;
mod metrics;
mod people;
mod server;
mod world;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use clap::Parser;
use coolpsonotes_load_events::{
    Action, Command, Cursor, Event, GameOver, Hello, LevelResult, Menu, Phase, Report, Screen,
    Settings, Stats, outage_s,
};
use futures::StreamExt;
use tokio::sync::{mpsc, watch};

use crate::bus::Bus;
use crate::people::{Person, World};
use crate::server::Server;
use crate::world::Rng;

#[derive(Parser, Debug, Clone)]
#[command(
    about = "Load test with a live dashboard: simulated people against a real, limited server"
)]
struct Args {
    /// How many people (in the first level).
    #[arg(long, default_value_t = 100)]
    users: u32,
    /// How many levels; each adds --step more people.
    #[arg(long, default_value_t = 1)]
    levels: u32,
    /// People added per level.
    #[arg(long, default_value_t = 100)]
    step: u32,
    /// Start right at this level: everyone up to it arrives at once (a
    /// warm-up), then that level's outage, and on from there.
    #[arg(long, default_value_t = 1)]
    from_level: u32,
    /// How long everyone works in each level (besides arriving and the
    /// outage), e.g. 60s, 2m.
    #[arg(long, default_value = "60s", value_parser = parse_duration)]
    duration: Duration,
    /// Over how long each level's newcomers arrive, e.g. 10s; 0 = at once.
    #[arg(long, default_value = "10s", value_parser = parse_duration)]
    arrive: Duration,
    /// How long the server stays down in each level's crash, e.g.
    /// "2s,5s,10s" (the last repeats). Default: 6 s for one level, from 2 s
    /// up to 90 s over several.
    #[arg(long, value_delimiter = ',', value_parser = parse_duration)]
    downtimes: Vec<Duration>,
    /// How fast people are: 2 = twice as fast as the defaults.
    #[arg(long, default_value_t = 1.0)]
    speed: f64,
    /// The server container's CPU limit (cores).
    #[arg(long, default_value_t = 1.0)]
    cpus: f32,
    /// The server container's memory limit (Docker syntax: 512m, 1g).
    #[arg(long, default_value = "512m")]
    memory: String,
    /// The server image (`task docker:build`).
    #[arg(long, default_value = "coolpsonotes:latest")]
    image: String,
    /// Run the server as a local process instead (no limits; Linux).
    #[arg(long)]
    native: bool,
    #[arg(long, default_value = "target/release/coolpsonotes-server")]
    server_bin: PathBuf,
    #[arg(long, default_value = "dist/web")]
    web_dir: PathBuf,
    /// The server's port on this machine.
    #[arg(long, default_value_t = 18_099)]
    server_port: u16,
    /// Where the live dashboard is served.
    #[arg(long, default_value = "127.0.0.1:8098")]
    dashboard: SocketAddr,
    #[arg(long, default_value = "dist/load")]
    dashboard_dir: PathBuf,
    /// Repeat a run: the same people, teams and choices.
    #[arg(long)]
    seed: Option<u64>,
    /// Don't crash the server.
    #[arg(long)]
    no_crash: bool,
    /// Open the dashboard in the browser.
    #[arg(long)]
    open: bool,
    /// Stop when done instead of keeping the dashboard up (for scripts;
    /// implies --now).
    #[arg(long)]
    exit: bool,
    /// Start right away with these settings, no start screen.
    #[arg(long)]
    now: bool,
    /// Go on with the next levels even after the server couldn't take one
    /// (otherwise: game over).
    #[arg(long)]
    keep_going: bool,
    /// Reports (and, with --native, the server's data) go here.
    #[arg(long, default_value = "target/load")]
    out: PathBuf,
    /// People's local copies (removed afterwards). Default: in memory
    /// (/dev/shm) where there is one — a big run writes a lot.
    #[arg(long)]
    people_dir: Option<PathBuf>,
}

fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = n.parse().map_err(|_| format!("not a duration: {s}"))?;
    Ok(Duration::from_secs(match unit {
        "" | "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        _ => return Err(format!("not a duration: {s} (use 90s, 3m)")),
    }))
}

/// Each level's outage: as given (the last repeating), or by default 6 s
/// for a single level and from 2 s growing to 90 s over several.
fn downtimes(given: &[Duration], levels: u32) -> Vec<Duration> {
    let given: Vec<u32> = given.iter().map(|d| d.as_secs() as u32).collect();
    (0..levels as usize)
        .map(|i| Duration::from_secs(outage_s(i, levels, &given) as u64))
        .collect()
}

/// The settings a run started with these flags would use.
fn settings(a: &Args) -> Settings {
    Settings {
        users: a.users,
        step: a.step,
        levels: a.levels.max(1),
        from_level: a.from_level,
        duration_s: a.duration.as_secs() as u32,
        cpus: a.cpus,
        memory: a.memory.clone(),
        speed: a.speed,
        outages: !a.no_crash,
        keep_going: a.keep_going,
    }
}

impl Args {
    /// These flags with what was chosen on the start screen (kept sane).
    fn with(&self, s: &Settings) -> Args {
        let mut a = self.clone();
        a.users = s.users.clamp(1, 100_000);
        a.step = s.step.min(100_000);
        a.levels = s.levels.clamp(1, 100);
        a.from_level = s.from_level.clamp(1, a.levels);
        a.duration = Duration::from_secs(s.duration_s.clamp(5, 3600) as u64);
        a.cpus = s.cpus.clamp(0.1, 256.0);
        if server::docker_memory(&s.memory).is_some() {
            a.memory = s.memory.clone();
        }
        a.speed = s.speed.clamp(0.05, 20.0);
        a.no_crash = !s.outages;
        a.keep_going = s.keep_going;
        a
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("LOAD_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("error")),
        )
        .with_writer(std::io::stderr)
        .init();
    coolpsonotes_client::ui::set_silent(true);
    coolpsonotes_client::ui::set_non_interactive(true);
    raise_open_files();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    let code = rt.block_on(session(args));
    rt.shutdown_timeout(Duration::from_secs(3));
    code
}

/// The dashboard, the start screen and the runs chosen on it, one after
/// the other (or one run right away with --now).
async fn session(args: Args) -> ExitCode {
    let bus = Bus::new();
    let (commands, mut asked) = mpsc::unbounded_channel();
    if let Err(e) = dashboard::serve(
        args.dashboard,
        args.dashboard_dir.clone(),
        bus.clone(),
        commands,
    )
    .await
    {
        eprintln!("error: {e:#}");
        return ExitCode::from(2);
    }
    let view = format!("http://{}", args.dashboard).replace("127.0.0.1", "localhost");
    println!("Live view: {view}");
    if args.open {
        let _ = std::process::Command::new("xdg-open").arg(&view).spawn();
    }
    clean_stale_runs();
    let right_away = args.now || args.exit;
    let mut chosen = settings(&args);
    let mut error = None;
    loop {
        if !right_away {
            bus.menu(Menu {
                settings: chosen.clone(),
                docker: !args.native,
                downtimes: args.downtimes.iter().map(|d| d.as_secs() as u32).collect(),
                error: error.take(),
            });
            println!("Choose the run on the start screen ({view}); Ctrl+C to stop.");
            loop {
                tokio::select! {
                    c = asked.recv() => match c {
                        Some(Command::Start(s)) => {
                            chosen = s;
                            break;
                        }
                        Some(Command::Menu) => {}
                        None => return ExitCode::from(2),
                    },
                    _ = tokio::signal::ctrl_c() => return ExitCode::from(130),
                }
            }
        }
        // The run, in a runtime of its own: when it ends, everything it
        // started (a thousand clients' tasks) ends with it.
        let (stop, stopped) = watch::channel(false);
        let mut game = std::pin::pin!(game(args.with(&chosen), bus.clone(), stopped));
        let outcome = tokio::select! {
            r = &mut game => r,
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\nstopping…");
                let _ = stop.send(true);
                let _ = game.await;
                return ExitCode::from(130);
            }
        };
        let code = match &outcome {
            Ok(r) if r.passed() => ExitCode::SUCCESS,
            Ok(_) => ExitCode::from(1),
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::from(2)
            }
        };
        if args.exit {
            return code;
        }
        match outcome {
            Ok(report) => {
                // Next time: try again where it gave up.
                if let Some(g) = &report.game_over {
                    chosen.from_level = g.level;
                }
                if right_away {
                    println!("The live view stays up at {view} — Ctrl+C to stop.");
                    let _ = tokio::signal::ctrl_c().await;
                    return code;
                }
                println!("Insert coin on the live view for another run; Ctrl+C to stop.");
                loop {
                    tokio::select! {
                        c = asked.recv() => match c {
                            Some(Command::Menu) => break,
                            Some(Command::Start(_)) => {}
                            None => return code,
                        },
                        _ = tokio::signal::ctrl_c() => return code,
                    }
                }
            }
            Err(e) if !right_away => error = Some(format!("{e:#}")),
            Err(_) => return code,
        }
    }
}

/// One run in a runtime of its own, removing people's copies afterwards.
async fn game(args: Args, bus: Bus, mut stop: watch::Receiver<bool>) -> Result<Report> {
    let (done, result) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let root = people_dir(args.people_dir.as_deref(), &args.out);
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let _ = done.send(Err(anyhow!(e)));
                return;
            }
        };
        let r = rt.block_on(async {
            tokio::select! {
                r = run(args, root.clone(), bus) => r,
                _ = stop.wait_for(|s| *s) => Err(anyhow!("stopped")),
            }
        });
        // Stop every client first (their live loops save into `root`), then
        // remove their copies.
        rt.shutdown_timeout(Duration::from_secs(3));
        let _ = std::fs::remove_dir_all(&root);
        let _ = done.send(r);
    });
    result
        .await
        .map_err(|_| anyhow!("the run ended unexpectedly"))?
}

/// Every person holds a WebSocket and HTTP connections: a thousand of them
/// need more than the usual 1024 open files.
fn raise_open_files() {
    let mut l = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: plain calls with a valid pointer to a local struct.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut l) == 0 && l.rlim_cur < l.rlim_max {
            l.rlim_cur = l.rlim_max;
            libc::setrlimit(libc::RLIMIT_NOFILE, &l);
        }
    }
}

/// What runs that were killed outright left behind: their containers and
/// people's copies (named after the runner's process id), once that
/// process is gone. Linux (it looks in /proc).
fn clean_stale_runs() {
    let gone = |name: &str| -> bool {
        name.strip_prefix("coolpsonotes-load-")
            .and_then(|pid| pid.parse::<u32>().ok())
            .is_some_and(|pid| !Path::new(&format!("/proc/{pid}")).exists())
    };
    if !Path::new("/proc/self").exists() {
        return;
    }
    if let Ok(out) = std::process::Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            "name=coolpsonotes-load-",
            "--format",
            "{{.Names}}",
        ])
        .output()
    {
        for name in String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|n| gone(n))
        {
            let _ = std::process::Command::new("docker")
                .args(["rm", "-f", "-v", name])
                .output();
        }
    }
    if let Ok(dir) = std::fs::read_dir("/dev/shm") {
        for e in dir.flatten() {
            if gone(&e.file_name().to_string_lossy()) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
}

fn people_dir(given: Option<&Path>, out: &Path) -> PathBuf {
    if let Some(p) = given {
        return p.to_path_buf();
    }
    let shm = Path::new("/dev/shm");
    if shm.is_dir() {
        shm.join(format!("coolpsonotes-load-{}", std::process::id()))
    } else {
        out.join("people")
    }
}

/// Peaks and averages (of a level).
#[derive(Default, Clone)]
struct Peaks {
    cpu: f32,
    mem: u64,
    rps_sum: f64,
    rps_n: u32,
    rps_peak: f32,
    http_p99: f32,
    queued: u32,
}

impl Peaks {
    fn avg_rps(&self) -> f32 {
        if self.rps_n > 0 {
            (self.rps_sum / self.rps_n as f64) as f32
        } else {
            0.0
        }
    }
}

async fn run(args: Args, root: PathBuf, bus: Bus) -> Result<Report> {
    let seed = args.seed.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1)
    });
    let levels = args.levels.max(1);
    let total = args.users + args.step * (levels - 1);
    let downs = downtimes(&args.downtimes, levels);
    let memory_bytes = if args.native {
        0
    } else {
        server::docker_memory(&args.memory).ok_or_else(|| anyhow!("--memory: use 512m, 1g, …"))?
    };
    people::clean(&root);
    std::fs::create_dir_all(&root)?;
    std::fs::create_dir_all(&args.out)?;
    let mut server = if args.native {
        let _ = std::fs::remove_dir_all(args.out.join("server-data"));
        Server::native(
            args.server_bin.clone(),
            args.web_dir.clone(),
            args.out.join("server-data"),
            args.server_port,
        )
    } else {
        Server::docker(&args.image, args.cpus, &args.memory, args.server_port)
    };
    bus.start_run(Hello {
        seed,
        users: total,
        duration_s: args.duration.as_secs() as u32,
        mode: if args.native { "native" } else { "docker" }.into(),
        cpus: if args.native { 0.0 } else { args.cpus },
        memory: memory_bytes,
        started_ms: now_ms(),
        server_url: server.url.clone(),
        levels,
        replay: !(args.now || args.exit),
    });
    println!(
        "{} people{}, {} s each level, seed {seed}, server: {}",
        total,
        if levels > 1 {
            format!(
                " in {levels} levels ({} first, then {} more each)",
                args.users, args.step
            )
        } else {
            String::new()
        },
        args.duration.as_secs(),
        if args.native {
            "local process (no limits)".to_string()
        } else {
            format!(
                "{} with {} CPU and {} memory",
                args.image, args.cpus, args.memory
            )
        }
    );

    bus.phase(
        Phase::Starting,
        "the real server image, limited like a small machine",
    );
    server.start().await?;
    bus.send(Event::Server {
        up: true,
        down_for_s: None,
    });

    // Each level's newcomers form their own teams.
    let mut rng = Rng::new(seed);
    let mut team_of = vec![0; total as usize];
    let mut cohorts: Vec<(std::ops::Range<u32>, Vec<Vec<u32>>)> = Vec::new();
    let mut next_team = 0u32;
    for level in 0..levels {
        let start = if level == 0 {
            0
        } else {
            args.users + args.step * (level - 1)
        };
        let end = args.users + args.step * level;
        let teams: Vec<Vec<u32>> = world::teams(&mut rng, end - start)
            .into_iter()
            .map(|m| m.into_iter().map(|p| p + start).collect())
            .collect();
        for members in &teams {
            for &p in members {
                team_of[p as usize] = next_team;
            }
            next_team += 1;
        }
        cohorts.push((start..end, teams));
    }
    let world = Arc::new(World::new(
        bus.clone(),
        server.url.clone(),
        root,
        total,
        team_of,
        args.speed,
    ));
    let up = Arc::new(AtomicBool::new(true));
    let peaks = Arc::new(Mutex::new(Peaks::default()));
    let t_start = Instant::now();
    tokio::spawn(stats_loop(
        world.clone(),
        server.usage_handle(),
        server.url.clone(),
        up.clone(),
        peaks.clone(),
        t_start,
    ));

    // Everyone's seat from the start: the crowd fills in level by level.
    for id in 0..total {
        bus.send(Event::Person {
            id,
            name: world.display(id).to_string(),
            team: world.team_of[id as usize],
        });
    }
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut workers = Vec::new();
    let mut results: Vec<LevelResult> = Vec::new();
    let mut first_team = 0u32;
    let mut overall = Peaks::default();
    let mut game_over: Option<GameOver> = None;
    // Starting at a later level: its people and all before it come at once.
    let first = (args.from_level.clamp(1, levels) - 1) as usize;
    if first > 0 {
        let skipped: Vec<_> = cohorts.drain(..first).collect();
        let (last, _) = &cohorts[0];
        let range = 0..last.end;
        let teams = skipped
            .into_iter()
            .chain(std::iter::once(cohorts.remove(0)))
            .flat_map(|(_, t)| t)
            .collect();
        cohorts.insert(0, (range, teams));
    }
    for (i, (range, teams)) in cohorts.iter().enumerate() {
        let level = first + i;
        let n = level as u32 + 1;
        let down = (!args.no_crash).then(|| downs[level]);
        let of = |s: &str| {
            if levels > 1 {
                format!("level {n}/{levels}: {s}")
            } else {
                s.to_string()
            }
        };
        bus.send(Event::Level {
            level: n,
            of: levels,
            users: range.end,
            downtime_s: down.map(|d| d.as_secs() as u32).unwrap_or(0),
        });
        let level_start = Instant::now();
        let (actions0, edits0, errors0) = {
            let mut t = world.bus.tally.lock().unwrap();
            t.steady = bus::Reservoir::default();
            (t.actions_total, t.edits, t.errors)
        };
        *peaks.lock().unwrap() = Peaks::default();

        // People arrive, spread over the arrival time, a little irregularly.
        bus.phase(
            Phase::Arriving,
            of(&format!(
                "{} {}people sign up: account keys, a device, a vault of their own ({} in all)",
                range.len(),
                if i == 0 { "" } else { "more " },
                range.end
            )),
        );
        // A warm-up of several levels' people takes as many times longer.
        let arrival = args.arrive * (range.len() as u32).div_ceil(args.step.max(1)).max(1);
        let spacing = arrival.as_secs_f64() / range.len().max(1) as f64;
        let mut arrival_rng = Rng::new(seed ^ 0xA11 ^ level as u64);
        let starts: Vec<Duration> = range
            .clone()
            .map(|id| {
                let i = (id - range.start) as f64;
                let jitter = (arrival_rng.range(0, 1000) as f64 / 1000.0 - 0.5) * spacing;
                Duration::from_secs_f64((i * spacing + jitter).max(0.0))
            })
            .collect();
        let begin = Instant::now();
        let arrived: Vec<Option<Person>> = futures::stream::iter(range.clone())
            .map(|id| {
                let at = begin + starts[(id - range.start) as usize];
                let world = world.clone();
                async move {
                    tokio::time::sleep_until(at.into()).await;
                    arrive(world, id, seed).await
                }
            })
            .buffer_unordered(64)
            .collect()
            .await;
        let mut persons: HashMap<u32, Person> = HashMap::new();
        for p in arrived.into_iter().flatten() {
            persons.insert(p.id, p);
        }
        // Why the server couldn't take this level, if it couldn't.
        let mut lost: Option<String> = None;
        if persons.len() * 2 < range.len() {
            lost = Some(format!(
                "only {} of {} newcomers could even sign up",
                persons.len(),
                range.len()
            ));
        }
        let in_all = world.clients.lock().unwrap().iter().flatten().count();
        println!(
            "{:>7.1} s  {}",
            t_start.elapsed().as_secs_f32(),
            of(&format!(
                "{in_all} people here ({} of {} newcomers got in)",
                persons.len(),
                range.len()
            ))
        );

        // Teams share vaults.
        bus.phase(
            Phase::Teaming,
            of(&format!(
                "{} teams: each owner creates a vault and shares it",
                teams.len()
            )),
        );
        let mut founders = Vec::new();
        for (i, members) in teams.iter().enumerate() {
            let t = first_team + i as u32;
            // Those who got in; the first of them starts the team.
            let members: Vec<u32> = members
                .iter()
                .copied()
                .filter(|m| persons.contains_key(m))
                .collect();
            let Some(mut p) = members.first().and_then(|o| persons.remove(o)) else {
                continue;
            };
            let name = world::team_name(t);
            founders.push(async move {
                let r = p.found_team(t, &name, &members).await;
                (p, r)
            });
        }
        for (p, team) in futures::future::join_all(founders).await {
            match team {
                Ok(team) => {
                    bus.send(Event::Team {
                        id: team.id,
                        name: team.name.clone(),
                        owner: team.owner,
                        members: team.members.clone(),
                    });
                    world.teams.lock().unwrap().push(team);
                }
                // Too busy a server: this team works without its vault.
                Err(e) => {
                    world.bus.tally.lock().unwrap().errors += 1;
                    bus.feed(
                        Action::CreateVault,
                        format!("{} couldn't set up their team: {e:#}", world.display(p.id)),
                    );
                }
            }
            persons.insert(p.id, p);
        }
        first_team += teams.len() as u32;
        let all_teams = world.teams.lock().unwrap().clone();
        let joins = persons.values().map(|p| {
            let t = all_teams.iter().find(|t| t.id == p.team).cloned();
            async move {
                if let Some(t) = t {
                    p.join(&t.vault_id, Duration::from_secs(60)).await;
                }
            }
        });
        futures::future::join_all(joins).await;
        if i == 0 {
            // The spotlight: the biggest teams there are at the start.
            let mut biggest = all_teams.clone();
            biggest.sort_by_key(|t| std::cmp::Reverse(t.members.len()));
            let spotlight: Vec<u32> = biggest.iter().take(3).map(|t| t.id).collect();
            tokio::spawn(screens(world.clone(), spotlight.clone(), stop_rx.clone()));
            tokio::spawn(ciphers(world.clone(), spotlight, stop_rx.clone()));
        }
        workers.extend(
            persons
                .into_values()
                .map(|p| tokio::spawn(p.work(stop_rx.clone()))),
        );

        // Work, with an outage halfway.
        bus.phase(
            Phase::Busy,
            of(&format!(
                "{} people typing together, starting notes, uploading pictures, sharing",
                range.end
            )),
        );
        let (mut online, mut recovery) = (None, None);
        let first_half = if down.is_some() {
            args.duration / 2
        } else {
            args.duration
        };
        if lost.is_none() {
            lost = work_for(&mut server, first_half).await;
        }
        if let (Some(d), None) = (down, &lost) {
            let secs = d.as_secs();
            match crash(&mut server, &bus, &world, &up, d, &of).await {
                Ok((o, r)) => {
                    (online, recovery) = (o, r);
                    if o.is_none() {
                        lost = Some(format!(
                            "after {secs} s down, not everyone could reconnect within two minutes"
                        ));
                    } else if r.is_none() {
                        lost = Some(format!(
                            "after {secs} s down, it couldn't work off the backlog within two minutes"
                        ));
                    }
                }
                Err(e) => lost = Some(format!("it didn't come back after the outage: {e:#}")),
            }
            if lost.is_none() || args.keep_going {
                bus.phase(
                    Phase::Busy,
                    of(if lost.is_none() {
                        "back to normal"
                    } else {
                        "still catching up: the server can't keep up"
                    }),
                );
                if let Some(why) = work_for(&mut server, args.duration - first_half).await {
                    lost.get_or_insert(why);
                }
            }
        }

        let result = {
            let t = world.bus.tally.lock().unwrap();
            let pk = peaks.lock().unwrap().clone();
            let (p50, p99) = t.steady.percentiles();
            overall.cpu = overall.cpu.max(pk.cpu);
            overall.mem = overall.mem.max(pk.mem);
            overall.rps_sum += pk.rps_sum;
            overall.rps_n += pk.rps_n;
            overall.rps_peak = overall.rps_peak.max(pk.rps_peak);
            overall.http_p99 = overall.http_p99.max(pk.http_p99);
            LevelResult {
                level: n,
                users: range.end,
                duration_s: level_start.elapsed().as_secs_f32(),
                actions: t.actions_total - actions0,
                edits: t.edits - edits0,
                errors: t.errors - errors0,
                avg_http_rps: pk.avg_rps(),
                peak_http_rps: pk.rps_peak,
                peak_cpu_pct: pk.cpu,
                peak_mem_bytes: pk.mem,
                http_p99_ms: pk.http_p99,
                propagation_p50_ms: p50,
                propagation_p99_ms: p99,
                downtime_s: down.map(|d| d.as_secs() as u32).unwrap_or(0),
                online_s: online,
                recovery_s: recovery,
                max_queued: pk.queued,
            }
        };
        print_level(&result, t_start);
        bus.send(Event::LevelDone(result.clone()));
        results.push(result);
        if let Some(reason) = lost {
            println!(
                "{:>7.1} s  GAME OVER at level {n}: {reason}",
                t_start.elapsed().as_secs_f32()
            );
            bus.feed(Action::Revision, format!("GAME OVER: {reason}"));
            game_over.get_or_insert(GameOver {
                level: n,
                users: range.end,
                reason: reason.clone(),
            });
            bus.phase(Phase::GameOver, of(&reason));
            // The server may be gone: it has to be there for the check (and
            // for the next level, when going on regardless).
            if server.died().await.is_some() || !server.is_healthy().await {
                if let Err(e) = server.restart().await {
                    eprintln!("error: the server won't start again: {e:#}");
                }
                up.store(true, Ordering::Relaxed);
                bus.send(Event::Server {
                    up: true,
                    down_for_s: None,
                });
            }
            if !args.keep_going {
                break;
            }
        }
    }
    let _ = stop_tx.send(true);
    let mut persons = Vec::new();
    for w in workers {
        persons.push(w.await?);
    }

    // Check.
    bus.phase(
        Phase::Checking,
        "waiting for every change to reach everyone, then comparing every copy",
    );
    println!("{:>7.1} s  checking", t_start.elapsed().as_secs_f32());
    check::settle(&world, Duration::from_secs(300)).await;
    let outcome = check::run(&world).await;
    let report = {
        let t = world.bus.tally.lock().unwrap();
        let (p50, p99) = t.steady_all.percentiles();
        let teams = world.teams.lock().unwrap().len() as u32;
        Report {
            duration_s: t_start.elapsed().as_secs_f32(),
            users: total,
            teams,
            vaults: t.vaults,
            notes: t.notes + teams * 4,
            pictures: t.pictures,
            edits: t.edits,
            actions: t.actions_total,
            errors: t.errors,
            errors_while_down: t.errors_while_down,
            peak_cpu_pct: overall.cpu,
            peak_mem_bytes: overall.mem,
            avg_http_rps: overall.avg_rps(),
            peak_http_rps: overall.rps_peak,
            http_p99_ms: overall.http_p99,
            propagation_p50_ms: p50,
            propagation_p99_ms: p99,
            recovery_s: results.iter().filter_map(|r| r.recovery_s).reduce(f32::max),
            notes_checked: outcome.notes_checked,
            copies_checked: outcome.copies_checked,
            mismatches: outcome.mismatches,
            sentences: outcome.sentences,
            sentences_missing: outcome.sentences_missing,
            levels: results,
            game_over: game_over.clone(),
        }
    };
    bus.report(report.clone());
    let verdict = if report.passed() {
        "every copy of every note is the same, nothing was lost"
    } else {
        "some copies differ — see the report"
    };
    match &game_over {
        Some(g) => bus.phase(
            Phase::GameOver,
            format!(
                "level {} ({} people): {} — and yet {verdict}",
                g.level, g.users, g.reason
            ),
        ),
        None => bus.phase(Phase::Done, verdict),
    }
    print_report(&report);
    let file = args.out.join(format!("report-{}.json", now_ms() / 1000));
    std::fs::write(&file, serde_json::to_vec_pretty(&report)?)?;
    println!("Report: {}", file.display());
    futures::future::join_all(persons.into_iter().map(|p| p.leave())).await;
    server.remove();
    Ok(report)
}

/// Let everyone work for `d`, checking every few seconds that the server is
/// still alive; why not, if it died or stopped answering.
async fn work_for(server: &mut Server, d: Duration) -> Option<String> {
    const UNANSWERED: Duration = Duration::from_secs(20);
    let end = Instant::now() + d;
    let mut unanswered = Duration::ZERO;
    while Instant::now() < end {
        let step = Duration::from_secs(2).min(end - Instant::now());
        tokio::time::sleep(step).await;
        if let Some(why) = server.died().await {
            return Some(why);
        }
        if server.is_healthy().await {
            unanswered = Duration::ZERO;
        } else {
            unanswered += step;
            if unanswered >= UNANSWERED {
                return Some(format!(
                    "it stopped answering (nothing for {} s)",
                    UNANSWERED.as_secs()
                ));
            }
        }
    }
    None
}

/// Sign someone up, trying again while the server is too busy to answer;
/// `None` if they never got in (the run goes on without them).
async fn arrive(world: Arc<World>, id: u32, seed: u64) -> Option<Person> {
    let mut wait = Duration::from_secs(2);
    for attempt in 1..=5 {
        match tokio::time::timeout(
            Duration::from_secs(60),
            Person::arrive(world.clone(), id, seed),
        )
        .await
        {
            Ok(Ok(p)) => return Some(p),
            Ok(Err(e)) => tracing::warn!(
                "signing up {} (attempt {attempt}): {e:#}",
                world.display(id)
            ),
            Err(_) => tracing::warn!(
                "signing up {} (attempt {attempt}): timed out",
                world.display(id)
            ),
        }
        tokio::time::sleep(wait).await;
        wait *= 2;
    }
    world.bus.tally.lock().unwrap().errors += 1;
    world.bus.feed(
        Action::SignUp,
        format!(
            "{} couldn't sign up: the server is too busy",
            world.display(id)
        ),
    );
    None
}

/// Kill the server, keep it down for `down`, start it again and follow the
/// comeback: (everyone connected again, the backlog worked off) after the
/// restart, in seconds.
async fn crash(
    server: &mut Server,
    bus: &Bus,
    world: &World,
    up: &AtomicBool,
    down: Duration,
    of: &dyn Fn(&str) -> String,
) -> Result<(Option<f32>, Option<f32>)> {
    let secs = down.as_secs() as u32;
    // What's normally in flight at this load.
    let baseline = check::queued(world).await;
    bus.phase(
        Phase::Crash,
        of(&format!(
            "kill -9, down for {secs} s — people keep writing offline"
        )),
    );
    bus.feed(
        Action::Revision,
        format!("💥 the server was killed (down for {secs} s)"),
    );
    up.store(false, Ordering::Relaxed);
    bus.send(Event::Server {
        up: false,
        down_for_s: Some(secs),
    });
    server.crash().await?;
    tokio::time::sleep(down).await;
    server.restart().await?;
    up.store(true, Ordering::Relaxed);
    bus.send(Event::Server {
        up: true,
        down_for_s: None,
    });
    bus.phase(
        Phase::Recovering,
        of("the server is back: everyone reconnects and uploads what they wrote meanwhile"),
    );
    bus.feed(Action::Revision, "the server is back");
    let (online, caught_up) = check::recover(world, baseline, Duration::from_secs(120)).await;
    Ok((
        online.map(|d| d.as_secs_f32()),
        caught_up.map(|d| d.as_secs_f32()),
    ))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// About once a second: the server's CPU and memory, its own counters, and
/// what the crowd did.
async fn stats_loop(
    world: Arc<World>,
    usage: Arc<Mutex<server::Usage>>,
    server_url: String,
    up: Arc<AtomicBool>,
    peaks: Arc<Mutex<Peaks>>,
    t_start: Instant,
) {
    let mut rates = metrics::Rates::new(&server_url);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tick.tick().await;
        let server_up = up.load(Ordering::Relaxed);
        let m = if server_up {
            rates.sample().await
        } else {
            None
        };
        let u = *usage.lock().unwrap();
        let clients: Vec<_> = world
            .clients
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .cloned()
            .collect();
        let mut online = 0;
        let mut queued = 0;
        for (_, h) in &clients {
            online += u32::from(h.is_online());
            queued += Person::queued(h).await as u32;
        }
        let (actions_ps, delivered_ps, p50, p99, edits, notes, pictures, vaults, errors) = {
            let mut t = world.bus.tally.lock().unwrap();
            let (a, d, p50, p99) = t.window();
            (
                a, d, p50, p99, t.edits, t.notes, t.pictures, t.vaults, t.errors,
            )
        };
        let m = m.unwrap_or_default();
        {
            let mut pk = peaks.lock().unwrap();
            pk.cpu = pk.cpu.max(u.cpu_pct);
            pk.mem = pk.mem.max(u.mem_bytes);
            if m.http_rps > 0.0 {
                pk.rps_sum += m.http_rps as f64;
                pk.rps_n += 1;
            }
            pk.rps_peak = pk.rps_peak.max(m.http_rps);
            pk.http_p99 = pk.http_p99.max(m.http_p99_ms);
            pk.queued = pk.queued.max(queued);
        }
        world.bus.stats(Stats {
            t_ms: t_start.elapsed().as_millis() as u64,
            server_up,
            cpu_pct: if server_up { u.cpu_pct } else { 0.0 },
            mem_bytes: if server_up { u.mem_bytes } else { 0 },
            http_rps: m.http_rps,
            http_p50_ms: m.http_p50_ms,
            http_p99_ms: m.http_p99_ms,
            updates_ps: m.updates_ps,
            ws_connections: m.ws_connections,
            online,
            actions_ps,
            propagation_p50_ms: p50,
            propagation_p99_ms: p99,
            delivered_ps,
            errors: errors as u32,
            edits,
            notes,
            pictures,
            vaults,
            queued,
        });
    }
}

/// The spotlight teams' shared notes (their current page) as one of their
/// people sees them, with the cursors of whoever is typing.
async fn screens(world: Arc<World>, teams: Vec<u32>, stop: watch::Receiver<bool>) {
    const LINES: usize = 18;
    let mut last: HashMap<u8, (String, Vec<String>, Vec<Cursor>)> = HashMap::new();
    while !*stop.borrow() {
        for (slot, &tid) in teams.iter().enumerate() {
            let slot = slot as u8;
            let Some(team) = world.team(tid) else {
                continue;
            };
            let Some((_, hub)) = world.clients.lock().unwrap()[team.owner as usize].clone() else {
                continue;
            };
            let Some(v) = hub.get(&team.vault_id) else {
                continue;
            };
            let text = v.lock().await.note_text(&team.hot_id);
            let all: Vec<&str> = text.lines().collect();
            let first = all.len().saturating_sub(LINES);
            let lines: Vec<String> = all[first..].iter().map(|l| l.to_string()).collect();
            let typing: Vec<u32> = world
                .typing
                .lock()
                .unwrap()
                .get(&team.hot_id)
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default();
            let cursors: Vec<Cursor> = typing
                .into_iter()
                .filter_map(|p| {
                    let at = people::section_end(&text, world.display(p));
                    let (line, col) = people::line_col(&text, at);
                    (line as usize >= first).then(|| Cursor {
                        person: p,
                        line: line - first as u32,
                        col,
                    })
                })
                .collect();
            let now = (team.hot_title.clone(), lines, cursors);
            if last.get(&slot) != Some(&now) {
                last.insert(slot, now.clone());
                world.bus.send(Event::Screen(Screen {
                    slot,
                    team: team.id,
                    title: now.0,
                    lines: now.1,
                    cursors: now.2,
                }));
            }
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
}

/// What the server holds for the spotlight notes' newest change: read back
/// from the server, so it's exactly what it stores.
async fn ciphers(world: Arc<World>, teams: Vec<u32>, stop: watch::Receiver<bool>) {
    let mut shown: HashMap<u8, i64> = HashMap::new();
    while !*stop.borrow() {
        for (slot, &tid) in teams.iter().enumerate() {
            let slot = slot as u8;
            let Some(team) = world.team(tid) else {
                continue;
            };
            let Some((session, hub)) = world.clients.lock().unwrap()[team.owner as usize].clone()
            else {
                continue;
            };
            let Some(v) = hub.get(&team.vault_id) else {
                continue;
            };
            let seq = v.lock().await.replica.seq;
            let Ok(ch) = session
                .api
                .changes(&team.vault_id, (seq - 40).max(0), 100, false)
                .await
            else {
                continue;
            };
            if let Some(u) = ch.updates.iter().rev().find(|u| u.object_id == team.hot_id)
                && shown.get(&slot) != Some(&u.seq)
            {
                shown.insert(slot, u.seq);
                let head = &u.payload[..u.payload.len().min(96)];
                world.bus.send(Event::Cipher {
                    slot,
                    seq: u.seq,
                    size: u.payload.len() as u32,
                    hex: data_encoding::HEXLOWER.encode(head),
                });
            }
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
}

fn mb(b: u64) -> f64 {
    b as f64 / (1024.0 * 1024.0)
}

fn print_level(r: &LevelResult, t_start: Instant) {
    println!(
        "{:>7.1} s  level {:>2}: {:>5} people · {:>6.0} req/s avg, {:>6.0} peak · CPU {:>4.0}% · {:>5.0} MB · \
         keystroke p50 {:.0} ms p99 {:.0} ms · down {} s, online in {}, caught up in {}",
        t_start.elapsed().as_secs_f32(),
        r.level,
        r.users,
        r.avg_http_rps,
        r.peak_http_rps,
        r.peak_cpu_pct,
        mb(r.peak_mem_bytes),
        r.propagation_p50_ms,
        r.propagation_p99_ms,
        r.downtime_s,
        r.online_s
            .map(|s| format!("{s:.1} s"))
            .unwrap_or_else(|| "—".into()),
        r.recovery_s
            .map(|s| format!("{s:.1} s"))
            .unwrap_or_else(|| "—".into()),
    );
}

fn print_report(r: &Report) {
    println!();
    println!("━━ Report ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!(
        "{} people in {} teams, {:.0} s: {} actions, {} edits, {} new notes, {} pictures, {} errors{}",
        r.users,
        r.teams,
        r.duration_s,
        r.actions,
        r.edits,
        r.notes,
        r.pictures,
        r.errors,
        if r.errors_while_down > 0 {
            format!(" ({} while the server was down)", r.errors_while_down)
        } else {
            String::new()
        }
    );
    println!(
        "Server: peak {:.0}% CPU, peak {:.0} MB memory; {:.0} requests/s on average, {:.0} at the peak; p99 {:.1} ms",
        r.peak_cpu_pct,
        mb(r.peak_mem_bytes),
        r.avg_http_rps,
        r.peak_http_rps,
        r.http_p99_ms
    );
    println!(
        "From one person's keystroke to the others' copies: p50 {:.0} ms, p99 {:.0} ms (outside outages)",
        r.propagation_p50_ms, r.propagation_p99_ms
    );
    if r.levels.len() > 1 {
        println!();
        println!(
            "level  people   req/s avg   peak   CPU   memory   keystroke p50/p99   down   online in   caught up   most queued"
        );
        for l in &r.levels {
            println!(
                "{:>5}  {:>6}   {:>9.0} {:>6.0} {:>4.0}%  {:>5.0} MB   {:>7.0} / {:<7.0} ms {:>4} s   {:>9}   {:>9}   {:>11}",
                l.level,
                l.users,
                l.avg_http_rps,
                l.peak_http_rps,
                l.peak_cpu_pct,
                mb(l.peak_mem_bytes),
                l.propagation_p50_ms,
                l.propagation_p99_ms,
                l.downtime_s,
                l.online_s
                    .map(|s| format!("{s:.1} s"))
                    .unwrap_or_else(|| "—".into()),
                l.recovery_s
                    .map(|s| format!("{s:.1} s"))
                    .unwrap_or_else(|| "—".into()),
                l.max_queued,
            );
        }
        println!();
    } else if let Some(s) = r.recovery_s {
        println!("After the crash, everyone was back and caught up in {s:.1} s");
    }
    if let Some(g) = &r.game_over {
        println!(
            "GAME OVER at level {} with {} people: {}",
            g.level, g.users, g.reason
        );
    }
    println!(
        "Checked {} notes ({} copies): {} differ; {} of {} finished sentences missing",
        r.notes_checked, r.copies_checked, r.mismatches, r.sentences_missing, r.sentences
    );
    println!(
        "{}",
        if r.passed() {
            "✓ every copy is the same and nothing was lost"
        } else {
            "✗ copies differ or something was lost"
        }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outages_grow_level_by_level() {
        let secs = |v: Vec<Duration>| v.iter().map(|d| d.as_secs()).collect::<Vec<_>>();
        assert_eq!(secs(downtimes(&[], 1)), [6]);
        let ten = secs(downtimes(&[], 10));
        assert_eq!((ten[0], ten[9]), (2, 90));
        assert!(ten.windows(2).all(|w| w[0] < w[1]), "{ten:?}");
        let given = [Duration::from_secs(3), Duration::from_secs(8)];
        assert_eq!(secs(downtimes(&given, 4)), [3, 8, 8, 8]);
    }
}
