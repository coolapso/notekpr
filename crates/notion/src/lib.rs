//! Notion's HTML exports as Markdown notes, for the web app's and the CLI's
//! imports. People export a page or a database at a time (⋯ → Export →
//! HTML, with subpages and folders for them) and import each; the design
//! is in the maintainer's notes (*Notion import*).
//!
//! Sans-IO: [`convert`] takes the exports as unzipped (paths and bytes)
//! and returns the notes and files to import, the icons by path, and a
//! [`Report`]. The clients unzip, choose where it lands
//! ([`landing_folder`]), say what the vault holds already ([`Options`]),
//! and import what comes back like any other files.
//!
//! * A page is a note; one with subpages a folder, its own text in a note
//!   inside it (`Trips/Trips.md`). A full-page database is a folder with a
//!   note per row, the properties in front matter, and an index note with
//!   a table of the rows. A database of tasks without more to say is a
//!   checklist; an inline one without more to say is a table in its page.
//! * Links between pages become wiki links; a link to a page that isn't in
//!   this export becomes `[[Its name]]`, which finds it once it is
//!   imported too.
//! * Every note keeps its page's ID in its front matter (`notion: <id>`),
//!   so a later import skips what came already and links to it.
//! * What doesn't come along is listed in a report note,
//!   [`REPORT`], at the top of the import.
//!
//! The same exports and options always give the same files, byte for byte.

mod csv;
mod dom;
mod read;
mod text;
mod write;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Files as unzipped: path and bytes.
pub type Files = Vec<(String, Vec<u8>)>;

/// The report note, at the top of an import.
pub const REPORT: &str = "Imported from Notion.md";
/// The folder an import's files go in, at its top.
pub const ATTACHMENTS: &str = "attachments";
/// The front matter key holding the Notion ID of the page a note came from.
pub const ID_KEY: &str = "notion";

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The folder the import lands in (`""`: the vault's top level).
    pub into: String,
    /// Pages imported before: Notion ID → the note's path in the vault
    /// ([`id_of`] reads a note's). They aren't imported again, and links
    /// to them find them.
    pub existing: BTreeMap<String, String>,
    /// Paths of what the vault holds (notes and files): a link names a note
    /// by its name only where no other note has it.
    pub vault_paths: Vec<String>,
    /// Pages and databases (Notion IDs) to leave out, with what's in them.
    pub skip: BTreeSet<String>,
}

pub struct Converted {
    /// Notes and files, by their paths in the vault (under
    /// [`Options::into`]), the report note among them.
    pub files: Files,
    /// Emoji icons of notes and folders, by path.
    pub icons: BTreeMap<String, String>,
    pub report: Report,
}

/// What an import brought and what it couldn't.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub notes: usize,
    pub files: usize,
    /// Pages already in the vault, from an earlier import.
    pub already: usize,
    pub databases: Vec<DatabaseReport>,
    /// Pages linked to that weren't in the export: their names, and the
    /// notes linking to them.
    pub waiting: BTreeMap<String, BTreeSet<String>>,
    /// What didn't come along, and the notes it was in (or what else).
    pub lost: BTreeMap<String, BTreeSet<String>>,
    /// Database templates, which aren't imported: `Database: A, B`.
    pub templates: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseReport {
    pub title: String,
    /// The note showing it (its index, checklist, or the page it's in).
    pub note: Option<String>,
    /// How many databases of this title (inline ones are counted together).
    pub databases: usize,
    pub rows: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// No Notion pages in what was given.
    NotAnExport,
    /// A Markdown export, which loses too much: export as HTML.
    Markdown,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Error::NotAnExport => {
                "this isn't an export from Notion: export a page as HTML (⋯ → Export) and import its .zip"
            }
            Error::Markdown => {
                "this is a Markdown export from Notion, which leaves out too much: export as HTML instead"
            }
        })
    }
}

impl std::error::Error for Error {}

/// Notion's exports (each as unzipped) as notes and files to import.
pub fn convert(exports: Vec<Files>, opts: &Options) -> Result<Converted, Error> {
    let ws = read::Workspace::read(exports);
    if ws.pages.is_empty() && ws.databases.is_empty() {
        return Err(
            if ws
                .unreadable
                .iter()
                .any(|u| matches!(u, read::Unreadable::Markdown(_)))
            {
                Error::Markdown
            } else {
                Error::NotAnExport
            },
        );
    }
    Ok(write::Writer::new(&ws, opts).run())
}

/// The folder an import lands in: `Notion`, or `Notion 2`, `Notion 3`, …,
/// the first that isn't `taken`, so it never writes into an earlier one.
pub fn landing_folder(taken: impl Fn(&str) -> bool) -> String {
    notekpr_core::account_export::unique_name("Notion", taken)
}

/// The Notion ID in a note's front matter: it came from that page.
pub fn id_of(note: &str) -> Option<String> {
    let len = notekpr_core::markdown::frontmatter_len(note)?;
    note[..len].lines().find_map(|l| {
        let v = l.strip_prefix(ID_KEY)?.trim_start().strip_prefix(':')?;
        let v = v.trim().trim_matches(['"', '\'']);
        (v.len() == 32).then(|| text::notion_id(v)).flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_of_notes() {
        let id = "9db7f440709042c0ab4f7089c8315891";
        assert_eq!(
            id_of(&format!("---\ntags:\n  - x\nnotion: {id}\n---\nText")).as_deref(),
            Some(id)
        );
        assert_eq!(id_of(&format!("notion: {id}\n")), None);
        assert_eq!(id_of("---\nnotion: nope\n---\n"), None);
    }

    #[test]
    fn landing() {
        assert_eq!(landing_folder(|_| false), "Notion");
        assert_eq!(
            landing_folder(|n| n == "Notion" || n == "Notion 2"),
            "Notion 3"
        );
    }
}
