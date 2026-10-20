//! `coolpsonotes-load`: a crowd of simulated people — real clients with real
//! keys and end-to-end encryption — against the real server image with CPU
//! and memory limits, with a live dashboard of everything that happens.
//!
//! The run: people arrive (sign up, a vault of their own), form teams
//! (shared vaults), then work: type together in shared notes, start notes,
//! upload pictures, jot quick notes, rename, share. Midway the server is
//! killed (SIGKILL) and started again; people keep writing offline and catch
//! up. At the end every copy of every shared note is compared, and every
//! sentence anyone finished must be there.

mod bus;
mod check;
mod dashboard;
mod metrics;
mod people;
mod server;
mod world;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use clap::Parser;
use coolpsonotes_load_events::{Cursor, Event, Hello, Phase, Report, Screen, Stats};
use futures::StreamExt;
use tokio::sync::watch;

use crate::bus::Bus;
use crate::people::{Person, Team, World};
use crate::server::Server;
use crate::world::Rng;

#[derive(Parser, Debug)]
#[command(
    about = "Load test with a live dashboard: simulated people against a real, limited server"
)]
struct Args {
    /// How many people.
    #[arg(long, default_value_t = 100)]
    users: u32,
    /// How long they work (after arriving), e.g. 90s, 3m.
    #[arg(long, default_value = "3m", value_parser = parse_duration)]
    duration: Duration,
    /// Over how long people arrive (sign up), e.g. 15s; 0 = all at once.
    #[arg(long, default_value = "15s", value_parser = parse_duration)]
    arrive: Duration,
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
    /// Don't kill the server midway.
    #[arg(long)]
    no_crash: bool,
    /// Open the dashboard in the browser.
    #[arg(long)]
    open: bool,
    /// Stop when done instead of keeping the dashboard up (for scripts).
    #[arg(long)]
    exit: bool,
    /// People's local copies and the report go here.
    #[arg(long, default_value = "target/load")]
    out: PathBuf,
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

#[tokio::main]
async fn main() -> ExitCode {
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
    // Stop cleanly (container removed) on Ctrl+C at any point.
    tokio::select! {
        r = run(args) => match r {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::from(1),
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::from(2)
            }
        },
        _ = tokio::signal::ctrl_c() => {
            eprintln!("\nstopped");
            ExitCode::from(130)
        }
    }
}

/// Peaks and averages for the report.
#[derive(Default)]
struct Peaks {
    cpu: f32,
    mem: u64,
    rps_sum: f64,
    rps_n: u32,
    rps_peak: f32,
    http_p99: f32,
}

async fn run(args: Args) -> Result<bool> {
    let seed = args.seed.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1)
    });
    let memory_bytes = if args.native {
        0
    } else {
        server::docker_memory(&args.memory).ok_or_else(|| anyhow!("--memory: use 512m, 1g, …"))?
    };
    let root = args.out.join("people");
    people::clean(&root);
    std::fs::create_dir_all(&root)?;
    let mut server = if args.native {
        Server::native(
            args.server_bin.clone(),
            args.web_dir.clone(),
            args.out.join("server-data"),
            args.server_port,
        )
    } else {
        Server::docker(&args.image, args.cpus, &args.memory, args.server_port)
    };
    if args.native {
        let _ = std::fs::remove_dir_all(args.out.join("server-data"));
    }
    let bus = Bus::new(Hello {
        seed,
        users: args.users,
        duration_s: args.duration.as_secs() as u32,
        mode: if args.native { "native" } else { "docker" }.into(),
        cpus: if args.native { 0.0 } else { args.cpus },
        memory: memory_bytes,
        started_ms: now_ms(),
        server_url: server.url.clone(),
    });
    dashboard::serve(args.dashboard, args.dashboard_dir.clone(), bus.clone()).await?;
    let view = format!("http://{}", args.dashboard).replace("127.0.0.1", "localhost");
    println!("Live view: {view}");
    if args.open {
        let _ = std::process::Command::new("xdg-open").arg(&view).spawn();
    }
    println!(
        "{} people, {} s, seed {seed}, server: {}",
        args.users,
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
    bus.send(Event::Server { up: true });

    let mut rng = Rng::new(seed);
    let teams = world::teams(&mut rng, args.users);
    let mut team_of = vec![0; args.users as usize];
    for (t, members) in teams.iter().enumerate() {
        for &p in members {
            team_of[p as usize] = t as u32;
        }
    }
    let world = Arc::new(World::new(
        bus.clone(),
        server.url.clone(),
        root,
        args.users,
        team_of,
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

    // People arrive.
    bus.phase(
        Phase::Arriving,
        format!(
            "{} people sign up: account keys, a device, a vault of their own",
            args.users
        ),
    );
    for id in 0..args.users {
        bus.send(Event::Person {
            id,
            name: world.display(id).to_string(),
            team: world.team_of[id as usize],
        });
    }
    // Spread over the arrival time, a little irregularly, as people come.
    let spacing = args.arrive.as_secs_f64() / args.users.max(1) as f64;
    let mut arrival_rng = Rng::new(seed ^ 0xA11);
    let starts: Vec<Duration> = (0..args.users)
        .map(|i| {
            let jitter = (arrival_rng.range(0, 1000) as f64 / 1000.0 - 0.5) * spacing;
            Duration::from_secs_f64((i as f64 * spacing + jitter).max(0.0))
        })
        .collect();
    let begin = Instant::now();
    let arrived: Vec<Result<Person>> = futures::stream::iter(0..args.users)
        .map(|id| {
            let at = begin + starts[id as usize];
            let world = world.clone();
            async move {
                tokio::time::sleep_until(at.into()).await;
                Person::arrive(world, id, seed).await
            }
        })
        .buffer_unordered(64)
        .collect()
        .await;
    println!(
        "{:>6.1} s  everyone arrived",
        t_start.elapsed().as_secs_f32()
    );
    let mut persons: HashMap<u32, Person> = HashMap::new();
    for p in arrived {
        let p = p?;
        persons.insert(p.id, p);
    }

    // Teams share vaults.
    bus.phase(
        Phase::Teaming,
        format!(
            "{} teams: each owner creates a vault and shares it",
            teams.len()
        ),
    );
    let mut founders = Vec::new();
    for (t, members) in teams.iter().enumerate() {
        let owner = members[0];
        let mut p = persons.remove(&owner).expect("arrived");
        let members = members.clone();
        let name = world::team_name(t as u32);
        founders.push(async move {
            let r = p.found_team(t as u32, &name, &members).await;
            (p, r)
        });
    }
    for (p, team) in futures::future::join_all(founders).await {
        let team = team?;
        bus.send(Event::Team {
            id: team.id,
            name: team.name.clone(),
            owner: team.owner,
            members: team.members.clone(),
        });
        world.teams.lock().unwrap().push(team);
        persons.insert(p.id, p);
    }
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
    println!("{:>6.1} s  teams formed", t_start.elapsed().as_secs_f32());

    // Work.
    bus.phase(
        Phase::Busy,
        "typing together, new notes, pictures, quick notes, renames, shares",
    );
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut spotlight = all_teams.clone();
    spotlight.sort_by_key(|t| std::cmp::Reverse(t.members.len()));
    spotlight.truncate(3);
    tokio::spawn(screens(world.clone(), spotlight.clone(), stop_rx.clone()));
    tokio::spawn(ciphers(world.clone(), spotlight, stop_rx.clone()));
    let workers: Vec<_> = persons
        .into_values()
        .map(|p| tokio::spawn(p.work(stop_rx.clone())))
        .collect();

    let busy_start = Instant::now();
    let mut recovery = None;
    if !args.no_crash {
        tokio::time::sleep(args.duration.mul_f32(0.5)).await;
        bus.phase(
            Phase::Crash,
            "kill -9: no warning, no shutdown — people keep writing offline",
        );
        bus.feed(
            coolpsonotes_load_events::Action::Revision,
            "💥 the server was killed",
        );
        up.store(false, Ordering::Relaxed);
        bus.send(Event::Server { up: false });
        server.crash().await?;
        tokio::time::sleep(Duration::from_secs(6)).await;
        server.restart().await?;
        up.store(true, Ordering::Relaxed);
        bus.send(Event::Server { up: true });
        let back = Instant::now();
        bus.phase(
            Phase::Recovering,
            "the server is back: everyone reconnects and uploads what they wrote meanwhile",
        );
        bus.feed(
            coolpsonotes_load_events::Action::Revision,
            "the server is back",
        );
        if check::settle(&world, Duration::from_secs(120))
            .await
            .is_some()
        {
            recovery = Some(back.elapsed().as_secs_f32());
        }
        bus.phase(Phase::Busy, "back to normal");
    }
    let left = args.duration.saturating_sub(busy_start.elapsed());
    tokio::time::sleep(left).await;
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
    println!("{:>6.1} s  checking", t_start.elapsed().as_secs_f32());
    check::settle(&world, Duration::from_secs(120)).await;
    println!("{:>6.1} s  settled", t_start.elapsed().as_secs_f32());
    let outcome = check::run(&world).await;
    println!("{:>6.1} s  compared", t_start.elapsed().as_secs_f32());
    let report = {
        let t = world.bus.tally.lock().unwrap();
        let pk = peaks.lock().unwrap();
        let mut steady = t.steady.clone();
        steady.sort_unstable();
        Report {
            duration_s: t_start.elapsed().as_secs_f32(),
            users: args.users,
            teams: teams.len() as u32,
            vaults: t.vaults,
            notes: t.notes + teams.len() as u32 * 4,
            pictures: t.pictures,
            edits: t.edits,
            actions: t.actions_total,
            errors: t.errors,
            errors_while_down: t.errors_while_down,
            peak_cpu_pct: pk.cpu,
            peak_mem_bytes: pk.mem,
            avg_http_rps: if pk.rps_n > 0 {
                (pk.rps_sum / pk.rps_n as f64) as f32
            } else {
                0.0
            },
            peak_http_rps: pk.rps_peak,
            http_p99_ms: pk.http_p99,
            propagation_p50_ms: bus::percentile(&steady, 0.5),
            propagation_p99_ms: bus::percentile(&steady, 0.99),
            recovery_s: recovery,
            notes_checked: outcome.notes_checked,
            copies_checked: outcome.copies_checked,
            mismatches: outcome.mismatches,
            sentences: outcome.sentences,
            sentences_missing: outcome.sentences_missing,
        }
    };
    bus.report(report.clone());
    bus.phase(
        Phase::Done,
        if report.passed() {
            "every copy of every note is the same, nothing was lost"
        } else {
            "some copies differ — see the report"
        },
    );
    print_report(&report);
    let file = args.out.join(format!("report-{}.json", now_ms() / 1000));
    std::fs::write(&file, serde_json::to_vec_pretty(&report)?)?;
    println!("Report: {}", file.display());

    if !args.exit {
        println!("The live view stays up at {view} — Ctrl+C to stop.");
        tokio::signal::ctrl_c().await?;
    }
    for p in persons {
        p.leave().await;
    }
    server.remove();
    Ok(report.passed())
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

/// The spotlight notes as one of their people sees them, with the cursors
/// of whoever is typing.
async fn screens(world: Arc<World>, teams: Vec<Team>, stop: watch::Receiver<bool>) {
    const LINES: usize = 18;
    let mut last: HashMap<u8, (Vec<String>, Vec<Cursor>)> = HashMap::new();
    while !*stop.borrow() {
        for (slot, team) in teams.iter().enumerate() {
            let slot = slot as u8;
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
            if last.get(&slot) != Some(&(lines.clone(), cursors.clone())) {
                last.insert(slot, (lines.clone(), cursors.clone()));
                world.bus.send(Event::Screen(Screen {
                    slot,
                    team: team.id,
                    title: team.hot_title.clone(),
                    lines,
                    cursors,
                }));
            }
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
}

/// What the server holds for the spotlight notes' newest change: read back
/// from the server, so it's exactly what it stores.
async fn ciphers(world: Arc<World>, teams: Vec<Team>, stop: watch::Receiver<bool>) {
    let mut shown: HashMap<u8, i64> = HashMap::new();
    while !*stop.borrow() {
        for (slot, team) in teams.iter().enumerate() {
            let slot = slot as u8;
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

fn print_report(r: &Report) {
    let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
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
        "From one person's keystroke to the others' screens: p50 {:.0} ms, p99 {:.0} ms (outside the crash)",
        r.propagation_p50_ms, r.propagation_p99_ms
    );
    if let Some(s) = r.recovery_s {
        println!("After the crash, everyone was back and caught up in {s:.1} s");
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
