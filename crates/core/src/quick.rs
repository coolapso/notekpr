//! Quick notes: a capture-first place for thoughts that don't have a home
//! yet, the same in every app.
//!
//! Each account has one quick-notes vault ([`VaultKind::QuickNotes`]): it
//! is never shared, holds at most [`VaultInfo::note_limit`] notes and is
//! flat — every note sits at its top level, named after the minute it was
//! made. A quick note leaves it by moving to a vault, or by being deleted
//! for good (it has no trash). Apps show it as "Quick notes", never as a
//! vault.
//!
//! [`VaultKind::QuickNotes`]: crate::protocol::VaultKind::QuickNotes
//! [`VaultInfo::note_limit`]: crate::protocol::VaultInfo::note_limit

use std::collections::BTreeSet;

use crate::doc::EntryKind;
use crate::engine::VaultReplica;
use crate::markdown;
use crate::protocol::Limit;

/// What the quick notes are called everywhere: the vault's (encrypted)
/// name and its folder in the Markdown mirror.
pub const NAME: &str = "Quick notes";

/// Whether `name` is the quick notes' own (in any case and spacing): no vault
/// may be called that, so in commands and in the Markdown folder it always
/// means one's quick notes. Clients refuse it; the server can't (vault names
/// are encrypted).
pub fn is_reserved_name(name: &str) -> bool {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        == NAME.to_lowercase()
}

/// What is said when a vault would be named like the quick notes.
pub fn reserved_name_message() -> String {
    format!("\"{NAME}\" is the name of your quick notes: pick another name for the vault.")
}

/// Name of a quick note made at this local time, e.g. `2026-10-19 14.32`.
/// The time uses a dot: `:` is not allowed in Windows file names (mirrored
/// folders) and note names replace it anyway.
pub fn stamp_name(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> String {
    format!("{year:04}-{month:02}-{day:02} {hour:02}.{minute:02}")
}

/// Whether a path in the quick-notes vault is where things may be: its top
/// level (no folders).
pub fn is_top_level(path: &str) -> bool {
    !path.is_empty() && !path.contains('/')
}

/// Whether one more quick note fits when there are `count`.
pub fn has_room(count: usize, limit: Limit) -> bool {
    limit.allows_one_more(count as u64)
}

/// What is said when there's no room for another quick note.
pub fn full_message(limit: Limit) -> String {
    match limit.max() {
        Some(n) => {
            format!("Quick notes are full ({n} of {n}): move some to a vault or delete them first.")
        }
        None => "Quick notes are full: move some to a vault or delete them first.".into(),
    }
}

/// How long a file may be among the quick notes with no quick note linking
/// to it before a tidy-up deletes it: time for the link to follow its file
/// (pasted into a note, or made on another device).
pub const UNLINKED_FILE_GRACE_MS: i64 = 60 * 60 * 1000;

/// Whether a quick note links to or embeds the file at `path` (`notes`: the
/// path and text of each). Only such a file may be among the quick notes:
/// they keep files for their notes, not files of their own.
pub fn is_linked(path: &str, notes: &[(String, String)]) -> bool {
    const FILE: &str = "";
    let mut candidates: Vec<(String, String)> = notes
        .iter()
        .enumerate()
        .map(|(i, (p, _))| (i.to_string(), p.clone()))
        .collect();
    candidates.push((FILE.to_string(), path.to_string()));
    notes.iter().any(|(from, text)| {
        markdown::linked_ids(text, from, &candidates)
            .iter()
            .any(|id| id == FILE)
    })
}

/// The quick notes' files that no quick note links to any more and that
/// haven't changed since `before` (ms): an app tidying up deletes them for
/// good. `None` while that can't be told: a note's text hasn't arrived, or
/// some content waits for a key.
pub fn unlinked_files(r: &VaultReplica, before: i64) -> Option<Vec<String>> {
    if !r.deferred.is_empty() || !r.unverified.is_empty() || !r.dropped.is_empty() {
        return None;
    }
    let live: Vec<_> = r
        .index
        .entries()
        .into_iter()
        .filter(|e| e.is_live())
        .collect();
    let candidates: Vec<(String, String)> = live
        .iter()
        .filter(|e| e.kind != EntryKind::Folder)
        .map(|e| (e.id.clone(), e.path.clone()))
        .collect();
    let mut linked = BTreeSet::new();
    for e in live.iter().filter(|e| e.kind == EntryKind::Note) {
        let note = r.notes.get(&e.id)?;
        linked.extend(markdown::linked_ids(&note.text(), &e.path, &candidates));
    }
    Some(
        live.into_iter()
            .filter(|e| {
                e.kind == EntryKind::Attachment && e.modified < before && !linked.contains(&e.id)
            })
            .map(|e| e.id)
            .collect(),
    )
}

/// Strip Markdown block markers from a line shown as plain text: headings,
/// quotes, list bullets and task boxes.
fn plain_line(line: &str) -> &str {
    let mut s = line.trim();
    loop {
        let before = s;
        let hashes = s.len() - s.trim_start_matches('#').len();
        if (1..=6).contains(&hashes) && s[hashes..].starts_with(' ') {
            s = s[hashes..].trim_start();
        }
        if let Some(r) = s.strip_prefix('>') {
            s = r.trim_start();
        }
        for m in ["- ", "* ", "+ "] {
            if let Some(r) = s.strip_prefix(m) {
                s = r.trim_start();
            }
        }
        let digits = s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 && (s[digits..].starts_with(". ") || s[digits..].starts_with(") ")) {
            s = s[digits + 2..].trim_start();
        }
        for m in ["[ ] ", "[x] ", "[X] "] {
            if let Some(r) = s.strip_prefix(m) {
                s = r.trim_start();
            }
        }
        if s == before {
            return s;
        }
    }
}

/// The text's lines worth showing: without YAML front matter, code fences
/// and blank lines, Markdown block markers removed.
fn content_lines(text: &str) -> impl Iterator<Item = &str> {
    let mut lines = text.lines().peekable();
    if lines.peek().is_some_and(|l| l.trim_end() == "---") {
        lines.next();
        let mut rest = lines.clone();
        // Only a closed block is front matter.
        if rest.any(|l| l.trim_end() == "---") {
            for l in lines.by_ref() {
                if l.trim_end() == "---" {
                    break;
                }
            }
        } else {
            lines = text.lines().peekable();
        }
    }
    lines
        .filter(|l| !l.trim_start().starts_with("```"))
        .map(plain_line)
        .filter(|l| !l.is_empty() && *l != "---")
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out = out.trim_end().to_string();
    out.push('…');
    out
}

/// What lists and cards show for a quick note: its first non-empty line
/// (as shown: without HTML tags), or the title if there is no text yet.
pub fn preview(text: &str, title: &str) -> String {
    content_lines(&markdown::shown_text(text))
        .next()
        .map(|l| clip(l, 160))
        .unwrap_or_else(|| title.to_string())
}

/// A suggested note name for moving a quick note to a vault: its first
/// line, without characters that break file names or `[[links]]`.
pub fn suggested_title(text: &str) -> String {
    let shown = markdown::shown_text(text);
    let first = content_lines(&shown).next().unwrap_or("");
    let cleaned: String = first
        .chars()
        .filter_map(|c| match c {
            '/' | '\\' | ':' | '|' => Some('-'),
            '*' | '?' | '"' | '<' | '>' | '#' | '[' | ']' | '^' => None,
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    let mut name = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.chars().count() > 80 {
        let cut: String = name.chars().take(80).collect();
        name = match cut.rfind(' ') {
            Some(i) if i > 40 => cut[..i].to_string(),
            _ => cut,
        };
    }
    name.trim_end_matches(['.', ' ', '-'])
        .trim_start_matches(['.', ' ', '-'])
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths;

    #[test]
    fn names_are_local_timestamps() {
        assert_eq!(stamp_name(2026, 10, 19, 14, 32), "2026-10-19 14.32");
        assert_eq!(stamp_name(2027, 1, 2, 3, 4), "2027-01-02 03.04");
        // Safe as a file name everywhere, and the unique-path helper
        // numbers a second one of the same minute.
        let name = stamp_name(2026, 10, 19, 14, 32);
        assert!(!name.contains([':', '/', '\\']));
        let p = paths::normalize_note(&name).unwrap();
        assert_eq!(p, "2026-10-19 14.32.md");
        assert!(is_top_level(&p));
        assert_eq!(paths::unique(&p, |x| x == p), "2026-10-19 14.32 2.md");
    }

    #[test]
    fn no_vault_is_named_like_the_quick_notes() {
        for n in [
            "Quick notes",
            "quick notes",
            " QUICK  Notes ",
            "Quick\tnotes",
        ] {
            assert!(is_reserved_name(n), "{n:?}");
        }
        for n in ["Quick notes 2", "Quicknotes", "Notes", "My quick notes", ""] {
            assert!(!is_reserved_name(n), "{n:?}");
        }
        assert!(reserved_name_message().contains("\"Quick notes\""));
    }

    #[test]
    fn files_stay_only_while_a_note_links_to_them() {
        use crate::engine::VaultMeta;
        use crate::keys::{VaultKey, VaultKeyring};
        use crate::protocol::Role;

        let notes = [(
            "Idea.md".to_string(),
            "Look: ![](photo.png) and [[plan.pdf]]".to_string(),
        )];
        assert!(is_linked("photo.png", &notes));
        assert!(is_linked("plan.pdf", &notes));
        assert!(!is_linked("other.png", &notes));
        assert!(!is_linked("photo.png", &[]));

        let mut ring = VaultKeyring::new("q");
        ring.insert(1, VaultKey::generate());
        let mut r = VaultReplica::new("q", Role::Owner, ring, VaultMeta::named(NAME));
        r.index.create("n1", "Idea.md", EntryKind::Note, 1);
        r.notes
            .entry("n1".into())
            .or_default()
            .set_text(&notes[0].1);
        r.index.create("a1", "photo.png", EntryKind::Attachment, 1);
        r.index.create("a2", "plan.pdf", EntryKind::Attachment, 1);
        r.index.create("a3", "loose.png", EntryKind::Attachment, 1);
        // Too new to tell: its link may be on its way.
        r.index
            .create("a4", "fresh.png", EntryKind::Attachment, 100);
        assert_eq!(unlinked_files(&r, 50), Some(vec!["a3".to_string()]));
        // Edited out of its note: it goes too.
        r.notes["n1"].set_text("Look: [[plan.pdf]]");
        let mut gone = unlinked_files(&r, 50).unwrap();
        gone.sort();
        assert_eq!(gone, ["a1", "a3"]);
        // A note whose text hasn't arrived may link to any of them.
        r.index.create("n2", "Other.md", EntryKind::Note, 1);
        assert_eq!(unlinked_files(&r, 50), None);
    }

    #[test]
    fn only_the_top_level() {
        assert!(is_top_level("Idea.md"));
        assert!(is_top_level("photo.png"));
        assert!(!is_top_level("Archive/old.md"));
        assert!(!is_top_level(""));
    }

    #[test]
    fn room_follows_the_limit() {
        assert!(has_room(19, Limit::Max(20)));
        assert!(!has_room(20, Limit::Max(20)));
        assert!(!has_room(25, Limit::Max(20)));
        assert!(has_room(1000, Limit::Unlimited));
        assert!(full_message(Limit::Max(20)).contains("20 of 20"));
    }

    #[test]
    fn preview_is_the_first_non_empty_line() {
        assert_eq!(
            preview("\n\n  Call the dentist  \nmore", "t"),
            "Call the dentist"
        );
        assert_eq!(preview("", "2026-10-19 14.32"), "2026-10-19 14.32");
        assert_eq!(preview("   \n\t\n", "title"), "title");
        assert_eq!(preview("# Big idea\nbody", "t"), "Big idea");
        assert_eq!(preview("- [ ] buy milk\n- eggs", "t"), "buy milk");
        assert_eq!(preview("> quoted thought", "t"), "quoted thought");
        assert_eq!(preview("1. first step", "t"), "first step");
        assert_eq!(preview("#tag at the start", "t"), "#tag at the start");
        assert_eq!(
            preview("<span style=\"color: #e44045\">Call</span> mom", "t"),
            "Call mom"
        );
        assert_eq!(
            preview("---\ntags: [a]\n---\n\nReal text", "t"),
            "Real text"
        );
        // An unclosed `---` is just a rule, not front matter.
        assert_eq!(preview("---\nText", "t"), "Text");
        assert_eq!(preview("```\ncode\n```", "t"), "code");
        let long = "word ".repeat(100);
        let p = preview(&long, "t");
        assert!(p.chars().count() <= 161 && p.ends_with('…'), "{p}");
    }

    #[test]
    fn suggested_titles_are_safe_names() {
        assert_eq!(suggested_title("Garden plan\nmore"), "Garden plan");
        assert_eq!(suggested_title("## Idea: a/b test?"), "Idea- a-b test");
        assert_eq!(suggested_title("See [[Other]] #tag"), "See Other tag");
        assert_eq!(
            suggested_title("<span style=\"background-color: #ffd0004d\">Garden</span> plan"),
            "Garden plan"
        );
        assert_eq!(suggested_title(""), "");
        assert_eq!(suggested_title("..."), "");
        let long = format!("{} end", "abcdefghij ".repeat(10));
        let t = suggested_title(&long);
        assert!(t.chars().count() <= 80 && !t.ends_with(' '), "{t}");
    }
}
