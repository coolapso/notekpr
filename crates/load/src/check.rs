//! The end of the run: did every change reach everyone, and is every copy
//! of every note the same?

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use notekpr_cli::hub::Hub;

use crate::people::{Person, World};

#[derive(Debug, Default, Clone, Copy)]
pub struct Outcome {
    pub notes_checked: u32,
    pub copies_checked: u32,
    pub mismatches: u32,
    pub sentences: u32,
    pub sentences_missing: u32,
    /// Changes still waiting on people's devices when copies were compared.
    pub waiting: u32,
    /// Finished sentences in their writer's copy, not uploaded yet.
    pub sentences_waiting: u32,
}

fn hubs(world: &World) -> Vec<(u32, Arc<Hub>)> {
    world
        .clients
        .lock()
        .unwrap()
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.as_ref().map(|(_, h)| (i as u32, h.clone())))
        .collect()
}

/// Wait until everyone is online with nothing left to upload. Returns how
/// long it took, or `None` on timeout.
pub async fn settle(world: &World, timeout: Duration) -> Option<Duration> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let mut quiet = true;
        for (_, h) in hubs(world) {
            if !h.is_online() || Person::queued(&h).await > 0 {
                quiet = false;
                break;
            }
        }
        if quiet {
            return Some(start.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    None
}

/// Changes waiting on everyone's devices right now.
pub async fn queued(world: &World) -> usize {
    let mut n = 0;
    for (_, h) in hubs(world) {
        n += Person::queued(&h).await;
    }
    n
}

/// After an outage: how long until everyone is connected again, and until
/// the backlog is worked off (no more changes waiting than `baseline`, what
/// was normal before). Either may not happen within `timeout`.
pub async fn recover(
    world: &World,
    baseline: usize,
    timeout: Duration,
) -> (Option<Duration>, Option<Duration>) {
    let start = Instant::now();
    let limit = baseline + baseline / 5 + 20;
    let (mut online, mut caught_up) = (None, None);
    while start.elapsed() < timeout && caught_up.is_none() {
        let hubs = hubs(world);
        if online.is_none() && hubs.iter().all(|(_, h)| h.is_online()) {
            online = Some(start.elapsed());
        }
        if online.is_some() && queued(world).await <= limit {
            caught_up = Some(start.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    (online, caught_up)
}

/// Compare every copy of every note people share.
pub async fn run(world: &World) -> Outcome {
    let hubs = hubs(world);
    // Twice: the first round's uploads reach the others in the second.
    for _ in 0..2 {
        for (_, h) in &hubs {
            for id in h.vault_ids() {
                let _ = h.sync_vault(&id).await;
            }
        }
    }
    // (vault, note) → one copy's text (for the sentences) and how many
    // copies have each content (a hash each, not every text).
    #[derive(Default)]
    struct Copies {
        text: Option<String>,
        count: HashMap<blake3::Hash, u32>,
        n: u32,
    }
    let waiting = queued(world).await as u32;
    let mut copies: HashMap<(String, String), Copies> = HashMap::new();
    for (_, h) in &hubs {
        for v in h.vaults() {
            let g = v.lock().await;
            let vid = g.id().to_string();
            for (_, e) in g.notes() {
                let text = g.note_text(&e.id);
                let c = copies.entry((vid.clone(), e.id.clone())).or_default();
                *c.count.entry(blake3::hash(text.as_bytes())).or_default() += 1;
                c.n += 1;
                if c.text.is_none() {
                    c.text = Some(text);
                }
            }
        }
    }
    let mut out = Outcome {
        waiting,
        ..Default::default()
    };
    for c in copies.values().filter(|c| c.n > 1) {
        out.notes_checked += 1;
        out.copies_checked += c.n;
        let majority = c.count.values().copied().max().unwrap_or(0);
        out.mismatches += c.n - majority;
    }
    let sentences = world.sentences.lock().unwrap().clone();
    let mut lost: Vec<(String, String, String, Vec<u32>)> = Vec::new();
    for (key, said) in sentences {
        let Some(text) = copies.get(&key).and_then(|c| c.text.as_ref()) else {
            out.sentences += said.len() as u32;
            out.sentences_missing += said.len() as u32;
            continue;
        };
        let mut wanted: HashMap<&str, (u32, Vec<u32>, Vec<String>)> = HashMap::new();
        for (s, who, at, phase) in &said {
            let w = wanted.entry(s.as_str()).or_default();
            w.0 += 1;
            w.1.push(*who);
            w.2.push(format!("{:.1} s, {phase}", *at as f64 / 1000.0));
        }
        for (s, (n, who, when)) in wanted {
            out.sentences += n;
            let missing = n.saturating_sub(text.matches(s).count() as u32);
            if missing > 0 && on_writers_device(&hubs, &key.0, &key.1, s, &who).await {
                out.sentences_waiting += missing;
            } else if missing > 0 {
                out.sentences_missing += missing;
                eprintln!("  typed at: {}", when.join("; "));
                report_missing(world, &key.1, s, &who, text);
                lost.push((key.0.clone(), key.1.clone(), s.to_string(), who.clone()));
            }
        }
    }
    for (vault, note, sentence, who) in lost {
        explain(world, &hubs, &vault, &note, &sentence, &who).await;
        if let Some(&w) = who.first() {
            replay(world, w, &vault, &note, &sentence).await;
        }
    }
    out
}

/// Whether a writer's own copy has the sentence while changes to the note
/// still wait on their device: not on the server yet, but not lost.
async fn on_writers_device(
    hubs: &[(u32, Arc<Hub>)],
    vault: &str,
    note: &str,
    sentence: &str,
    who: &[u32],
) -> bool {
    for (id, h) in hubs.iter().filter(|(id, _)| who.contains(id)) {
        let Some(v) = h.get(vault) else {
            continue;
        };
        let g = v.lock().await;
        let pending = g.replica.pending.iter().any(|p| p.object_id == note)
            || g.outbox.held.iter().any(|p| p.object_id == note);
        if pending && g.note_text(note).contains(sentence) {
            tracing::debug!("{}'s sentence is still on their device", id);
            return true;
        }
    }
    false
}

/// What each client knows of a note that lost a sentence.
async fn explain(
    world: &World,
    hubs: &[(u32, Arc<Hub>)],
    vault: &str,
    note: &str,
    sentence: &str,
    who: &[u32],
) {
    for (id, h) in hubs {
        let Some(v) = h.get(vault) else {
            continue;
        };
        let g = v.lock().await;
        let entry = g.entry(note);
        let has = g.note_text(note).contains(sentence);
        let pending = g
            .replica
            .pending
            .iter()
            .filter(|p| p.object_id == note)
            .count();
        let held = g.outbox.held.iter().filter(|p| p.object_id == note).count();
        let deferred = g.replica.deferred.len();
        let unverified = g.replica.unverified.len();
        let dropped = g
            .replica
            .dropped
            .values()
            .filter(|o| o.as_str() == note)
            .count();
        let writer = if who.contains(id) { " (writer)" } else { "" };
        eprintln!(
            "  {}{writer}: in index {} (path {:?}, live {}), has it {has}, pending {pending}, held {held}, deferred {deferred}, unverified {unverified}, dropped {dropped}, doc key {}",
            world.display(*id),
            entry.is_some(),
            entry.as_ref().map(|e| e.path.clone()),
            entry.as_ref().is_some_and(|e| e.is_live()),
            g.replica.doc_keys.contains_key(note),
        );
    }
}

/// Say what is missing where, as precisely as possible (for finding out
/// whether it's the product or the test).
fn report_missing(world: &World, note: &str, sentence: &str, who: &[u32], text: &str) {
    let chars: Vec<char> = sentence.chars().collect();
    let found = (0..=chars.len())
        .rev()
        .find(|&n| text.contains(&chars[..n].iter().collect::<String>()))
        .unwrap_or(0);
    let prefix: String = chars[..found].iter().collect();
    let context = text
        .find(&prefix)
        .filter(|_| found > 0)
        .map(|i| {
            let end = (i + prefix.len() + 60).min(text.len());
            let end = (end..=text.len())
                .find(|&e| text.is_char_boundary(e))
                .unwrap_or(text.len());
            text[i..end].replace('\n', "⏎")
        })
        .unwrap_or_default();
    // And from the end: a sentence split in two (its start typed elsewhere)
    // still has its tail.
    let tail = (0..=chars.len())
        .rev()
        .find(|&n| text.contains(&chars[chars.len() - n..].iter().collect::<String>()))
        .unwrap_or(0);
    let names: Vec<&str> = who.iter().map(|&p| world.display(p)).collect();
    eprintln!(
        "MISSING in note {note} (typed by {}): {sentence:?}\n  longest start found: {found}/{} chars; there: {context:?}; longest end found: {tail} chars",
        names.join(", "),
        chars.len()
    );
}

/// Replay a note's changes as the server stores them (decrypted with the
/// writer's keys): when did the sentence appear, and whose change took it
/// away?
async fn replay(world: &World, writer: u32, vault: &str, note: &str, sentence: &str) {
    use notekpr_core::crypto::Purpose;
    use notekpr_core::doc::NoteDoc;
    let Some((session, hub)) = world.clients.lock().unwrap()[writer as usize].clone() else {
        return;
    };
    let Some(v) = hub.get(vault) else {
        return;
    };
    let names: HashMap<String, String> = world
        .clients
        .lock()
        .unwrap()
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            c.as_ref()
                .map(|(s, _)| (s.user_id.clone(), world.display(i as u32).to_string()))
        })
        .collect();
    let doc = NoteDoc::new();
    let (mut since, mut had, mut first, mut count) = (0, false, None, 0);
    let mut first_page = true;
    loop {
        // The first page starts from the stored snapshots (what was
        // compacted away is in them).
        let Ok(ch) = session.api.changes(vault, since, 1000, first_page).await else {
            eprintln!("    replay: cannot fetch changes");
            return;
        };
        for s in ch.snapshots.iter().filter(|s| s.object_id == note) {
            let plain = v
                .lock()
                .await
                .replica
                .decrypt_object(Purpose::Snapshot, note, &s.payload);
            match plain {
                Ok(p) => {
                    let _ = doc.apply_update(&p);
                    had = doc.text().contains(sentence);
                    let who = names.get(&s.author_id).map(String::as_str).unwrap_or("?");
                    eprintln!(
                        "    snapshot covering seq {} by {who}: the sentence is {}in it",
                        s.covers_seq,
                        if had { "" } else { "not " }
                    );
                }
                Err(e) => eprintln!("    snapshot: cannot decrypt: {e}"),
            }
        }
        first_page = false;
        for u in ch.updates.iter().filter(|u| u.object_id == note) {
            first.get_or_insert(u.seq);
            count += 1;
            let plain = v
                .lock()
                .await
                .replica
                .decrypt_object(Purpose::Update, note, &u.payload);
            match plain {
                Ok(p) => {
                    if let Err(e) = doc.apply_update(&p) {
                        eprintln!("    seq {}: does not apply: {e}", u.seq);
                    }
                }
                Err(e) => {
                    eprintln!("    seq {}: cannot decrypt: {e}", u.seq);
                    continue;
                }
            }
            let has = doc.text().contains(sentence);
            if has != had {
                let who = names.get(&u.author_id).map(String::as_str).unwrap_or("?");
                eprintln!(
                    "    seq {} by {who}: the sentence {}",
                    u.seq,
                    if has { "appears" } else { "DISAPPEARS" }
                );
                had = has;
            }
        }
        since = ch.next_since;
        if !ch.has_more {
            break;
        }
    }
    eprintln!(
        "    replayed {count} stored changes from seq {:?}: the sentence is {}there at the end",
        first,
        if had { "" } else { "not " }
    );
}
