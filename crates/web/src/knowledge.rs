//! Knowledge navigation over decrypted notes: link resolution, backlinks,
//! tags, outline and full-text search. Runs entirely in the browser.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use notekpr_core::doc::EntryKind;
use notekpr_core::markdown::{self, Link, LinkKind, NoteInfo};
use notekpr_core::{paths, search};

use leptos::prelude::*;

use crate::state::{self, step};

thread_local! {
    static ANALYSIS: RefCell<HashMap<(String, String), (u64, Rc<NoteInfo>)>> = RefCell::new(HashMap::new());
}

fn hash(s: &str) -> u64 {
    // FNV-1a: only used to detect changed text for cache invalidation.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h ^ (s.len() as u64)
}

pub fn analysis(vault: &str, note: &str, text: &str) -> Rc<NoteInfo> {
    let key = (vault.to_string(), note.to_string());
    let h = hash(text);
    ANALYSIS.with(|a| {
        if let Some((hh, info)) = a.borrow().get(&key)
            && *hh == h
        {
            return info.clone();
        }
        let info = Rc::new(markdown::analyze(text));
        a.borrow_mut().insert(key, (h, info.clone()));
        info
    })
}

/// All live notes `(id, path, text)` of a vault.
pub fn notes_with_text(vault: &str) -> Vec<(String, String, String)> {
    step(|c| {
        let Some(r) = c.replicas.get(vault) else {
            return vec![];
        };
        c.live_entries(vault)
            .into_iter()
            .filter(|e| e.kind == EntryKind::Note)
            .map(|e| {
                let t = r.note_text(&e.id);
                (e.id, e.path, t)
            })
            .collect()
    })
}

fn candidates(vault: &str) -> Vec<(String, String)> {
    state::step(|c| c.live_entries(vault))
        .into_iter()
        .map(|e| (e.id, e.path))
        .collect()
}

/// Resolve a wiki-link style target (note title or path) to an entry id.
pub fn resolve_target(vault: &str, target: &str, from_path: &str) -> Option<String> {
    let link = Link {
        kind: LinkKind::Wiki,
        target: target.to_string(),
        heading: None,
        alias: None,
        embed: false,
        start: 0,
        end: 0,
    };
    let c = candidates(vault);
    markdown::resolve(&link, from_path, &c).map(str::to_string)
}

/// Like [`resolve_target`] but for an embed: also matches attachment file names.
pub fn resolve_attachment(vault: &str, target: &str) -> Option<String> {
    let entries = state::step(|c| c.live_entries(vault));
    let t = target.trim().trim_start_matches("./").to_lowercase();
    let exact = entries
        .iter()
        .find(|e| e.kind == EntryKind::Attachment && e.path.to_lowercase() == t);
    let by_name = || {
        entries
            .iter()
            .filter(|e| {
                e.kind == EntryKind::Attachment
                    && paths::file_name(&e.path).to_lowercase() == paths::file_name(&t)
            })
            .min_by_key(|e| e.path.len())
    };
    exact.or_else(by_name).map(|e| e.id.clone())
}

/// Fast existence check used while rendering the editor (cached per tree revision).
pub struct LinkIndex {
    rev: u64,
    vault: String,
    keys: std::collections::HashSet<String>,
}

thread_local! {
    static LINKS: RefCell<Option<LinkIndex>> = const { RefCell::new(None) };
}

pub fn link_exists(vault: &str, target: &str) -> bool {
    let rev = state::ui().tree_rev.get_untracked();
    let key = {
        let t = target.trim().trim_start_matches("./").to_lowercase();
        let t = t.split('#').next().unwrap_or("").to_string();
        if paths::is_note(&t) {
            t[..t.len() - 3].to_string()
        } else {
            t
        }
    };
    if key.is_empty() {
        return true;
    }
    LINKS.with(|l| {
        let stale = l
            .borrow()
            .as_ref()
            .is_none_or(|x| x.rev != rev || x.vault != vault);
        if stale {
            let mut keys = std::collections::HashSet::new();
            for (_, p) in candidates(vault) {
                let lp = p.to_lowercase();
                let lp = if paths::is_note(&lp) {
                    lp[..lp.len() - 3].to_string()
                } else {
                    lp
                };
                let mut cur = lp.as_str();
                keys.insert(cur.to_string());
                while let Some(i) = cur.find('/') {
                    cur = &cur[i + 1..];
                    keys.insert(cur.to_string());
                }
            }
            *l.borrow_mut() = Some(LinkIndex {
                rev,
                vault: vault.to_string(),
                keys,
            });
        }
        l.borrow().as_ref().is_some_and(|x| x.keys.contains(&key))
    })
}

/// Forget everything derived from decrypted notes (on sign-out).
pub fn clear_caches() {
    ANALYSIS.with(|a| a.borrow_mut().clear());
    LINKS.with(|l| *l.borrow_mut() = None);
}

/// The vault's notes and the links between them, for the graph view.
pub fn graph(vault: &str) -> crate::graph::Graph {
    let (notes, entries) = step(|c| {
        let Some(r) = c.replicas.get(vault) else {
            return (vec![], vec![]);
        };
        let live = c.live_entries(vault);
        let notes: Vec<_> = live
            .iter()
            .filter(|e| e.kind == EntryKind::Note)
            .map(|e| {
                (
                    e.id.clone(),
                    e.path.clone(),
                    e.icon.clone(),
                    r.note_text(&e.id),
                )
            })
            .collect();
        let entries = live.into_iter().map(|e| (e.id, e.path)).collect::<Vec<_>>();
        (notes, entries)
    });
    let notes = notes
        .into_iter()
        .map(|(id, path, icon, text)| crate::graph::NoteLinks {
            links: analysis(vault, &id, &text).links.clone(),
            id,
            path,
            icon,
        })
        .collect();
    crate::graph::Graph::build(notes, &entries)
}

pub struct Backlink {
    pub id: String,
    pub path: String,
    pub snippet: String,
}

pub fn backlinks(vault: &str, note: &str) -> Vec<Backlink> {
    let notes = notes_with_text(vault);
    let c = candidates(vault);
    let mut out = Vec::new();
    for (id, path, text) in &notes {
        if id == note {
            continue;
        }
        let info = analysis(vault, id, text);
        for l in &info.links {
            if markdown::resolve(l, path, &c) == Some(note) {
                let line_start = text[..l.start].rfind('\n').map(|i| i + 1).unwrap_or(0);
                let line_end = text[l.end..]
                    .find('\n')
                    .map(|i| l.end + i)
                    .unwrap_or(text.len());
                // As shown: without HTML tags (colors).
                let snippet: String = markdown::shown_text(&text[line_start..line_end])
                    .chars()
                    .take(160)
                    .collect();
                out.push(Backlink {
                    id: id.clone(),
                    path: path.clone(),
                    snippet,
                });
                break;
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Links in a note that point to nothing (broken-link detection).
pub fn broken_links(vault: &str, note: &str, path: &str, text: &str) -> Vec<String> {
    let info = analysis(vault, note, text);
    let c = candidates(vault);
    let mut out: Vec<String> = info
        .links
        .iter()
        .filter(|l| !l.target.is_empty() && markdown::resolve(l, path, &c).is_none())
        .map(|l| l.target.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

pub fn outgoing(vault: &str, note: &str, path: &str, text: &str) -> Vec<(String, String)> {
    let info = analysis(vault, note, text);
    let c = candidates(vault);
    let mut out: Vec<(String, String)> = info
        .links
        .iter()
        .filter_map(|l| markdown::resolve(l, path, &c))
        .filter(|id| *id != note)
        .filter_map(|id| c.iter().find(|(i, _)| i == id).cloned())
        .collect();
    out.sort();
    out.dedup();
    out
}

pub fn tags(vault: &str) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (id, _, text) in notes_with_text(vault) {
        for t in &analysis(vault, &id, &text).tags {
            *counts.entry(t.clone()).or_default() += 1;
        }
    }
    let mut v: Vec<(String, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

pub fn outline(vault: &str, note: &str, text: &str) -> Vec<(u8, String, usize)> {
    analysis(vault, note, text)
        .headings
        .iter()
        .map(|h| (h.level, h.text.clone(), h.offset))
        .collect()
}

pub struct Hit {
    pub vault: String,
    pub vault_name: String,
    pub id: String,
    pub path: String,
    pub score: f32,
    pub snippet: String,
    pub highlights: Vec<(usize, usize)>,
}

/// Full-text search across all locally decrypted vaults.
pub fn search_all(query: &str, only_vault: Option<&str>) -> Vec<Hit> {
    let q = search::parse_query(query);
    if q.is_empty() {
        return vec![];
    }
    let vaults: Vec<(String, String)> = step(|c| {
        c.replicas
            .iter()
            .filter(|(_, r)| r.member)
            .map(|(id, r)| (id.clone(), r.meta.name.clone()))
            .collect()
    });
    let mut hits = Vec::new();
    for (vid, vname) in vaults {
        if only_vault.is_some_and(|v| v != vid) {
            continue;
        }
        if let Some(want) = &q.vault
            && !vname.to_lowercase().contains(want)
        {
            continue;
        }
        for (id, path, text) in notes_with_text(&vid) {
            let info = analysis(&vid, &id, &text);
            if let Some(h) = search::score(&q, &path, &text, &info.tags) {
                hits.push(Hit {
                    vault: vid.clone(),
                    vault_name: vname.clone(),
                    id,
                    path,
                    score: h.score,
                    snippet: h.snippet,
                    highlights: h.highlights,
                });
            }
        }
    }
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(100);
    hits
}

/// Fuzzy title matching for the quick switcher.
pub fn quick(query: &str) -> Vec<(String, String, String, String)> {
    let q: Vec<char> = query
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let (vaults, recent) = step(|c| {
        let vaults: Vec<_> = c
            .replicas
            .iter()
            .filter(|(_, r)| r.member)
            .map(|(id, r)| (id.clone(), r.meta.name.clone(), c.live_entries(id)))
            .collect();
        (vaults, c.prefs.recent.clone())
    });
    let mut scored = Vec::new();
    for (vid, vname, entries) in &vaults {
        for e in entries {
            if e.kind != EntryKind::Note {
                continue;
            }
            let score = if q.is_empty() {
                recent
                    .iter()
                    .position(|r| r.0 == *vid && r.1 == e.id)
                    .map(|p| 1000 - p as i64)
                    .unwrap_or(0)
            } else {
                let Some(s) = fuzzy(&q, &e.path, e.title()) else {
                    continue;
                };
                s + if recent.iter().any(|r| r.0 == *vid && r.1 == e.id) {
                    5
                } else {
                    0
                }
            };
            scored.push((
                score,
                vid.clone(),
                e.id.clone(),
                e.path.clone(),
                vname.clone(),
            ));
        }
    }
    // Notes shared with us individually.
    for s in state::ui().shared.get_untracked() {
        let score = if q.is_empty() {
            0
        } else {
            let Some(x) = fuzzy(&q, &s.title, &s.title) else {
                continue;
            };
            x
        };
        scored.push((
            score,
            s.vault,
            s.note,
            s.title,
            format!("Shared by {}", s.owner),
        ));
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.3.len().cmp(&b.3.len())));
    scored
        .into_iter()
        .take(50)
        .map(|(_, v, id, p, vn)| (v, id, p, vn))
        .collect()
}

/// Subsequence match of `q` in `hay`, with bonuses for contiguous runs and
/// hits in the title.
fn fuzzy(q: &[char], hay: &str, title: &str) -> Option<i64> {
    let hay: Vec<char> = hay.to_lowercase().chars().collect();
    let title = title.to_lowercase();
    let mut s = 0i64;
    let mut hi = 0;
    let mut run = 0;
    for qc in q {
        let p = hay[hi..].iter().position(|c| c == qc)?;
        run = if p == 0 { run + 1 } else { 0 };
        s += 1 + run * 3 - (p as i64).min(5);
        hi += p + 1;
    }
    let qs: String = q.iter().collect();
    if title.starts_with(&qs) {
        s += 40;
    } else if title.contains(&qs) {
        s += 20;
    }
    Some(s)
}
