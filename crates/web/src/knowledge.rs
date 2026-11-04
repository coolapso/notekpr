//! Knowledge navigation over decrypted notes: link resolution, backlinks,
//! tags, outline and full-text search. Runs entirely in the browser.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use notekpr_core::doc::{Entry, EntryKind};
use notekpr_core::markdown::{self, Link, LinkKind, NoteInfo};
use notekpr_core::{paths, search};

use leptos::prelude::*;

use crate::state::{self, step};

thread_local! {
    /// Per note: its analysis, and the text and revision it was made from.
    static ANALYSIS: RefCell<HashMap<(String, String), Analysed>> = RefCell::new(HashMap::new());
    /// Per vault: what its links resolve to, and the index revision that
    /// was made from.
    static RESOLVERS: RefCell<HashMap<String, (u64, Rc<markdown::Resolver>)>> =
        RefCell::new(HashMap::new());
}

struct Analysed {
    /// The note's revision (`NoteDoc::revision`); 0 when the text came from
    /// elsewhere.
    rev: u64,
    hash: u64,
    info: Rc<NoteInfo>,
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
    analysed(vault, note, 0, text)
}

/// The analysis of `text`, the note's text at revision `rev` (0: unknown).
fn analysed(vault: &str, note: &str, rev: u64, text: &str) -> Rc<NoteInfo> {
    let key = (vault.to_string(), note.to_string());
    let h = hash(text);
    ANALYSIS.with(|a| {
        let mut a = a.borrow_mut();
        if let Some(x) = a.get_mut(&key)
            && x.hash == h
        {
            if rev != 0 {
                x.rev = rev;
            }
            return x.info.clone();
        }
        let info = Rc::new(markdown::analyze(text));
        a.insert(
            key,
            Analysed {
                rev,
                hash: h,
                info: info.clone(),
            },
        );
        info
    })
}

/// A note's analysis. Its text is read only if the note changed since it
/// was last analysed: whole-vault passes (backlinks, the graph, tags) run
/// on every change that arrives, and most notes haven't changed.
fn analysis_of(c: &state::Core, vault: &str, note: &str) -> Rc<NoteInfo> {
    let doc = c.replicas.get(vault).and_then(|r| r.notes.get(note));
    let rev = doc.map_or(0, |d| d.revision());
    if rev != 0 {
        let key = (vault.to_string(), note.to_string());
        let hit = ANALYSIS.with(|a| {
            a.borrow()
                .get(&key)
                .filter(|x| x.rev == rev)
                .map(|x| x.info.clone())
        });
        if let Some(info) = hit {
            return info;
        }
    }
    let text = doc.map(|d| d.text()).unwrap_or_default();
    analysed(vault, note, rev, &text)
}

/// The vault's notes, `(id, path, analysis)`.
fn analysed_notes(vault: &str) -> Vec<(String, String, Rc<NoteInfo>)> {
    step(|c| {
        c.live_entries(vault)
            .into_iter()
            .filter(|e| e.kind == EntryKind::Note)
            .map(|e| {
                let info = analysis_of(c, vault, &e.id);
                (e.id, e.path, info)
            })
            .collect()
    })
}

/// What links in the vault resolve to, made again when its index changed.
fn resolver(vault: &str) -> Rc<markdown::Resolver> {
    step(|c| {
        let rev = c.replicas.get(vault).map(|r| r.index.revision());
        let cached = RESOLVERS.with(|m| {
            m.borrow()
                .get(vault)
                .filter(|(at, _)| Some(*at) == rev)
                .map(|(_, r)| r.clone())
        });
        if let Some(r) = cached {
            return r;
        }
        let r = Rc::new(markdown::Resolver::new(
            c.live_entries(vault)
                .into_iter()
                .map(|e| (e.id, e.path))
                .collect(),
        ));
        if let Some(rev) = rev {
            RESOLVERS.with(|m| m.borrow_mut().insert(vault.to_string(), (rev, r.clone())));
        }
        r
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

/// Notes and drawings with what search sees of them: (id, path, text).
fn searchable(vault: &str) -> Vec<(String, String, String)> {
    step(|c| {
        let Some(r) = c.replicas.get(vault) else {
            return vec![];
        };
        c.live_entries(vault)
            .into_iter()
            .filter(|e| matches!(e.kind, EntryKind::Note | EntryKind::Drawing))
            .map(|e| {
                let t = r.searchable_text(&e.id);
                (e.id, e.path, t)
            })
            .collect()
    })
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
    resolver(vault)
        .resolve(&link, from_path)
        .map(str::to_string)
}

/// The shortest wiki link target that finds the entry `id` from the note
/// at `from`: its name, else its path.
pub fn link_target(vault: &str, id: &str, from: &str) -> Option<String> {
    let links = resolver(vault);
    let (_, path) = links.candidates().iter().find(|(i, _)| i == id)?;
    Some(notekpr_core::attachments::link_target(
        &links, id, path, from,
    ))
}

/// What an embed shows (`![[…]]`): a file or a drawing — another app's
/// drawing file too, Obsidian's linked without its `.md` — by path or by
/// file name (the shortest path wins).
pub fn resolve_embed(vault: &str, target: &str) -> Option<String> {
    state::step(|c| resolve_embed_in(c, vault, target))
}

fn resolve_embed_in(c: &state::Core, vault: &str, target: &str) -> Option<String> {
    let entries = c.live_entries(vault);
    let t = target.trim().trim_start_matches("./").to_lowercase();
    let embeddable = |e: &Entry| {
        matches!(e.kind, EntryKind::Attachment | EntryKind::Drawing)
            || crate::drawings::is_other_apps_entry(c, vault, e)
    };
    let same = |path: &str, t: &str| {
        let p = path.to_lowercase();
        p == t || p.strip_suffix(".md") == Some(t)
    };
    let find = |t: &str, ok: &dyn Fn(&Entry) -> bool| {
        let exact = entries.iter().find(|e| same(&e.path, t) && ok(e));
        let by_name = || {
            entries
                .iter()
                .filter(|e| same(paths::file_name(&e.path), paths::file_name(t)) && ok(e))
                .min_by_key(|e| e.path.len())
        };
        exact.or_else(by_name).map(|e| e.id.clone())
    };
    // Notes from Obsidian embed `X.excalidraw` (or `X`): made a drawing
    // (imported, say), it is `X.drawing`.
    find(&t, &embeddable).or_else(|| {
        let ours = paths::drawing_link_target(&t)?;
        find(&ours, &|e| e.kind == EntryKind::Drawing)
    })
}

/// Whether a wiki link to `target` finds something (while rendering the
/// editor).
pub fn link_exists(vault: &str, target: &str) -> bool {
    let key = {
        let t = target.trim().trim_start_matches("./").to_lowercase();
        let t = t.split('#').next().unwrap_or("").to_string();
        if paths::is_note(&t) {
            t[..t.len() - 3].to_string()
        } else {
            t
        }
    };
    key.is_empty() || resolver(vault).knows(&key)
}

/// Forget everything derived from decrypted notes (on sign-out).
pub fn clear_caches() {
    ANALYSIS.with(|a| a.borrow_mut().clear());
    RESOLVERS.with(|r| r.borrow_mut().clear());
}

/// The vault's notes and drawings and the links between them, for the
/// graph view.
pub fn graph(vault: &str) -> crate::graph::Graph {
    let links = resolver(vault);
    let notes = step(|c| {
        let Some(r) = c.replicas.get(vault) else {
            return vec![];
        };
        c.live_entries(vault)
            .into_iter()
            .filter(|e| matches!(e.kind, EntryKind::Note | EntryKind::Drawing))
            .map(|e| {
                let drawing = e.kind == EntryKind::Drawing;
                // A drawing's links, as text (`[[…]]` each).
                let links = if drawing {
                    r.notes
                        .get(&e.id)
                        .map(|d| crate::graph::drawing_links(&d.drawing().scene()))
                        .unwrap_or_default()
                } else {
                    analysis_of(c, vault, &e.id).links.clone()
                };
                crate::graph::NoteLinks {
                    drawing,
                    links,
                    id: e.id,
                    path: e.path,
                    icon: e.icon,
                }
            })
            .collect()
    });
    crate::graph::Graph::build(notes, &links)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Backlink {
    pub id: String,
    pub path: String,
    pub snippet: String,
}

pub fn backlinks(vault: &str, note: &str) -> Vec<Backlink> {
    let links = resolver(vault);
    let mut out = Vec::new();
    for (id, path, info) in analysed_notes(vault) {
        if id == note {
            continue;
        }
        let Some(l) = info
            .links
            .iter()
            .find(|l| links.resolve(l, &path) == Some(note))
        else {
            continue;
        };
        // The text only of the notes that link here, for the snippet.
        let text = step(|c| c.replicas.get(vault).map(|r| r.note_text(&id))).unwrap_or_default();
        let line_start = text
            .get(..l.start)
            .and_then(|t| t.rfind('\n'))
            .map(|i| i + 1)
            .unwrap_or(0);
        let line_end = text
            .get(l.end..)
            .and_then(|t| t.find('\n'))
            .map(|i| l.end + i)
            .unwrap_or(text.len());
        // As shown: without HTML tags (colors).
        let snippet: String = markdown::shown_text(text.get(line_start..line_end).unwrap_or(""))
            .chars()
            .take(160)
            .collect();
        out.push(Backlink { id, path, snippet });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// What uses the file `id`: the notes linking to it or embedding it (with
/// the line that does), and the drawings showing it (no line).
pub fn used_in(vault: &str, id: &str) -> Vec<Backlink> {
    let mut out = backlinks(vault, id);
    let drawings: Vec<(String, String)> = step(|c| {
        let Some(r) = c.replicas.get(vault) else {
            return vec![];
        };
        c.live_entries(vault)
            .into_iter()
            .filter(|e| e.kind == EntryKind::Drawing)
            .filter(|e| {
                r.notes.get(&e.id).is_some_and(|d| {
                    d.drawing()
                        .files()
                        .values()
                        .any(|f| notekpr_core::pictures::attachment_of(f) == Some(id))
                })
            })
            .map(|e| (e.id, e.path))
            .collect()
    });
    out.extend(drawings.into_iter().map(|(id, path)| Backlink {
        id,
        path,
        snippet: String::new(),
    }));
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Links in a note that point to nothing (broken-link detection).
pub fn broken_links(vault: &str, note: &str, path: &str, text: &str) -> Vec<String> {
    let info = analysis(vault, note, text);
    let links = resolver(vault);
    let mut out: Vec<String> = info
        .links
        .iter()
        .filter(|l| !l.target.is_empty() && links.resolve(l, path).is_none())
        .map(|l| l.target.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

pub fn outgoing(vault: &str, note: &str, path: &str, text: &str) -> Vec<(String, String)> {
    let info = analysis(vault, note, text);
    let links = resolver(vault);
    let mut out: Vec<(String, String)> = info
        .links
        .iter()
        .filter_map(|l| links.find(l, path))
        .filter(|(id, _)| id != note)
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

pub fn tags(vault: &str) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (_, _, info) in analysed_notes(vault) {
        for t in &info.tags {
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
        for (id, path, text) in searchable(&vid) {
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
