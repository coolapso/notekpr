//! Quick notes: a capture-first space for thoughts that don't have a home yet.
//!
//! Quick notes are ordinary notes in the `Quick notes/` folder of one vault —
//! the synced `quick_vault` preference, otherwise the person's own first
//! vault — so they sync, mirror to Markdown files and work in other editors.
//! Archived ones move to `Quick notes/Archive/`; promoting one moves and
//! renames it out of `Quick notes/`.
//!
//! The pure helpers at the top are shared in spirit with the CLI
//! (`coolpsonotes quick`, `crates/client/src/ops.rs`): same folder, naming
//! and default-vault rule.

use coolpsonotes_core::doc::EntryKind;
use coolpsonotes_core::paths;
use coolpsonotes_core::protocol::Role;

use leptos::prelude::*;

use crate::actions;
use crate::state::{self, Route, ViewMode, with};

/// Folder of the quick notes in their vault.
pub const FOLDER: &str = "Quick notes";
/// Archived quick notes (hidden from the sidebar and the overview's list).
pub const ARCHIVE: &str = "Quick notes/Archive";

// ------------------------------------------------------------------ pure helpers

/// Name of a quick note made at this local time, e.g. `2026-10-19 14.32`.
/// The time uses a dot: `:` is not allowed in Windows file names (mirrored
/// folders) and note names replace it anyway.
pub fn stamp_name(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> String {
    format!("{year:04}-{month:02}-{day:02} {hour:02}.{minute:02}")
}

/// Whether a vault path is a quick note (archived or not).
pub fn is_quick(path: &str) -> bool {
    paths::is_within(path, FOLDER) && paths::is_note(path)
}

pub fn is_archived(path: &str) -> bool {
    paths::is_within(path, ARCHIVE)
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

/// What lists and cards show for a quick note: its first non-empty line,
/// or the title if there is no text yet.
pub fn preview(text: &str, title: &str) -> String {
    content_lines(text)
        .next()
        .map(|l| clip(l, 160))
        .unwrap_or_else(|| title.to_string())
}

/// The text after the preview line, joined into one line (card snippets).
pub fn rest(text: &str) -> String {
    clip(
        &content_lines(text).skip(1).collect::<Vec<_>>().join(" "),
        280,
    )
}

/// A suggested note name for promoting a quick note: its first line,
/// without characters that break file names or `[[links]]`.
pub fn promote_title(text: &str) -> String {
    let first = content_lines(text).next().unwrap_or("");
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

/// A vault that could hold the quick notes.
#[derive(Clone, Debug, PartialEq)]
pub struct VaultCandidate {
    pub id: String,
    pub name: String,
    pub owned: bool,
    pub writable: bool,
    pub archived: bool,
    /// When the vault was created (`i64::MAX` if unknown).
    pub created_at: i64,
}

impl VaultCandidate {
    pub fn new(id: &str, name: &str, role: Role, archived: bool, created_at: i64) -> Self {
        VaultCandidate {
            id: id.to_string(),
            name: name.to_string(),
            owned: role == Role::Owner,
            writable: role.can_write(),
            archived,
            created_at,
        }
    }
}

/// The quick-notes vault: the chosen one (`pref`) if it can still be
/// edited, otherwise the first vault the person owns (their personal vault,
/// the oldest one), otherwise the first one they can edit. Archived vaults
/// are only used when chosen explicitly.
pub fn choose_vault(pref: Option<&str>, vaults: &[VaultCandidate]) -> Option<String> {
    if let Some(p) = pref
        && vaults.iter().any(|v| v.id == p && v.writable)
    {
        return Some(p.to_string());
    }
    let mut c: Vec<&VaultCandidate> = vaults
        .iter()
        .filter(|v| v.writable && !v.archived)
        .collect();
    c.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    c.iter()
        .find(|v| v.owned)
        .or_else(|| c.first())
        .map(|v| v.id.clone())
}

// ------------------------------------------------------------------ app state

/// A quick note for the views.
#[derive(Clone, Debug, PartialEq)]
pub struct QuickNote {
    pub vault: String,
    pub id: String,
    pub path: String,
    pub title: String,
    pub preview: String,
    pub rest: String,
    pub created: i64,
    pub modified: i64,
    pub archived: bool,
}

/// The vaults that could hold quick notes (with their keys loaded).
pub fn candidates() -> Vec<VaultCandidate> {
    with(|c| {
        c.vault_infos
            .values()
            .filter_map(|i| {
                let r = c.replicas.get(&i.id)?;
                Some(VaultCandidate::new(
                    &i.id,
                    &r.meta.name,
                    r.role,
                    i.archived,
                    i.created_at,
                ))
            })
            .collect()
    })
}

/// The vault quick notes go to (`None`: no vault can be edited).
pub fn vault() -> Option<String> {
    let pref = with(|c| c.prefs.quick_vault.clone());
    choose_vault(pref.as_deref(), &candidates())
}

/// Quick notes of `vault`, newest first: the archived ones too with
/// `archived`, at most `limit`.
pub fn notes(vault: &str, archived: bool, limit: usize) -> Vec<QuickNote> {
    let mut list: Vec<_> = state::live_entries(vault)
        .into_iter()
        .filter(|e| e.kind == EntryKind::Note && is_quick(&e.path))
        .filter(|e| archived || !is_archived(&e.path))
        .collect();
    list.sort_by(|a, b| {
        b.created
            .cmp(&a.created)
            .then(b.modified.cmp(&a.modified))
            .then(a.id.cmp(&b.id))
    });
    list.truncate(limit);
    list.into_iter()
        .map(|e| {
            let text =
                with(|c| c.replicas.get(vault).map(|r| r.note_text(&e.id))).unwrap_or_default();
            let title = e.title().to_string();
            QuickNote {
                vault: vault.to_string(),
                preview: preview(&text, &title),
                rest: rest(&text),
                archived: is_archived(&e.path),
                id: e.id,
                path: e.path,
                title,
                created: e.created,
                modified: e.modified,
            }
        })
        .collect()
}

pub const NEEDS_VAULT: &str =
    "Quick notes need a vault you can edit. Create one, or ask for edit access to a shared vault.";

fn local_stamp() -> String {
    let d = js_sys::Date::new_0();
    stamp_name(
        d.get_full_year() as i32,
        d.get_month() + 1,
        d.get_date(),
        d.get_hours(),
        d.get_minutes(),
    )
}

/// Create a quick note with `text`; returns its vault and id.
pub async fn create(text: &str) -> Option<(String, String)> {
    let Some(v) = vault() else {
        state::error(NEEDS_VAULT);
        return None;
    };
    let text = text.trim_end().trim_start_matches(['\n', '\r']);
    let id = actions::create_note_with(&v, FOLDER, &local_stamp(), text).await?;
    Some((v, id))
}

/// Create an empty quick note and open it, ready to type.
pub fn new_and_open() {
    leptos::task::spawn_local(async {
        if let Some((v, id)) = create("").await {
            let ui = state::ui();
            ui.modal.set(None);
            if ui.mode.get_untracked() == ViewMode::Reading {
                ui.mode.set(ViewMode::Live);
            }
            state::navigate(Route::Note(v, id));
        }
    });
}

pub fn archive(vault: &str, id: &str) {
    match actions::move_entry(vault, id, ARCHIVE) {
        Ok(()) => state::info("Archived."),
        Err(e) => state::error(e),
    }
}

pub fn unarchive(vault: &str, id: &str) {
    match actions::move_entry(vault, id, FOLDER) {
        Ok(()) => state::info("Back in quick notes."),
        Err(e) => state::error(e),
    }
}

/// Turn a quick note into a regular note: move it to `folder` and give it
/// the name `title`.
pub fn promote(vault: &str, id: &str, title: &str, folder: &str) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() || title.contains('/') || title.contains('\\') {
        return Err("Give the note a name without '/'.".into());
    }
    let folder = match folder.trim().trim_matches('/') {
        "" => String::new(),
        f => paths::normalize(f).map_err(|e| e.to_string())?,
    };
    if folder == FOLDER || paths::is_within(&folder, FOLDER) {
        return Err("Pick a folder outside Quick notes.".into());
    }
    let target = paths::normalize_note(&paths::join(&folder, title)).map_err(|e| e.to_string())?;
    if state::live_entries(vault)
        .iter()
        .any(|e| e.id != id && e.path.eq_ignore_ascii_case(&target))
    {
        return Err("A note with that name already exists in that folder.".into());
    }
    actions::move_entry(vault, id, &folder)?;
    actions::rename_entry(vault, id, title)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_local_timestamps() {
        assert_eq!(stamp_name(2026, 10, 19, 14, 32), "2026-10-19 14.32");
        assert_eq!(stamp_name(2027, 1, 2, 3, 4), "2027-01-02 03.04");
        // Safe as a file name everywhere, and the unique-path helper
        // numbers a second one of the same minute.
        let name = stamp_name(2026, 10, 19, 14, 32);
        assert!(!name.contains([':', '/', '\\']));
        let p = paths::normalize_note(&paths::join(FOLDER, &name)).unwrap();
        assert_eq!(p, "Quick notes/2026-10-19 14.32.md");
        assert_eq!(paths::title(&p), "2026-10-19 14.32");
        assert_eq!(
            paths::unique(&p, |x| x == p),
            "Quick notes/2026-10-19 14.32 2.md"
        );
    }

    #[test]
    fn quick_and_archived_paths() {
        assert!(is_quick("Quick notes/2026-10-19 14.32.md"));
        assert!(is_quick("Quick notes/Archive/old.md"));
        assert!(is_archived("Quick notes/Archive/old.md"));
        assert!(!is_archived("Quick notes/new.md"));
        assert!(!is_quick("Quick notes/photo.png"));
        assert!(!is_quick("Quick notes.md"));
        assert!(!is_quick("Quick notes 2/a.md"));
        assert!(!is_quick("Projects/Quick notes/a.md"));
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
            preview("---\ntags: [a]\n---\n\nReal text", "t"),
            "Real text"
        );
        // An unclosed `---` is just a rule, not front matter.
        assert_eq!(preview("---\nText", "t"), "Text");
        assert_eq!(preview("```\ncode\n```", "t"), "code");
        let long = "word ".repeat(100);
        let p = preview(&long, "t");
        assert!(p.chars().count() <= 161 && p.ends_with('…'), "{p}");
        assert_eq!(rest("Title line\n\nsecond\n- third"), "second third");
        assert_eq!(rest("only one"), "");
    }

    #[test]
    fn promote_title_is_a_safe_name() {
        assert_eq!(promote_title("Garden plan\nmore"), "Garden plan");
        assert_eq!(promote_title("## Idea: a/b test?"), "Idea- a-b test");
        assert_eq!(promote_title("See [[Other]] #tag"), "See Other tag");
        assert_eq!(promote_title(""), "");
        assert_eq!(promote_title("..."), "");
        let long = format!("{} end", "abcdefghij ".repeat(10));
        let t = promote_title(&long);
        assert!(t.chars().count() <= 80 && !t.ends_with(' '), "{t}");
    }

    fn cand(id: &str, role: Role, archived: bool, created_at: i64) -> VaultCandidate {
        VaultCandidate::new(id, id, role, archived, created_at)
    }

    #[test]
    fn default_vault_is_the_own_first_one() {
        let vaults = vec![
            cand("family", Role::Editor, false, 1),
            cand("work", Role::Owner, false, 30),
            cand("mine", Role::Owner, false, 10),
            cand("read", Role::Viewer, false, 0),
            cand("old", Role::Owner, true, 5),
        ];
        // The personal (oldest own, not archived) vault by default.
        assert_eq!(choose_vault(None, &vaults).as_deref(), Some("mine"));
        // A choice that can still be edited wins, even an archived vault.
        assert_eq!(choose_vault(Some("work"), &vaults).as_deref(), Some("work"));
        assert_eq!(choose_vault(Some("old"), &vaults).as_deref(), Some("old"));
        assert_eq!(
            choose_vault(Some("family"), &vaults).as_deref(),
            Some("family")
        );
        // A read-only or vanished choice falls back to the default.
        assert_eq!(choose_vault(Some("read"), &vaults).as_deref(), Some("mine"));
        assert_eq!(choose_vault(Some("gone"), &vaults).as_deref(), Some("mine"));
        // Without an own vault: the first one we can edit.
        let shared = vec![
            cand("b", Role::Editor, false, 20),
            cand("a", Role::Editor, false, 10),
            cand("r", Role::Viewer, false, 1),
        ];
        assert_eq!(choose_vault(None, &shared).as_deref(), Some("a"));
        // Nothing writable: no quick notes.
        let ro = vec![
            cand("r", Role::Viewer, false, 1),
            cand("o", Role::Owner, true, 1),
        ];
        assert_eq!(choose_vault(None, &ro), None);
        assert_eq!(choose_vault(None, &[]), None);
        // Unknown creation times (offline CLI): by name.
        let unknown = vec![
            VaultCandidate::new("2", "Zettel", Role::Owner, false, i64::MAX),
            VaultCandidate::new("1", "My notes", Role::Owner, false, i64::MAX),
        ];
        assert_eq!(choose_vault(None, &unknown).as_deref(), Some("1"));
    }
}
