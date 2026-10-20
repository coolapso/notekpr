//! A simulated person: a real client (the CLI's library — account keys,
//! end-to-end encryption, a WebSocket for live changes, uploads over HTTP)
//! with a habit of doing what people do in notes.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use coolpsonotes_client::account::{self, LoginOptions};
use coolpsonotes_client::hub::{Hub, HubEvent, HubOptions};
use coolpsonotes_client::profile::Dirs;
use coolpsonotes_client::session::Session;
use coolpsonotes_client::{livesync, manage, note_shares, ops};
use coolpsonotes_core::protocol::Role;
use coolpsonotes_load_events::Action;
use tokio::sync::{broadcast, watch};

use crate::bus::Bus;
use crate::world::{self, Rng};

/// A team and its shared vault.
#[derive(Clone, Debug)]
pub struct Team {
    pub id: u32,
    pub name: String,
    pub owner: u32,
    pub members: Vec<u32>,
    pub vault_id: String,
    /// The note everyone writes in together.
    pub hot_id: String,
    pub hot_title: String,
}

/// Per writer: their part's length after each keystroke, and when.
pub type Keystrokes = HashMap<u32, Vec<(usize, Instant)>>;
/// A person's client: their session and their copies of the vaults.
pub type Client = (Arc<Session>, Arc<Hub>);

/// What everyone shares: who is who, and what has been written where.
pub struct World {
    pub bus: Bus,
    pub server_url: String,
    pub root: PathBuf,
    pub names: Vec<(String, String)>,
    pub team_of: Vec<u32>,
    pub teams: Mutex<Vec<Team>>,
    /// Per note and writer: the length of their part after each of their
    /// keystrokes, and when they typed it. Someone else's copy having that
    /// much of the part means the keystroke reached them: exact latencies,
    /// whoever else types at the same time.
    pub typed: Mutex<HashMap<String, Keystrokes>>,
    /// How many of a writer's keystrokes each reader has been credited with.
    pub credited: Mutex<HashMap<(u32, String, u32), usize>>,
    /// Sentences people finished, per (vault, note).
    pub sentences: Mutex<HashMap<(String, String), Vec<String>>>,
    /// Who is typing in which note right now.
    pub typing: Mutex<HashMap<String, BTreeSet<u32>>>,
    /// Every person's client, by id (filled as they arrive).
    pub clients: Mutex<Vec<Option<Client>>>,
}

impl World {
    pub fn new(
        bus: Bus,
        server_url: String,
        root: PathBuf,
        people: u32,
        team_of: Vec<u32>,
    ) -> World {
        World {
            bus,
            server_url,
            root,
            names: (0..people).map(world::person_name).collect(),
            team_of,
            teams: Mutex::default(),
            typed: Mutex::default(),
            credited: Mutex::default(),
            sentences: Mutex::default(),
            typing: Mutex::default(),
            clients: Mutex::new(vec![None; people as usize]),
        }
    }

    pub fn display(&self, id: u32) -> &str {
        &self.names[id as usize].0
    }

    pub fn team(&self, id: u32) -> Option<Team> {
        self.teams
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.id == id)
            .cloned()
    }

    fn team_of_vault(&self, vault_id: &str) -> Option<u32> {
        self.teams
            .lock()
            .unwrap()
            .iter()
            .find(|t| t.vault_id == vault_id)
            .map(|t| t.id)
    }
}

pub struct Person {
    pub id: u32,
    pub team: u32,
    pub session: Arc<Session>,
    pub hub: Arc<Hub>,
    pub personal: String,
    world: Arc<World>,
    rng: Rng,
    live_stop: watch::Sender<bool>,
    live: tokio::task::JoinHandle<Result<()>>,
    notes_made: u32,
    pictures: u32,
}

impl Person {
    /// Sign up (account keys, a device), make a vault of one's own and go
    /// live.
    pub async fn arrive(world: Arc<World>, id: u32, seed: u64) -> Result<Person> {
        let (display, login) = world.names[id as usize].clone();
        let dirs = Dirs::under(&world.root.join(&login));
        let t0 = Instant::now();
        account::login(
            &dirs,
            &login,
            LoginOptions {
                server: world.server_url.clone(),
                dev_user: Some(login.clone()),
                device_name: Some(format!("{display}'s laptop")),
                ..Default::default()
            },
        )
        .await
        .with_context(|| format!("signing up {login}"))?;
        let session = Arc::new(Session::open(&dirs, &login)?);
        world
            .bus
            .action(id, Action::SignUp, None, 0, t0.elapsed(), true);
        world
            .bus
            .feed(Action::SignUp, format!("{display} signed up"));

        let t0 = Instant::now();
        let v = manage::create_vault(&session, &format!("{display}'s notes")).await?;
        let personal = v.id().to_string();
        world
            .bus
            .action(id, Action::CreateVault, None, 0, t0.elapsed(), true);

        let hub = Hub::start(
            session.clone(),
            HubOptions {
                catch_up: true,
                offline_ok: true,
                ..Default::default()
            },
        )
        .await?;
        // Listen first, so the first "connected" isn't missed.
        tokio::spawn(watch_hub(world.clone(), id, hub.clone(), hub.subscribe()));
        let (live_stop, rx) = watch::channel(false);
        let live = tokio::spawn(livesync::run(hub.clone(), rx));
        world.clients.lock().unwrap()[id as usize] = Some((session.clone(), hub.clone()));
        Ok(Person {
            id,
            team: world.team_of[id as usize],
            session,
            hub,
            personal,
            rng: Rng::new(seed ^ (id as u64).wrapping_mul(0x2545_F491_4F6C_DD1D)),
            world,
            live_stop,
            live,
            notes_made: 0,
            pictures: 0,
        })
    }

    fn display(&self) -> String {
        self.world.display(self.id).to_string()
    }

    /// As a team's owner: create its vault, share it with everyone in it and
    /// write the first notes.
    pub async fn found_team(&mut self, team: u32, name: &str, members: &[u32]) -> Result<Team> {
        let t0 = Instant::now();
        let v = manage::create_vault(&self.session, name).await?;
        let vault_id = v.id().to_string();
        self.world.bus.action(
            self.id,
            Action::CreateVault,
            Some(team),
            0,
            t0.elapsed(),
            true,
        );
        let list = self.session.api.vaults().await?;
        self.hub.open_listed(&list).await?;
        let shared = self
            .hub
            .get(&vault_id)
            .ok_or_else(|| anyhow!("the new vault didn't open"))?;
        for &m in members.iter().filter(|&&m| m != self.id) {
            let t0 = Instant::now();
            let login = self.world.names[m as usize].1.clone();
            let res = manage::share(&self.session, &shared, &login, Role::Editor, false).await;
            let ok = ok(&res);
            self.world
                .bus
                .action(self.id, Action::Share, Some(team), 0, t0.elapsed(), ok);
            if ok {
                self.world.bus.feed(
                    Action::Share,
                    format!(
                        "{} shared “{name}” with {}",
                        self.display(),
                        self.world.display(m)
                    ),
                );
            }
        }
        let hot_title = self
            .rng
            .pick(&["Weekly plan", "Notes", "Shared notes", "This week", "Plan"])
            .to_string();
        let names: Vec<String> = members
            .iter()
            .map(|&m| self.world.display(m).to_string())
            .collect();
        let others: Vec<String> = (0..3)
            .map(|i| format!("{} {}", self.rng.pick(world::TITLES), i + 1))
            .collect();
        let hot = ops::put_document(
            &self.hub,
            &vault_id,
            &format!("{hot_title}.md"),
            &sectioned(&hot_title, &others, &names),
            None,
        )
        .await
        .map_err(|e| anyhow!("{e}"))?;
        for (i, title) in others.iter().enumerate() {
            let link = &others[(i + 1) % others.len()];
            ops::put_document(
                &self.hub,
                &vault_id,
                &format!("{title}.md"),
                &sectioned(title, &[link.clone(), hot_title.clone()], &names),
                None,
            )
            .await
            .map_err(|e| anyhow!("{e}"))?;
        }
        Ok(Team {
            id: team,
            name: name.to_string(),
            owner: self.id,
            members: members.to_vec(),
            vault_id,
            hot_id: hot.id,
            hot_title,
        })
    }

    /// Wait until the team's vault reached this person.
    pub async fn join(&self, vault_id: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.hub.get(vault_id).is_some() {
                return true;
            }
            if let Ok(list) = self.session.api.vaults().await {
                let _ = self.hub.open_listed(&list).await;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        false
    }

    /// Do what people do until told to stop.
    pub async fn work(mut self, stop: watch::Receiver<bool>) -> Person {
        let mut next_revision = Instant::now() + Duration::from_secs(self.rng.range(10, 25));
        while !*stop.borrow() {
            let r = match self.rng.weighted(&[62, 9, 4, 8, 3, 2, 2]) {
                0 => self.type_some(&stop).await,
                1 => self.new_note().await,
                2 => self.picture().await,
                3 => self.quick_note().await,
                4 => self.rename().await,
                5 => self.trash().await,
                _ => self.share_note().await,
            };
            if let Err(e) = r {
                tracing::debug!(person = self.id, "{e:#}");
            }
            if Instant::now() >= next_revision {
                let t0 = Instant::now();
                let n = self.hub.snapshot_idle(Duration::from_secs(5), 5).await;
                if n > 0 {
                    self.world.bus.action(
                        self.id,
                        Action::Revision,
                        Some(self.team),
                        0,
                        t0.elapsed(),
                        true,
                    );
                }
                next_revision = Instant::now() + Duration::from_secs(self.rng.range(15, 30));
            }
            let think = Duration::from_millis(self.rng.range(600, 3500));
            if sleep_or_stop(think, &stop).await {
                break;
            }
        }
        self
    }

    /// The team's notes (id), the shared one more often.
    fn team_note(&mut self) -> Option<(String, String)> {
        let team = self.world.team(self.team)?;
        let v = self.hub.get(&team.vault_id)?;
        if self.rng.chance(0.55) {
            return Some((team.vault_id, team.hot_id));
        }
        let g = v.try_lock().ok()?;
        let notes: Vec<String> = g
            .notes()
            .into_iter()
            .filter(|(p, _)| !p.starts_with("Quick notes/"))
            .map(|(_, e)| e.id)
            .collect();
        drop(g);
        if notes.is_empty() {
            return None;
        }
        Some((team.vault_id, self.rng.pick(&notes).clone()))
    }

    /// Type a sentence or two into one of the team's notes, a few
    /// characters at a time, the way people type.
    async fn type_some(&mut self, stop: &watch::Receiver<bool>) -> Result<()> {
        let Some((vault_id, note_id)) = self.team_note() else {
            return Ok(());
        };
        let sentence = world::sentence(&mut self.rng);
        let keys = world::keystrokes(&mut self.rng, &sentence);
        self.world
            .typing
            .lock()
            .unwrap()
            .entry(note_id.clone())
            .or_default()
            .insert(self.id);
        let mut first = true;
        let mut done = true;
        for k in keys {
            if *stop.borrow() {
                done = false;
                break;
            }
            let t0 = Instant::now();
            let res = self.insert(&vault_id, &note_id, &k, first).await;
            first = false;
            self.world.bus.action(
                self.id,
                Action::Type,
                Some(self.team),
                k.len(),
                t0.elapsed(),
                ok(&res),
            );
            if res.is_err() {
                done = false;
                break;
            }
            tokio::time::sleep(Duration::from_millis(self.rng.range(70, 280))).await;
        }
        if let Some(set) = self.world.typing.lock().unwrap().get_mut(&note_id) {
            set.remove(&self.id);
        }
        if done {
            self.world
                .sentences
                .lock()
                .unwrap()
                .entry((vault_id, note_id))
                .or_default()
                .push(sentence);
        }
        Ok(())
    }

    /// Insert at the end of this person's section of a note.
    async fn insert(
        &self,
        vault_id: &str,
        note_id: &str,
        text: &str,
        starts_sentence: bool,
    ) -> Result<()> {
        let v = self
            .hub
            .get(vault_id)
            .ok_or_else(|| anyhow!("vault not open"))?;
        let t0 = Instant::now();
        {
            let mut g = v.lock().await;
            let current = g.note_text(note_id);
            let new = insert_in_section(&current, &self.display(), text, starts_sentence);
            g.set_note_text(note_id, &new)?;
            g.save()?;
            let len = section_len(&new, &self.display());
            self.world
                .typed
                .lock()
                .unwrap()
                .entry(note_id.to_string())
                .or_default()
                .entry(self.id)
                .or_default()
                .push((len, t0));
        }
        self.hub
            .local_changed(vault_id, vec![note_id.to_string()], false)
            .await;
        Ok(())
    }

    async fn new_note(&mut self) -> Result<()> {
        let Some(team) = self.world.team(self.team) else {
            return Ok(());
        };
        self.notes_made += 1;
        let title = format!(
            "{} — {} {}",
            self.rng.pick(world::TITLES),
            self.display(),
            self.notes_made
        );
        let names: Vec<String> = team
            .members
            .iter()
            .map(|&m| self.world.display(m).to_string())
            .collect();
        let t0 = Instant::now();
        let res = ops::put_document(
            &self.hub,
            &team.vault_id,
            &format!("{title}.md"),
            &sectioned(&title, std::slice::from_ref(&team.hot_title), &names),
            None,
        )
        .await;
        self.world.bus.action(
            self.id,
            Action::Note,
            Some(self.team),
            title.len(),
            t0.elapsed(),
            ok(&res),
        );
        if ok(&res) {
            self.world.bus.feed(
                Action::Note,
                format!("{} started “{title}” in {}", self.display(), team.name),
            );
        }
        Ok(())
    }

    async fn picture(&mut self) -> Result<()> {
        let Some(team) = self.world.team(self.team) else {
            return Ok(());
        };
        self.pictures += 1;
        let kib = self.rng.range(20, 180) as u32;
        let data = world::picture(&mut self.rng, kib);
        let name = format!(
            "photo-{}-{}.png",
            self.world.names[self.id as usize].1, self.pictures
        );
        let t0 = Instant::now();
        let res = ops::put_attachment(
            &self.hub,
            &team.vault_id,
            &format!("Pictures/{name}"),
            &data,
            Some("image/png"),
        )
        .await;
        self.world.bus.action(
            self.id,
            Action::Picture,
            Some(self.team),
            data.len(),
            t0.elapsed(),
            ok(&res),
        );
        if ok(&res) {
            self.world.bus.feed(
                Action::Picture,
                format!(
                    "{} added a picture to {} ({} KB)",
                    self.display(),
                    team.name,
                    data.len() / 1000
                ),
            );
            // Show it in the shared note, in their own part.
            self.insert(
                &team.vault_id,
                &team.hot_id,
                &format!(" ![[{name}]]"),
                false,
            )
            .await?;
        }
        Ok(())
    }

    async fn quick_note(&mut self) -> Result<()> {
        let text = world::sentence(&mut self.rng);
        let t0 = Instant::now();
        let res = ops::create_quick_note(&self.hub, &self.personal, &text).await;
        self.world.bus.action(
            self.id,
            Action::Quick,
            None,
            text.len(),
            t0.elapsed(),
            ok(&res),
        );
        Ok(())
    }

    async fn rename(&mut self) -> Result<()> {
        let Some(team) = self.world.team(self.team) else {
            return Ok(());
        };
        let Some(v) = self.hub.get(&team.vault_id) else {
            return Ok(());
        };
        let candidates: Vec<String> = v
            .lock()
            .await
            .notes()
            .into_iter()
            .filter(|(_, e)| e.id != team.hot_id)
            .map(|(p, _)| p)
            .collect();
        if candidates.is_empty() {
            return Ok(());
        }
        let from = self.rng.pick(&candidates).clone();
        let to = format!(
            "{} {}.md",
            self.rng.pick(world::TITLES),
            self.rng.range(10, 999)
        );
        let t0 = Instant::now();
        let res = ops::move_document(&self.hub, &team.vault_id, &from, &to, true).await;
        self.world.bus.action(
            self.id,
            Action::Rename,
            Some(self.team),
            0,
            t0.elapsed(),
            ok(&res),
        );
        if ok(&res) {
            self.world.bus.feed(
                Action::Rename,
                format!(
                    "{} renamed “{}” to “{}” (links follow)",
                    self.display(),
                    from.trim_end_matches(".md"),
                    to.trim_end_matches(".md")
                ),
            );
        }
        Ok(())
    }

    /// Clear out an old quick note.
    async fn trash(&mut self) -> Result<()> {
        let Some(v) = self.hub.get(&self.personal) else {
            return Ok(());
        };
        let quick: Vec<String> = v
            .lock()
            .await
            .notes()
            .into_iter()
            .filter(|(p, _)| ops::is_quick_note(p))
            .map(|(p, _)| p)
            .collect();
        if quick.len() < 2 {
            return Ok(());
        }
        let path = self.rng.pick(&quick).clone();
        let t0 = Instant::now();
        let res = ops::delete_document(&self.hub, &self.personal, &path).await;
        self.world
            .bus
            .action(self.id, Action::Trash, None, 0, t0.elapsed(), ok(&res));
        Ok(())
    }

    /// Share one of one's own notes with someone from another team.
    async fn share_note(&mut self) -> Result<()> {
        let people = self.world.names.len() as u64;
        let other = self.rng.range(0, people - 1) as u32;
        if self.world.team_of[other as usize] == self.team {
            return Ok(());
        }
        let title = format!(
            "For {} {}",
            self.world.display(other),
            self.rng.range(1, 999)
        );
        let text = format!("# {title}\n\n{}\n", world::sentence(&mut self.rng));
        ops::put_document(
            &self.hub,
            &self.personal,
            &format!("{title}.md"),
            &text,
            None,
        )
        .await
        .map_err(|e| anyhow!("{e}"))?;
        let Some(v) = self.hub.get(&self.personal) else {
            return Ok(());
        };
        let login = self.world.names[other as usize].1.clone();
        let t0 = Instant::now();
        let res =
            note_shares::share_note(&self.session, &v, &title, &login, Role::Viewer, false).await;
        self.world
            .bus
            .action(self.id, Action::ShareNote, None, 0, t0.elapsed(), ok(&res));
        if ok(&res) {
            self.world.bus.feed(
                Action::ShareNote,
                format!(
                    "{} shared a note with {}",
                    self.display(),
                    self.world.display(other)
                ),
            );
        }
        Ok(())
    }

    /// Changes not uploaded yet.
    pub async fn queued(hub: &Hub) -> usize {
        let mut n = 0;
        for v in hub.vaults() {
            n += v.lock().await.replica.pending.len();
        }
        n
    }

    /// Go offline for good (end of the run).
    pub async fn leave(self) {
        let _ = self.live_stop.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(5), self.live).await;
        self.hub.save_all().await;
    }
}

/// Whether it worked (failures go to the log: `LOAD_LOG=coolpsonotes_load=warn`).
fn ok<T, E: std::fmt::Display>(r: &std::result::Result<T, E>) -> bool {
    if let Err(e) = r {
        tracing::warn!("{e:#}");
    }
    r.is_ok()
}

/// Report live changes received from others, and the connection state.
async fn watch_hub(
    world: Arc<World>,
    id: u32,
    hub: Arc<Hub>,
    mut ev: broadcast::Receiver<HubEvent>,
) {
    loop {
        match ev.recv().await {
            Ok(HubEvent::Connection { online }) => {
                world
                    .bus
                    .send(coolpsonotes_load_events::Event::Online { id, online });
            }
            Ok(HubEvent::Changed {
                vault_id,
                objects,
                local: false,
                ..
            }) => {
                let team = world.team_of_vault(&vault_id);
                let Some(v) = hub.get(&vault_id) else {
                    continue;
                };
                for o in objects {
                    let writers: Vec<u32> = match world.typed.lock().unwrap().get(&o) {
                        Some(w) => w.keys().copied().filter(|&w| w != id).collect(),
                        None => continue,
                    };
                    let text = v.lock().await.note_text(&o);
                    let now = Instant::now();
                    let mut arrived = Vec::new();
                    {
                        let typed = world.typed.lock().unwrap();
                        let mut credited = world.credited.lock().unwrap();
                        for w in writers {
                            let have = section_len(&text, world.display(w));
                            let log = &typed[&o][&w];
                            let next = credited.entry((id, o.clone(), w)).or_default();
                            while *next < log.len() && log[*next].0 <= have {
                                arrived.push((w, now.duration_since(log[*next].1)));
                                *next += 1;
                            }
                        }
                    }
                    for (from, latency) in arrived {
                        world.bus.delivered(id, Some(from), team, latency);
                    }
                }
            }
            Ok(_) => {}
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// Sleep, or return true early when told to stop.
pub async fn sleep_or_stop(d: Duration, stop: &watch::Receiver<bool>) -> bool {
    let mut rx = stop.clone();
    tokio::select! {
        _ = tokio::time::sleep(d) => *stop.borrow(),
        _ = rx.wait_for(|s| *s) => true,
    }
}

/// A note with a part for each person writing in it, so people typing at
/// the same time each write their own sentences (as people do in a shared
/// plan) — concurrent edits still merge into one text.
pub fn sectioned(title: &str, links: &[String], names: &[String]) -> String {
    let mut s = format!("# {title}\n\n");
    if !links.is_empty() {
        let links: Vec<String> = links.iter().map(|l| format!("[[{l}]]")).collect();
        s.push_str(&format!("See also {}.\n\n", links.join(", ")));
    }
    for n in names {
        s.push_str(&format!("## {n}\n\n"));
    }
    s
}

/// Where a person's next words go: the end of their part of the note (or
/// the end of the note if it has no part for them).
pub fn section_end(text: &str, name: &str) -> usize {
    let heading = format!("## {name}\n");
    let Some(start) = text
        .match_indices(&heading)
        .find(|(i, _)| *i == 0 || text.as_bytes()[i - 1] == b'\n')
        .map(|(i, _)| i)
    else {
        return text.trim_end().len();
    };
    let body = start + heading.len();
    let end = text[body..]
        .find("\n## ")
        .map(|i| body + i)
        .unwrap_or(text.len());
    let mut e = end;
    while e > body && text.as_bytes()[e - 1] == b'\n' {
        e -= 1;
    }
    e
}

/// How long `name`'s part of a note is (0 if it has none).
pub fn section_len(text: &str, name: &str) -> usize {
    let heading = format!("## {name}\n");
    match text
        .match_indices(&heading)
        .find(|(i, _)| *i == 0 || text.as_bytes()[i - 1] == b'\n')
    {
        Some((start, _)) => section_end(text, name).saturating_sub(start + heading.len()),
        None => 0,
    }
}

/// `text` with `words` added at the end of `name`'s part.
pub fn insert_in_section(text: &str, name: &str, words: &str, starts_sentence: bool) -> String {
    let at = section_end(text, name);
    let (before, after) = text.split_at(at);
    let sep = if starts_sentence && !before.is_empty() && !before.ends_with('\n') {
        " "
    } else {
        ""
    };
    // A part's first words: keep a blank line before the next heading.
    let pad = if before.ends_with('\n') && after.starts_with("\n## ") {
        "\n"
    } else {
        ""
    };
    format!("{before}{sep}{words}{pad}{after}")
}

/// Line and column (in characters) of a byte offset.
pub fn line_col(text: &str, at: usize) -> (u32, u32) {
    let before = &text[..at.min(text.len())];
    let line = before.matches('\n').count() as u32;
    let col = before.rsplit('\n').next().unwrap_or("").chars().count() as u32;
    (line, col)
}

/// Remove what a run left in `root` (people's local copies).
pub fn clean(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_person_writes_in_their_own_part() {
        let names = vec!["Maya".to_string(), "Leo".to_string()];
        let mut t = sectioned("Plan", &["Ideas 1".into()], &names);
        for (who, words, starts) in [
            ("Maya", "Hel", true),
            ("Leo", "Hi.", true),
            ("Maya", "lo.", false),
            ("Maya", "More.", true),
            ("Leo", "Bye.", true),
        ] {
            t = insert_in_section(&t, who, words, starts);
        }
        assert_eq!(
            t,
            "# Plan\n\nSee also [[Ideas 1]].\n\n## Maya\nHello. More.\n\n## Leo\nHi. Bye.\n"
        );
        assert_eq!(section_end("no parts here\n\n", "Maya"), 13);
        assert_eq!(section_len(&t, "Maya"), "Hello. More.".len());
        assert_eq!(section_len(&t, "Leo"), "Hi. Bye.".len());
        assert_eq!(section_len(&t, "Nobody"), 0);
    }

    #[test]
    fn positions() {
        assert_eq!(line_col("ab\ncdé\nf", 7), (1, 3));
        assert_eq!(line_col("ab", 0), (0, 0));
    }
}
