//! What the dashboard knows, built from the events of the run. Pure, so
//! tested natively; drawing is in `draw.rs`.

use std::collections::{BTreeMap, VecDeque};

use coolpsonotes_load_events::{
    Action, Command, Event, Hello, LevelResult, Menu, Phase, Report, Screen, Settings, Stats,
};

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
    /// The start screen, while nothing runs.
    pub menu: Option<MenuState>,
    /// Where things can be clicked this frame (filled while drawing).
    pub hits: Vec<(f64, f64, f64, f64, Hit)>,
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
            // Nothing runs: the start screen.
            Event::Menu(m) => {
                let connected = self.connected;
                *self = App::new();
                self.connected = connected;
                self.menu = Some(MenuState {
                    settings: m.settings.clone(),
                    menu: m,
                    selected: OPTIONS,
                });
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

    /// Place people around the server, team after team along a ring (a
    /// small gap between teams), then on the next ring inwards: a big crowd
    /// fills in from the outside, level by level. People are sized to fit.
    pub fn place(&mut self, cx: f64, cy: f64, rx: f64, ry: f64) {
        let teams = self.team_members();
        let n = self.people.len().max(1) as f64;
        let circumference = std::f64::consts::PI * (rx + ry);
        let rings = ((16.0 * n) / circumference).ceil().clamp(1.0, 10.0) as usize;
        let ring_step = if rings > 1 {
            (0.45 / (rings - 1) as f64).min(0.12)
        } else {
            0.0
        };
        let gap = 0.8;
        let total = n + gap * teams.len() as f64;
        let per_ring = (total / rings as f64).ceil().max(1.0);
        let inner = 1.0 - ring_step * (rings - 1) as f64;
        let along = circumference * inner / per_ring;
        let across = if rings > 1 { ry * ring_step } else { f64::MAX };
        self.dot_radius = (along.min(across) * 0.38).clamp(2.5, 9.0);
        let mut cursor = 0.0;
        for members in teams.values() {
            for &id in members {
                let ring = ((cursor / per_ring) as usize).min(rings - 1);
                let a = -std::f64::consts::FRAC_PI_2
                    + (cursor % per_ring) * std::f64::consts::TAU / per_ring;
                let scale = 1.0 - ring as f64 * ring_step;
                self.people[id as usize].pos =
                    (cx + a.cos() * rx * scale, cy + a.sin() * ry * scale);
                cursor += 1.0;
            }
            cursor += gap;
        }
        self.layout_dirty = false;
    }

    /// A key or click: what to ask the runner, if anything.
    pub fn input(&mut self, i: Input) -> Option<Command> {
        let hit = match i {
            Input::Click(x, y) => self
                .hits
                .iter()
                .find(|(hx, hy, w, h, _)| x >= *hx && x <= hx + w && y >= *hy && y <= hy + h)
                .map(|(.., h)| *h),
            _ => None,
        };
        if let Some(m) = &mut self.menu {
            let docker = m.menu.docker;
            match (i, hit) {
                (Input::Up, _) => m.selected = (m.selected + OPTIONS) % (OPTIONS + 1),
                (Input::Down, _) => m.selected = (m.selected + 1) % (OPTIONS + 1),
                (Input::Left, _) if m.selected < OPTIONS => {
                    change(&mut m.settings, m.selected, -1, docker)
                }
                (Input::Right, _) if m.selected < OPTIONS => {
                    change(&mut m.settings, m.selected, 1, docker)
                }
                (Input::Enter, _) if m.selected < OPTIONS => {
                    change(&mut m.settings, m.selected, 1, docker)
                }
                (Input::Enter, _) | (_, Some(Hit::Start)) => {
                    return Some(Command::Start(m.settings.clone()));
                }
                (_, Some(Hit::Row(r))) => {
                    m.selected = r;
                    change(&mut m.settings, r, 1, docker);
                }
                (_, Some(Hit::Dec(r))) => {
                    m.selected = r;
                    change(&mut m.settings, r, -1, docker);
                }
                (_, Some(Hit::Inc(r))) => {
                    m.selected = r;
                    change(&mut m.settings, r, 1, docker);
                }
                _ => {}
            }
            return None;
        }
        // After a run: insert coin for another.
        let replay = self.hello.as_ref().is_some_and(|h| h.replay);
        match (i, hit) {
            (Input::Enter, _) | (_, Some(Hit::Coin)) if self.report.is_some() && replay => {
                Some(Command::Menu)
            }
            _ => None,
        }
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

/// What can be clicked.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Row(usize),
    Dec(usize),
    Inc(usize),
    Start,
    Coin,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Click(f64, f64),
}

/// The start screen: what the runner offered, and what is chosen.
#[derive(Clone, Debug)]
pub struct MenuState {
    pub menu: Menu,
    pub settings: Settings,
    /// The option under the cursor; `OPTIONS` is START.
    pub selected: usize,
}

/// How many settings there are (START comes after them).
pub const OPTIONS: usize = 8;

pub fn option_label(i: usize) -> &'static str {
    [
        "START AT LEVEL",
        "LEVELS",
        "PEOPLE PER LEVEL",
        "SERVER CPU",
        "SERVER MEMORY",
        "TYPING SPEED",
        "WORK PER LEVEL",
        "OUTAGES",
    ][i]
}

pub fn option_value(s: &Settings, i: usize, docker: bool) -> String {
    match i {
        0 => s.from_level.to_string(),
        1 => s.levels.to_string(),
        2 => s.step.max(s.users).to_string(),
        3 if !docker => "no limit".into(),
        3 => format!(
            "{} {}",
            trim(s.cpus as f64),
            if s.cpus == 1.0 { "core" } else { "cores" }
        ),
        4 if !docker => "no limit".into(),
        4 => memory_label(&s.memory),
        5 => format!("{}×", trim(s.speed)),
        6 => format!("{} s", s.duration_s),
        _ => if s.outages { "ON" } else { "OFF" }.into(),
    }
}

fn trim(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn memory_label(m: &str) -> String {
    let m = m.trim().to_lowercase();
    match m.strip_suffix('g') {
        Some(g) => format!("{g} GB"),
        None => format!("{} MB", m.trim_end_matches('m')),
    }
}

/// The next or previous choice of a setting (wrapping around).
pub fn change(s: &mut Settings, i: usize, dir: i32, docker: bool) {
    fn step<T: PartialOrd + Copy>(list: &[T], now: T, dir: i32) -> T {
        let i = list.iter().position(|v| *v >= now).unwrap_or(0) as i32;
        let exact = list.get(i as usize).is_some_and(|v| *v == now);
        let next = if exact || dir > 0 {
            i + dir
        } else {
            i - 1 + dir.max(0)
        };
        list[next.rem_euclid(list.len() as i32) as usize]
    }
    match i {
        0 => {
            s.from_level =
                (s.from_level as i32 - 1 + dir).rem_euclid(s.levels.max(1) as i32) as u32 + 1
        }
        1 => {
            s.levels = step(&[1, 2, 3, 5, 10, 15, 20], s.levels, dir);
            s.from_level = s.from_level.min(s.levels);
        }
        2 => {
            let n = step(&[25, 50, 100, 200, 300, 500], s.step.max(s.users), dir);
            s.users = n;
            s.step = n;
        }
        3 if docker => s.cpus = step(&[0.25, 0.5, 1.0, 2.0, 4.0, 8.0], s.cpus, dir),
        4 if docker => {
            let sizes = ["128m", "256m", "512m", "1g", "2g", "4g"];
            let mb = |m: &str| {
                let m = m.trim().to_lowercase();
                match m.strip_suffix('g') {
                    Some(g) => g.parse::<f64>().unwrap_or(0.5) * 1024.0,
                    None => m.trim_end_matches('m').parse::<f64>().unwrap_or(512.0),
                }
            };
            let now = mb(&s.memory);
            let values: Vec<f64> = sizes.iter().map(|m| mb(m)).collect();
            let v = step(&values, now, dir);
            let k = values.iter().position(|x| *x == v).unwrap_or(2);
            s.memory = sizes[k].to_string();
        }
        5 => s.speed = step(&[0.5, 1.0, 2.0, 4.0], s.speed, dir),
        6 => s.duration_s = step(&[20, 30, 45, 60, 90, 120], s.duration_s, dir),
        7 => s.outages = !s.outages,
        _ => {}
    }
}

/// "level 6: 600 people, 17 s outage" for the chosen start.
pub fn start_preview(m: &MenuState) -> String {
    let s = &m.settings;
    let level = s.from_level.max(1);
    let people = s.users + s.step * (level - 1);
    let mut out = format!("level {level}: {people} people");
    if s.outages {
        let down =
            coolpsonotes_load_events::outage_s(level as usize - 1, s.levels, &m.menu.downtimes);
        out.push_str(&format!(", {down} s outage"));
    }
    if s.levels > level {
        let last = s.users + s.step * (s.levels - 1);
        out.push_str(&format!(" · then up to level {} ({last} people)", s.levels));
    }
    out
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
                replay: false,
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

    fn settings() -> Settings {
        Settings {
            users: 100,
            step: 100,
            levels: 10,
            from_level: 1,
            duration_s: 45,
            cpus: 1.0,
            memory: "512m".into(),
            speed: 1.0,
            outages: true,
        }
    }

    fn menu_app() -> App {
        let mut a = App::new();
        a.apply(
            Event::Menu(Menu {
                settings: settings(),
                docker: true,
                downtimes: vec![],
                error: None,
            }),
            0.0,
        );
        a
    }

    #[test]
    fn choosing_on_the_start_screen() {
        let mut a = menu_app();
        assert_eq!(a.menu.as_ref().unwrap().selected, OPTIONS, "START first");
        // Down to the first option, then a later level.
        assert_eq!(a.input(Input::Down), None);
        for _ in 0..5 {
            a.input(Input::Right);
        }
        let m = a.menu.as_ref().unwrap();
        assert_eq!(m.settings.from_level, 6);
        assert_eq!(
            start_preview(m),
            "level 6: 600 people, 17 s outage · then up to level 10 (1000 people)"
        );
        // More CPU and memory.
        for _ in 0..3 {
            a.input(Input::Down);
        }
        a.input(Input::Right);
        a.input(Input::Down);
        a.input(Input::Right);
        let s = &a.menu.as_ref().unwrap().settings;
        assert_eq!((s.cpus, s.memory.as_str()), (2.0, "1g"));
        assert_eq!(option_value(s, 3, true), "2 cores");
        assert_eq!(option_value(s, 4, true), "1 GB");
        // Wrapping round, and back.
        a.input(Input::Left);
        a.input(Input::Left);
        assert_eq!(a.menu.as_ref().unwrap().settings.memory, "256m");
        // START sends the settings.
        a.menu.as_mut().unwrap().selected = OPTIONS;
        match a.input(Input::Enter) {
            Some(Command::Start(s)) => assert_eq!((s.from_level, s.cpus), (6, 2.0)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn insert_coin_only_after_a_report_and_when_there_is_a_start_screen() {
        let mut a = App::new();
        let hello = |replay| {
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
                replay,
            })
        };
        a.apply(hello(true), 0.0);
        assert_eq!(a.input(Input::Enter), None, "still running");
        a.apply(Event::Report(Report::default()), 0.0);
        assert_eq!(a.input(Input::Enter), Some(Command::Menu));
        a.apply(hello(false), 0.0);
        a.apply(Event::Report(Report::default()), 0.0);
        assert_eq!(
            a.input(Input::Enter),
            None,
            "--now: no start screen to go back to"
        );
    }

    #[test]
    fn a_thousand_fit_without_overlapping() {
        let mut a = App::new();
        for id in 0..1000 {
            a.apply(person(id, id / 5), 0.0);
        }
        a.place(500.0, 300.0, 400.0, 190.0);
        let r = a.dot_radius;
        assert!(r >= 2.5, "{r}");
        let mut seen = std::collections::BTreeSet::new();
        for p in &a.people {
            seen.insert(((p.pos.0 / r) as i64, (p.pos.1 / r) as i64));
        }
        assert!(seen.len() > 950, "{} distinct places", seen.len());
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
