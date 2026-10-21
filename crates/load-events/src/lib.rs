//! What the load test (`coolpsonotes-load`) streams to its live dashboard
//! (`coolpsonotes-load-ui`): one JSON array of [`Event`]s per WebSocket
//! message.
//!
//! Everything here comes from something that really happened: a simulated
//! person's request, an update another one received, the server's own
//! metrics, the container's CPU and memory. Nothing is made up for the show.
//! People are numbered `0..users`; teams `0..teams`.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Event {
    /// The run's settings; first event on every connection.
    Hello(Hello),
    /// The part of the run we're in.
    Phase { phase: Phase, detail: String },
    /// Someone arrived (signed up: account keys, a first device).
    Person { id: u32, name: String, team: u32 },
    /// A team and its shared vault (`owner` created it and shared it).
    Team {
        id: u32,
        name: String,
        owner: u32,
        members: Vec<u32>,
    },
    /// A person's live connection (WebSocket) went up or down.
    Online { id: u32, online: bool },
    /// A person did something: `ms` from starting it until the server had it
    /// (or it was queued, when the server is down), `bytes` the size of the
    /// change.
    Action {
        id: u32,
        kind: Action,
        team: Option<u32>,
        bytes: u32,
        ms: u32,
        ok: bool,
    },
    /// `to` received someone's change live, `latency_ms` after `from` made it.
    Delivered {
        to: u32,
        from: Option<u32>,
        team: Option<u32>,
        latency_ms: u32,
    },
    /// A note being written right now, as one of its people sees it.
    Screen(Screen),
    /// What the server stores for that note's newest change: ciphertext.
    Cipher {
        slot: u8,
        seq: i64,
        size: u32,
        hex: String,
    },
    /// The server went down (crashed on purpose) or came back; while down,
    /// for how long it will be.
    Server {
        up: bool,
        #[serde(default)]
        down_for_s: Option<u32>,
    },
    /// A level starts: `users` people in all, a crash of `downtime_s`.
    Level {
        level: u32,
        of: u32,
        users: u32,
        downtime_s: u32,
    },
    /// A level is over.
    LevelDone(LevelResult),
    /// Measurements, about once a second.
    Stats(Stats),
    /// A line for the activity feed.
    Feed { text: String, kind: Action },
    /// The end of the run.
    Report(Report),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Hello {
    pub seed: u64,
    pub users: u32,
    pub duration_s: u32,
    /// "docker" (limited container) or "native" (a process, no limits).
    pub mode: String,
    /// CPU limit in cores (0 = none).
    pub cpus: f32,
    /// Memory limit in bytes (0 = none).
    pub memory: u64,
    /// When the run started (ms since the epoch).
    pub started_ms: i64,
    /// The server under load, for watching it in a browser.
    pub server_url: String,
    /// How many levels (1 = a single crowd).
    #[serde(default)]
    pub levels: u32,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Starting,
    Arriving,
    Teaming,
    Busy,
    Crash,
    Recovering,
    Checking,
    Done,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Starting => "Starting the server",
            Phase::Arriving => "People arriving",
            Phase::Teaming => "Teams sharing vaults",
            Phase::Busy => "Busy",
            Phase::Crash => "Server crashed",
            Phase::Recovering => "Recovering",
            Phase::Checking => "Checking every copy",
            Phase::Done => "Done",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    SignUp,
    CreateVault,
    Share,
    Type,
    Note,
    Picture,
    Quick,
    Rename,
    Trash,
    ShareNote,
    Revision,
}

impl Action {
    pub const ALL: [Action; 11] = [
        Action::SignUp,
        Action::CreateVault,
        Action::Share,
        Action::Type,
        Action::Note,
        Action::Picture,
        Action::Quick,
        Action::Rename,
        Action::Trash,
        Action::ShareNote,
        Action::Revision,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Action::SignUp => "sign-ups",
            Action::CreateVault => "vaults",
            Action::Share => "shares",
            Action::Type => "typing",
            Action::Note => "new notes",
            Action::Picture => "pictures",
            Action::Quick => "quick notes",
            Action::Rename => "renames",
            Action::Trash => "trashed",
            Action::ShareNote => "notes shared",
            Action::Revision => "revisions",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Screen {
    pub slot: u8,
    pub team: u32,
    pub title: String,
    /// The note's last lines.
    pub lines: Vec<String>,
    /// Where the people writing in it are (in `lines`).
    pub cursors: Vec<Cursor>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Cursor {
    pub person: u32,
    pub line: u32,
    pub col: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Stats {
    /// Since the start of the run.
    pub t_ms: u64,
    pub server_up: bool,
    /// Of one core (100 = one full core).
    pub cpu_pct: f32,
    pub mem_bytes: u64,
    /// HTTP requests per second, and their time at the server.
    pub http_rps: f32,
    pub http_p50_ms: f32,
    pub http_p99_ms: f32,
    /// Encrypted changes the server stored per second.
    pub updates_ps: f32,
    pub ws_connections: u32,
    pub online: u32,
    /// Everyone's actions per second.
    pub actions_ps: f32,
    /// From one person's edit to another one having it.
    pub propagation_p50_ms: f32,
    pub propagation_p99_ms: f32,
    pub delivered_ps: f32,
    pub errors: u32,
    pub edits: u64,
    pub notes: u32,
    pub pictures: u32,
    pub vaults: u32,
    /// Changes waiting on people's devices (e.g. while the server is down).
    pub queued: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub duration_s: f32,
    pub users: u32,
    pub teams: u32,
    pub vaults: u32,
    pub notes: u32,
    pub pictures: u32,
    pub edits: u64,
    pub actions: u64,
    pub errors: u64,
    /// Of those, while the server was down (sharing, for one, needs it).
    #[serde(default)]
    pub errors_while_down: u64,
    pub peak_cpu_pct: f32,
    pub peak_mem_bytes: u64,
    pub avg_http_rps: f32,
    pub peak_http_rps: f32,
    pub http_p99_ms: f32,
    pub propagation_p50_ms: f32,
    pub propagation_p99_ms: f32,
    /// How long it took after the crash until everyone was back online.
    pub recovery_s: Option<f32>,
    /// Notes compared across every person who has them.
    pub notes_checked: u32,
    pub copies_checked: u32,
    /// Copies that differ from the others (should be 0).
    pub mismatches: u32,
    /// Sentences people finished typing, and how many of them are missing
    /// from the final text (should be 0).
    pub sentences: u32,
    pub sentences_missing: u32,
    #[serde(default)]
    pub levels: Vec<LevelResult>,
}

/// One level of the run: how the server did with this many people, and
/// how it came back from this long an outage.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct LevelResult {
    pub level: u32,
    pub users: u32,
    pub duration_s: f32,
    pub actions: u64,
    pub edits: u64,
    pub errors: u64,
    pub avg_http_rps: f32,
    pub peak_http_rps: f32,
    pub peak_cpu_pct: f32,
    pub peak_mem_bytes: u64,
    pub http_p99_ms: f32,
    /// Keystroke to others' copies, outside the outage.
    pub propagation_p50_ms: f32,
    pub propagation_p99_ms: f32,
    pub downtime_s: u32,
    /// From the restart until everyone was connected again.
    #[serde(default)]
    pub online_s: Option<f32>,
    /// From the restart until the outage's backlog was worked off (changes
    /// waiting on devices back down to where they were before the crash);
    /// none if not within two minutes.
    pub recovery_s: Option<f32>,
    /// The most changes waiting on people's devices at once.
    pub max_queued: u32,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.mismatches == 0 && self.sentences_missing == 0 && self.notes_checked > 0
    }
}
