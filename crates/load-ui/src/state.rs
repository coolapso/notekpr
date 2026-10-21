//! What the dashboard knows, built from the events of the run. Pure, so
//! tested natively; drawing is in `draw.rs`.

use std::collections::{BTreeMap, VecDeque};

use coolpsonotes_load_events::{Action, Event, Hello, LevelResult, Phase, Report, Screen, Stats};

pub const SLOTS: usize = 3;
const KEEP_STATS: usize = 3600;
const KEEP_FEED: usize = 30;
/// More flying at once than this and new deliveries aren't drawn.
const MAX_PARTICLES: usize = 3000;

#[derive(Clone, Debug, Default)]
pub struct PersonView {
    pub name: String,
    pub team: u32,
    pub online: bool,
    /// Has been online: offline after that means trouble, before it just
    /// connecting.
    pub was_online: bool,
    pub arrived: bool,
    /// Typing until (ms, page clock).
    pub typing_until: f64,
    pub last_action: f64,
    pub pos: (f64, f64),
}

#[derive(Clone, Debug)]
pub struct TeamView {
    pub name: String,
    pub owner: u32,
    pub members: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum End {
    Server,
    Person(u32),
}

#[derive(Clone, Debug)]
pub struct Particle {
    pub from: End,
    pub to: End,
    pub kind: Action,
    /// Whose change it is (its colour), if anyone's.
    pub person: Option<u32>,
    pub failed: bool,
    pub t0: f64,
    pub dur: f64,
    pub size: f64,
    /// Sideways bend of the path.
    pub bend: f64,
}

#[derive(Clone, Debug)]
pub struct Cipher {
    pub seq: i64,
    pub size: u32,
    pub hex: String,
    pub at: f64,
}

#[derive(Default)]
pub struct App {
    pub hello: Option<Hello>,
    pub phase: Option<Phase>,
    pub detail: String,
    pub phase_since: f64,
    pub people: Vec<PersonView>,
    pub teams: BTreeMap<u32, TeamView>,
    pub server_up: bool,
    pub server_changed: f64,
    /// While down: for how long, and since when (page clock).
    pub down_for: Option<(u32, f64)>,
    /// The level now: (level, of, people in all, its outage in s).
    pub level: Option<(u32, u32, u32, u32)>,
    pub levels: Vec<LevelResult>,
    /// When each level began (run time, ms).
    pub level_starts: Vec<u64>,
    /// How big people are drawn (from the layout).
    pub dot_radius: f64,
    pub stats: VecDeque<Stats>,
    pub feed: VecDeque<(String, Action, f64)>,
    pub screens: [Option<Screen>; SLOTS],
    pub ciphers: [Option<Cipher>; SLOTS],
    pub report: Option<Report>,
    pub report_at: f64,
    pub particles: Vec<Particle>,
    pub connected: bool,
    /// People need placing again (someone arrived, the window changed).
    pub layout_dirty: bool,
    seed: u64,
}

impl App {
    pub fn new() -> App {
        App {
            seed: 0x2545_F491_4F6C_DD1D,
            ..Default::default()
        }
    }

    fn random(&mut self) -> f64 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        (self.seed >> 11) as f64 / (1u64 << 53) as f64
    }

    fn person(&mut self, id: u32) -> &mut PersonView {
        let i = id as usize;
        if self.people.len() <= i {
            self.people.resize(i + 1, PersonView::default());
            self.layout_dirty = true;
        }
        &mut self.people[i]
    }

    pub fn latest(&self) -> Option<&Stats> {
        self.stats.back()
    }

    pub fn apply(&mut self, e: Event, now: f64) {
        match e {
            // A connection starts with the whole story: start over.
            Event::Hello(h) => {
                let connected = self.connected;
                *self = App::new();
                self.connected = connected;
                self.hello = Some(h);
                self.layout_dirty = true;
            }
            Event::Phase { phase, detail } => {
                if self.phase != Some(phase) {
                    self.phase_since = now;
                }
                self.phase = Some(phase);
                self.detail = detail;
            }
            Event::Person { id, name, team } => {
                let p = self.person(id);
                p.name = name;
                p.team = team;
                self.layout_dirty = true;
            }
            Event::Team {
                id,
                name,
                owner,
                members,
            } => {
                self.teams.insert(
                    id,
                    TeamView {
                        name,
                        owner,
                        members,
                    },
                );
            }
            Event::Online { id, online } => {
                let p = self.person(id);
                p.online = online;
                p.was_online |= online;
                p.arrived = true;
            }
            Event::Action {
                id,
                kind,
                bytes,
                ok,
                ..
            } => {
                {
                    let p = self.person(id);
                    p.arrived = true;
                    p.last_action = now;
                    if kind == Action::Type {
                        p.typing_until = now + 700.0;
                    }
                }
                let size = match kind {
                    Action::Picture => 3.0 + (bytes as f64 / 1024.0).sqrt() * 0.5,
                    Action::Type => 2.2,
                    _ => 3.2,
                };
                let bend = self.random() - 0.5;
                self.particles.push(Particle {
                    from: End::Person(id),
                    to: End::Server,
                    kind,
                    person: Some(id),
                    failed: !ok,
                    t0: now,
                    dur: 420.0 + bend.abs() * 200.0,
                    size,
                    bend,
                });
            }
            Event::Delivered { to, from, .. } => {
                if self.particles.len() < MAX_PARTICLES {
                    let bend = self.random() - 0.5;
                    self.particles.push(Particle {
                        from: End::Server,
                        to: End::Person(to),
                        kind: Action::Type,
                        person: from,
                        failed: false,
                        t0: now,
                        dur: 380.0 + bend.abs() * 200.0,
                        size: 1.8,
                        bend,
                    });
                }
            }
            Event::Screen(s) => {
                let slot = s.slot as usize;
                if slot < SLOTS {
                    self.screens[slot] = Some(s);
                }
            }
            Event::Cipher {
                slot,
                seq,
                size,
                hex,
            } => {
                if (slot as usize) < SLOTS {
                    self.ciphers[slot as usize] = Some(Cipher {
                        seq,
                        size,
                        hex,
                        at: now,
                    });
                }
            }
            Event::Server { up, down_for_s } => {
                if self.server_up != up {
                    self.server_changed = now;
                }
                self.server_up = up;
                self.down_for = down_for_s.filter(|_| !up).map(|s| (s, now));
            }
            Event::Level {
                level,
                of,
                users,
                downtime_s,
            } => {
                if self.level_starts.len() < level as usize {
                    let t = self.latest().map(|s| s.t_ms).unwrap_or(0);
                    self.level_starts.push(t);
                }
                self.level = Some((level, of, users, downtime_s));
            }
            Event::LevelDone(r) => {
                if self.levels.iter().all(|l| l.level != r.level) {
                    self.levels.push(r);
                }
            }
            Event::Stats(s) => {
                if self.stats.back().is_none_or(|l| s.t_ms > l.t_ms) {
                    self.stats.push_back(s);
                    if self.stats.len() > KEEP_STATS {
                        self.stats.pop_front();
                    }
                }
            }
            Event::Feed { text, kind } => {
                self.feed.push_front((text, kind, now));
                self.feed.truncate(KEEP_FEED);
            }
            Event::Report(r) => {
                self.report = Some(r);
                self.report_at = now;
            }
        }
    }

    /// Forget particles that arrived.
    pub fn tick(&mut self, now: f64) {
        self.particles.retain(|p| now - p.t0 < p.dur);
    }

    /// Who is in which team (from people's own team numbers; teams' names
    /// arrive later).
    pub fn team_members(&self) -> BTreeMap<u32, Vec<u32>> {
        let mut m: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for (i, p) in self.people.iter().enumerate() {
            m.entry(p.team).or_default().push(i as u32);
        }
        m
    }

    /// Place people around the server, team by team, on as many rings as
    /// it takes to give everyone some room (sizing them to fit).
    pub fn place(&mut self, cx: f64, cy: f64, rx: f64, ry: f64) {
        let teams = self.team_members();
        let n = self.people.len().max(1) as f64;
        let circumference = std::f64::consts::PI * (rx + ry);
        let rings = ((16.0 * n) / circumference).ceil().clamp(1.0, 10.0) as usize;
        let per_ring = (n / rings as f64).ceil();
        // Rings close together, so a team stays one cluster.
        let ring_step = if rings > 1 {
            (0.45 / (rings - 1) as f64).min(0.08)
        } else {
            0.0
        };
        let along = circumference / per_ring.max(1.0);
        let across = if rings > 1 { ry * ring_step } else { f64::MAX };
        self.dot_radius = (along.min(across) * 0.36).clamp(2.5, 9.0);
        // Each team gets a slice, with a gap between teams.
        let gap = 0.6;
        let slots: f64 = teams
            .values()
            .map(|m| (m.len() as f64 / rings as f64).ceil() + gap)
            .sum();
        let step = std::f64::consts::TAU / slots.max(per_ring);
        let mut angle = -std::f64::consts::FRAC_PI_2;
        for members in teams.values() {
            for (j, &id) in members.iter().enumerate() {
                let ring = j % rings;
                let k = (j / rings) as f64;
                let a = angle + k * step;
                let scale = 1.0 - ring as f64 * ring_step;
                self.people[id as usize].pos =
                    (cx + a.cos() * rx * scale, cy + a.sin() * ry * scale);
            }
            angle += ((members.len() as f64 / rings as f64).ceil() + gap) * step;
        }
        self.layout_dirty = false;
    }

    pub fn position(&self, e: End, server: (f64, f64)) -> (f64, f64) {
        match e {
            End::Server => server,
            End::Person(id) => self
                .people
                .get(id as usize)
                .map(|p| p.pos)
                .unwrap_or(server),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(id: u32, team: u32) -> Event {
        Event::Person {
            id,
            name: format!("P{id}"),
            team,
        }
    }

    #[test]
    fn a_new_connection_starts_over() {
        let mut a = App::new();
        a.apply(person(0, 0), 0.0);
        a.apply(
            Event::Hello(Hello {
                seed: 1,
                users: 1,
                duration_s: 1,
                mode: "docker".into(),
                cpus: 1.0,
                memory: 0,
                started_ms: 0,
                server_url: String::new(),
                levels: 1,
            }),
            1.0,
        );
        assert!(a.people.is_empty());
        assert!(a.hello.is_some());
    }

    #[test]
    fn actions_fly_to_the_server_and_land() {
        let mut a = App::new();
        a.apply(person(3, 0), 0.0);
        a.apply(
            Event::Action {
                id: 3,
                kind: Action::Type,
                team: Some(0),
                bytes: 3,
                ms: 10,
                ok: true,
            },
            100.0,
        );
        assert_eq!(a.particles.len(), 1);
        assert_eq!(a.particles[0].to, End::Server);
        assert!(a.people[3].typing_until > 100.0);
        a.tick(10_000.0);
        assert!(a.particles.is_empty());
    }

    #[test]
    fn stats_only_move_forward() {
        let mut a = App::new();
        for t in [1000, 2000, 1500, 3000] {
            a.apply(
                Event::Stats(Stats {
                    t_ms: t,
                    ..Default::default()
                }),
                0.0,
            );
        }
        let ts: Vec<u64> = a.stats.iter().map(|s| s.t_ms).collect();
        assert_eq!(ts, [1000, 2000, 3000]);
    }

    #[test]
    fn everyone_gets_a_place_and_teams_sit_together() {
        let mut a = App::new();
        for id in 0..40 {
            a.apply(person(id, id % 5), 0.0);
        }
        a.place(500.0, 400.0, 300.0, 250.0);
        let mut seen = std::collections::BTreeSet::new();
        for p in &a.people {
            assert!(p.pos.0 > 150.0 && p.pos.0 < 850.0, "{:?}", p.pos);
            seen.insert(((p.pos.0 * 10.0) as i64, (p.pos.1 * 10.0) as i64));
        }
        assert_eq!(seen.len(), 40, "nobody on top of someone else");
    }
}
