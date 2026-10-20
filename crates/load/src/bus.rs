//! Where everything that happens goes: batched for the dashboards, counted
//! for the stats, and remembered for dashboards that connect later.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use coolpsonotes_load_events::{Action, Event, Hello, Phase, Report, Stats};
use tokio::sync::broadcast;

/// How many of the latest events of a kind a late dashboard gets.
const KEEP_STATS: usize = 900;
const KEEP_FEED: usize = 40;
/// Latencies over this window make the live percentiles.
const WINDOW: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Sticky {
    hello: Option<Event>,
    phase: Option<Event>,
    people: Vec<Event>,
    teams: Vec<Event>,
    online: BTreeMap<u32, bool>,
    screens: BTreeMap<u8, Event>,
    ciphers: BTreeMap<u8, Event>,
    server: Option<Event>,
    stats: VecDeque<Event>,
    feed: VecDeque<Event>,
    report: Option<Event>,
}

/// Running totals and recent samples.
#[derive(Default)]
pub struct Tally {
    pub actions: BTreeMap<String, u64>,
    pub actions_total: u64,
    pub errors: u64,
    pub errors_while_down: u64,
    pub edits: u64,
    pub notes: u32,
    pub pictures: u32,
    pub vaults: u32,
    pub delivered_total: u64,
    recent_actions: VecDeque<Instant>,
    recent_delivered: VecDeque<(Instant, u32)>,
    /// Steady-state propagation latencies (outside the crash), for the report.
    pub steady: Vec<u32>,
    pub phase: Option<Phase>,
}

impl Tally {
    fn prune(&mut self, now: Instant) {
        while self
            .recent_actions
            .front()
            .is_some_and(|t| now.duration_since(*t) > WINDOW)
        {
            self.recent_actions.pop_front();
        }
        while self
            .recent_delivered
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > WINDOW)
        {
            self.recent_delivered.pop_front();
        }
    }

    /// Actions and deliveries per second, and propagation p50/p99 (ms), over
    /// the last few seconds.
    pub fn window(&mut self) -> (f32, f32, f32, f32) {
        let now = Instant::now();
        self.prune(now);
        let secs = WINDOW.as_secs_f32();
        let mut lat: Vec<u32> = self.recent_delivered.iter().map(|(_, l)| *l).collect();
        lat.sort_unstable();
        (
            self.recent_actions.len() as f32 / secs,
            self.recent_delivered.len() as f32 / secs,
            percentile(&lat, 0.5),
            percentile(&lat, 0.99),
        )
    }
}

pub fn percentile(sorted: &[u32], p: f64) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[i] as f32
}

#[derive(Clone)]
pub struct Bus {
    tx: broadcast::Sender<Arc<String>>,
    pending: Arc<Mutex<Vec<Event>>>,
    sticky: Arc<Mutex<Sticky>>,
    pub tally: Arc<Mutex<Tally>>,
}

impl Bus {
    pub fn new(hello: Hello) -> Bus {
        let (tx, _) = broadcast::channel(4096);
        let bus = Bus {
            tx,
            pending: Arc::default(),
            sticky: Arc::default(),
            tally: Arc::default(),
        };
        bus.send(Event::Hello(hello));
        // Ship what happened every 50 ms, as one message.
        let b = bus.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(50));
            loop {
                tick.tick().await;
                let batch = std::mem::take(&mut *b.pending.lock().unwrap());
                if !batch.is_empty()
                    && let Ok(json) = serde_json::to_string(&batch)
                {
                    let _ = b.tx.send(Arc::new(json));
                }
            }
        });
        bus
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<String>> {
        self.tx.subscribe()
    }

    /// Everything a dashboard that connects now needs, as one message.
    pub fn snapshot(&self) -> String {
        let s = self.sticky.lock().unwrap();
        let mut out: Vec<&Event> = Vec::new();
        out.extend(s.hello.iter());
        out.extend(s.people.iter());
        out.extend(s.teams.iter());
        let online: Vec<Event> = s
            .online
            .iter()
            .map(|(&id, &online)| Event::Online { id, online })
            .collect();
        out.extend(online.iter());
        out.extend(s.server.iter());
        out.extend(s.stats.iter());
        out.extend(s.feed.iter());
        out.extend(s.screens.values());
        out.extend(s.ciphers.values());
        out.extend(s.phase.iter());
        out.extend(s.report.iter());
        serde_json::to_string(&out).unwrap_or_else(|_| "[]".into())
    }

    pub fn send(&self, e: Event) {
        self.remember(&e);
        self.pending.lock().unwrap().push(e);
    }

    fn remember(&self, e: &Event) {
        let mut s = self.sticky.lock().unwrap();
        match e {
            Event::Hello(_) => s.hello = Some(e.clone()),
            Event::Phase { phase, .. } => {
                s.phase = Some(e.clone());
                self.tally.lock().unwrap().phase = Some(*phase);
            }
            Event::Person { .. } => s.people.push(e.clone()),
            Event::Team { .. } => s.teams.push(e.clone()),
            Event::Online { id, online } => {
                s.online.insert(*id, *online);
            }
            Event::Screen(sc) => {
                s.screens.insert(sc.slot, e.clone());
            }
            Event::Cipher { slot, .. } => {
                s.ciphers.insert(*slot, e.clone());
            }
            Event::Server { .. } => s.server = Some(e.clone()),
            Event::Stats(_) => {
                s.stats.push_back(e.clone());
                if s.stats.len() > KEEP_STATS {
                    s.stats.pop_front();
                }
            }
            Event::Feed { .. } => {
                s.feed.push_back(e.clone());
                if s.feed.len() > KEEP_FEED {
                    s.feed.pop_front();
                }
            }
            Event::Report(_) => s.report = Some(e.clone()),
            Event::Action { .. } | Event::Delivered { .. } => {}
        }
    }

    pub fn phase(&self, phase: Phase, detail: impl Into<String>) {
        self.send(Event::Phase {
            phase,
            detail: detail.into(),
        });
    }

    pub fn feed(&self, kind: Action, text: impl Into<String>) {
        self.send(Event::Feed {
            text: text.into(),
            kind,
        });
    }

    /// Someone did something (and how it went).
    pub fn action(
        &self,
        id: u32,
        kind: Action,
        team: Option<u32>,
        bytes: usize,
        took: Duration,
        ok: bool,
    ) {
        {
            let mut t = self.tally.lock().unwrap();
            t.recent_actions.push_back(Instant::now());
            t.actions_total += 1;
            *t.actions.entry(kind.label().to_string()).or_default() += 1;
            if !ok {
                t.errors += 1;
                if matches!(t.phase, Some(Phase::Crash) | Some(Phase::Recovering)) {
                    t.errors_while_down += 1;
                }
            }
            match kind {
                Action::Type => t.edits += 1,
                Action::Note => t.notes += 1,
                Action::Picture => t.pictures += 1,
                Action::CreateVault => t.vaults += 1,
                _ => {}
            }
        }
        self.send(Event::Action {
            id,
            kind,
            team,
            bytes: bytes as u32,
            ms: took.as_millis() as u32,
            ok,
        });
    }

    pub fn delivered(&self, to: u32, from: Option<u32>, team: Option<u32>, latency: Duration) {
        let ms = latency.as_millis() as u32;
        {
            let mut t = self.tally.lock().unwrap();
            t.recent_delivered.push_back((Instant::now(), ms));
            t.delivered_total += 1;
            if t.phase == Some(Phase::Busy) {
                t.steady.push(ms);
            }
        }
        self.send(Event::Delivered {
            to,
            from,
            team,
            latency_ms: ms,
        });
    }

    pub fn stats(&self, s: Stats) {
        self.send(Event::Stats(s));
    }

    pub fn report(&self, r: Report) {
        self.send(Event::Report(r));
    }
}
