//! Where everything that happens goes: batched for the dashboards, counted
//! for the stats, and remembered for dashboards that connect later.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notekpr_load_events::{Action, Event, Hello, Menu, Phase, Report, Stats};
use tokio::sync::broadcast;

/// How many of the latest events of a kind a late dashboard gets (stats:
/// an hour, one a second).
const KEEP_STATS: usize = 3600;
/// At most this many edits (up, and arriving) a second are drawn; the
/// numbers count all of them.
const DRAWN_PER_SECOND: f64 = 1200.0;
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
    level: Option<Event>,
    levels: Vec<Event>,
    stats: VecDeque<Event>,
    feed: VecDeque<Event>,
    report: Option<Event>,
    menu: Option<Event>,
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
    /// Propagation latencies outside outages, for the report: this level's
    /// and the whole run's (a fair random sample of each).
    pub steady: Reservoir,
    pub steady_all: Reservoir,
    pub phase: Option<Phase>,
}

/// A uniform random sample of at most `CAP` values from a stream of any
/// length (Algorithm R): percentiles without keeping millions of numbers.
#[derive(Default)]
pub struct Reservoir {
    seen: u64,
    items: Vec<u32>,
    state: u64,
}

impl Reservoir {
    const CAP: usize = 200_000;

    pub fn add(&mut self, v: u32) {
        self.seen += 1;
        if self.items.len() < Self::CAP {
            self.items.push(v);
            return;
        }
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let j = (self.state >> 11) % self.seen;
        if (j as usize) < Self::CAP {
            self.items[j as usize] = v;
        }
    }

    /// p50 and p99.
    pub fn percentiles(&self) -> (f32, f32) {
        let mut v = self.items.clone();
        v.sort_unstable();
        (percentile(&v, 0.5), percentile(&v, 0.99))
    }
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

/// Lets a sample through: `DRAWN_PER_SECOND` (each for actions and for
/// arrivals), refilled continuously.
struct Budget {
    actions: f64,
    delivered: f64,
    at: Instant,
}

impl Budget {
    fn refill(&mut self) {
        let now = Instant::now();
        let add = now.duration_since(self.at).as_secs_f64() * DRAWN_PER_SECOND;
        self.actions = (self.actions + add).min(DRAWN_PER_SECOND / 4.0);
        self.delivered = (self.delivered + add).min(DRAWN_PER_SECOND / 4.0);
        self.at = now;
    }
}

#[derive(Clone)]
pub struct Bus {
    tx: broadcast::Sender<Arc<String>>,
    pending: Arc<Mutex<Vec<Event>>>,
    sticky: Arc<Mutex<Sticky>>,
    budget: Arc<Mutex<Budget>>,
    pub tally: Arc<Mutex<Tally>>,
}

impl Bus {
    /// Lives as long as the dashboard; runs come and go ([`Bus::start_run`]).
    pub fn new() -> Bus {
        let (tx, _) = broadcast::channel(4096);
        let bus = Bus {
            tx,
            pending: Arc::default(),
            sticky: Arc::default(),
            budget: Arc::new(Mutex::new(Budget {
                actions: 0.0,
                delivered: 0.0,
                at: Instant::now(),
            })),
            tally: Arc::default(),
        };
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

    /// Forget the last run (what dashboards are told and what was counted).
    fn reset(&self) {
        self.pending.lock().unwrap().clear();
        *self.sticky.lock().unwrap() = Sticky::default();
        *self.tally.lock().unwrap() = Tally::default();
    }

    /// A run begins: dashboards start over with its settings.
    pub fn start_run(&self, hello: Hello) {
        self.reset();
        self.send(Event::Hello(hello));
    }

    /// Nothing runs: dashboards show the start screen.
    pub fn menu(&self, m: Menu) {
        self.reset();
        self.send(Event::Menu(m));
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<String>> {
        self.tx.subscribe()
    }

    /// Everything a dashboard that connects now needs, as one message.
    pub fn snapshot(&self) -> String {
        let s = self.sticky.lock().unwrap();
        let mut out: Vec<&Event> = Vec::new();
        out.extend(s.menu.iter());
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
        out.extend(s.levels.iter());
        out.extend(s.level.iter());
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
            Event::Level { .. } => s.level = Some(e.clone()),
            Event::LevelDone(_) => s.levels.push(e.clone()),
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
            Event::Menu(_) => s.menu = Some(e.clone()),
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
        // Everything but typing is rare enough to always show.
        if kind == Action::Type && !self.take(|b| &mut b.actions) {
            return;
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
                t.steady.add(ms);
                t.steady_all.add(ms);
            }
        }
        if !self.take(|b| &mut b.delivered) {
            return;
        }
        self.send(Event::Delivered {
            to,
            from,
            team,
            latency_ms: ms,
        });
    }

    /// Whether this one is drawn (see [`DRAWN_PER_SECOND`]).
    fn take(&self, which: impl Fn(&mut Budget) -> &mut f64) -> bool {
        let mut b = self.budget.lock().unwrap();
        b.refill();
        let tokens = which(&mut b);
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            true
        } else {
            false
        }
    }

    pub fn stats(&self, s: Stats) {
        self.send(Event::Stats(s));
    }

    pub fn report(&self, r: Report) {
        self.send(Event::Report(r));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sample_keeps_the_shape_of_a_long_stream() {
        let mut r = Reservoir::default();
        for i in 0..1_000_000u32 {
            r.add(i % 1000);
        }
        assert_eq!(r.items.len(), Reservoir::CAP);
        let (p50, p99) = r.percentiles();
        assert!((480.0..520.0).contains(&p50), "{p50}");
        assert!((980.0..=999.0).contains(&p99), "{p99}");
    }
}
