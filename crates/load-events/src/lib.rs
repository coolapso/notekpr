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
    /// Nothing running: the start screen, with what a run would use.
    Menu(Menu),
}

/// What the start screen offers.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Menu {
    pub settings: Settings,
    /// Whether the server runs in Docker (CPU and memory limits apply).
    pub docker: bool,
    /// The outages given on the command line, in seconds (the last
    /// repeats); empty: the default ([`outage_s`]).
    pub downtimes: Vec<u32>,
    /// Why the last run couldn't start, if it couldn't.
    #[serde(default)]
    pub error: Option<String>,
    /// A server elsewhere (`--server`): its address.
    #[serde(default)]
    pub remote: Option<String>,
    /// Outages can't be had (a remote server without commands to kill and
    /// start it).
    #[serde(default)]
    pub no_outages: bool,
}

/// Level `index`'s outage (0-based) in seconds: as given (the last
/// repeating), or 6 s for a single level and from 2 s growing to 90 s over
/// several.
pub fn outage_s(index: usize, levels: u32, given: &[u32]) -> u32 {
    match given.get(index).or(given.last()) {
        Some(&s) => s,
        None if levels <= 1 => 6,
        None => {
            let f = index as f64 / (levels - 1) as f64;
            (2.0 * 45f64.powf(f)).round() as u32
        }
    }
}

/// How a run goes, as chosen on the start screen (or on the command line).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Settings {
    /// People in the first level, and added by each next one.
    pub users: u32,
    pub step: u32,
    pub levels: u32,
    pub from_level: u32,
    /// How long everyone works in each level.
    pub duration_s: u32,
    pub cpus: f32,
    /// Docker syntax: 512m, 1g.
    pub memory: String,
    pub speed: f64,
    pub outages: bool,
    /// Play every level even after the server couldn't take one.
    #[serde(default)]
    pub keep_going: bool,
    /// People as busy as real ones: mostly reading or away, now and then a
    /// sentence at about 40 words a minute. Otherwise busier than anyone,
    /// on purpose.
    #[serde(default)]
    pub realistic: bool,
}

/// What the dashboard asks the runner (sent over the same WebSocket).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Command {
    /// Start a run with these settings.
    Start(Settings),
    /// Back to the start screen (after a run).
    Menu,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Hello {
    pub seed: u64,
    pub users: u32,
    pub duration_s: u32,
    /// "docker" (limited container), "native" (a process, no limits) or
    /// "remote" (a server elsewhere, `server_url`).
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
    /// Whether there is a start screen to go back to afterwards.
    #[serde(default)]
    pub replay: bool,
    /// People as busy as real ones (see [`Settings::realistic`]).
    #[serde(default)]
    pub realistic: bool,
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
    /// The server couldn't take the next level: no more levels (the copies
    /// are still checked).
    GameOver,
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
            Phase::GameOver => "Game over",
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
    /// Changes still waiting on people's devices when the copies were
    /// compared (the server couldn't take them): then copies may differ for
    /// now without anything being lost.
    #[serde(default)]
    pub waiting: u32,
    /// Finished sentences not on the server yet: in their writer's copy,
    /// waiting to be uploaded (not counted as missing).
    #[serde(default)]
    pub sentences_waiting: u32,
    /// The server's limits were lifted for the check (after a game over),
    /// so it could take everyone's uploads.
    #[serde(default)]
    pub limits_lifted: bool,
    #[serde(default)]
    pub levels: Vec<LevelResult>,
    /// Where the server gave up, if it did.
    #[serde(default)]
    pub game_over: Option<GameOver>,
}

/// The level the server couldn't take, and why.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct GameOver {
    pub level: u32,
    pub users: u32,
    pub reason: String,
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
    /// From taking the server down until everyone was connected again:
    /// how long people were away (the outage, stopping and starting it, and
    /// their reconnecting).
    #[serde(default)]
    pub away_s: Option<f32>,
    /// While everyone works (no arrivals, outage or comeback): the
    /// average requests a second and CPU.
    #[serde(default)]
    pub working_rps: f32,
    #[serde(default)]
    pub working_cpu_pct: f32,
}

impl LevelResult {
    /// Whether there are numbers of everyone working in this level (not if
    /// the server gave up first, or its CPU isn't known).
    pub fn worked(&self) -> bool {
        self.working_cpu_pct > 0.0
    }

    /// Requests a second while everyone works (or over the whole level, if
    /// there was no such time).
    pub fn rps(&self) -> f32 {
        if self.working_rps > 0.0 {
            self.working_rps
        } else {
            self.avg_http_rps
        }
    }
}

/// What the check found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Every copy the same, every sentence there.
    Passed,
    /// Changes were still waiting on people's devices: copies can differ
    /// for now, but nothing was found lost.
    Unfinished,
    /// Something is missing or copies differ with nothing left to upload.
    Failed,
}

impl Report {
    pub fn verdict(&self) -> Verdict {
        if self.notes_checked == 0
            || self.sentences_missing > 0
            || (self.mismatches > 0 && self.waiting == 0)
        {
            Verdict::Failed
        } else if self.waiting > 0 || self.mismatches > 0 || self.sentences_waiting > 0 {
            Verdict::Unfinished
        } else {
            Verdict::Passed
        }
    }

    pub fn passed(&self) -> bool {
        self.verdict() == Verdict::Passed
    }

    /// The check's outcome in a sentence.
    pub fn verdict_text(&self) -> String {
        match self.verdict() {
            Verdict::Passed => "every copy of every note is the same, nothing was lost".into(),
            Verdict::Unfinished => format!(
                "the check couldn't finish: {} changes still wait on people's devices (the \
                 server couldn't take them), so {} copies differ for now and {} sentences \
                 are only on their writers' devices — nothing was found lost",
                self.waiting, self.mismatches, self.sentences_waiting
            ),
            Verdict::Failed => format!(
                "{} copies differ, {} of {} finished sentences missing",
                self.mismatches, self.sentences_missing, self.sentences
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_still_on_devices_are_not_called_lost() {
        let checked = Report {
            notes_checked: 10,
            copies_checked: 40,
            sentences: 100,
            ..Default::default()
        };
        assert_eq!(checked.verdict(), Verdict::Passed);
        // Copies differ while changes still wait on devices: unfinished.
        let waiting = Report {
            mismatches: 3,
            waiting: 120,
            sentences_waiting: 2,
            ..checked.clone()
        };
        assert_eq!(waiting.verdict(), Verdict::Unfinished);
        assert!(waiting.verdict_text().contains("nothing was found lost"));
        // A sentence gone from its writer's copy, or copies that differ with
        // nothing left to upload: failed.
        let lost = Report {
            sentences_missing: 1,
            ..waiting.clone()
        };
        assert_eq!(lost.verdict(), Verdict::Failed);
        let differ = Report {
            mismatches: 1,
            ..checked.clone()
        };
        assert_eq!(differ.verdict(), Verdict::Failed);
        // Nothing compared is no pass.
        assert_eq!(Report::default().verdict(), Verdict::Failed);
    }
}
