//! The end of the run: did every change reach everyone, and is every copy
//! of every note the same?

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use coolpsonotes_client::hub::Hub;

use crate::people::{Person, World};

#[derive(Debug, Default, Clone, Copy)]
pub struct Outcome {
    pub notes_checked: u32,
    pub copies_checked: u32,
    pub mismatches: u32,
    pub sentences: u32,
    pub sentences_missing: u32,
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
    // (vault, note) → each person's text.
    let mut copies: HashMap<(String, String), Vec<String>> = HashMap::new();
    for (_, h) in &hubs {
        for v in h.vaults() {
            let g = v.lock().await;
            let vid = g.id().to_string();
            for (_, e) in g.notes() {
                copies
                    .entry((vid.clone(), e.id.clone()))
                    .or_default()
                    .push(g.note_text(&e.id));
            }
        }
    }
    let mut out = Outcome::default();
    for texts in copies.values().filter(|t| t.len() > 1) {
        out.notes_checked += 1;
        out.copies_checked += texts.len() as u32;
        let mut count: HashMap<blake3::Hash, u32> = HashMap::new();
        for t in texts {
            *count.entry(blake3::hash(t.as_bytes())).or_default() += 1;
        }
        let majority = count.values().copied().max().unwrap_or(0);
        out.mismatches += texts.len() as u32 - majority;
    }
    let sentences = world.sentences.lock().unwrap().clone();
    for (key, said) in sentences {
        let Some(text) = copies.get(&key).and_then(|t| t.first()) else {
            out.sentences += said.len() as u32;
            out.sentences_missing += said.len() as u32;
            continue;
        };
        let mut wanted: HashMap<&str, u32> = HashMap::new();
        for s in &said {
            *wanted.entry(s.as_str()).or_default() += 1;
        }
        for (s, n) in wanted {
            out.sentences += n;
            out.sentences_missing += n.saturating_sub(text.matches(s).count() as u32);
        }
    }
    out
}
