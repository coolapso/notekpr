//! Moving a note, or a folder with everything in it, to another vault.
//!
//! Each vault has its own keys, so a move copies: the note or the folder's
//! notes, folders and files, and the files elsewhere its notes link to.
//! Once the other vault has every copy, the originals go to the trash
//! (deleted for good from the quick notes), except files a note staying
//! behind still links to. History, comments and note shares stay with the
//! originals. [`plan`] decides what goes, [`Plan::place`] where each copy
//! goes in the other vault; the apps do the copying (files are downloaded
//! and uploaded again).

use std::collections::{BTreeMap, BTreeSet};

use crate::doc::{Entry, EntryKind};
use crate::engine::VaultReplica;
use crate::markdown;
use crate::paths::{self, PathError};

/// What moves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum What {
    /// A note, by id.
    Note(String),
    /// A folder, by path, with everything in it.
    Folder(String),
}

/// Why nothing can move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unmovable {
    /// There's no such note or folder (any more).
    Gone,
    /// The text of one of its notes hasn't all arrived: a copy would lack it.
    Loading,
}

#[derive(Clone, Debug)]
enum Place {
    /// The note or folder moved.
    Root,
    /// In the folder moved, at this path under it.
    Within(String),
    /// A file from elsewhere a note links to: beside the note moved, at the
    /// top level of the folder moved.
    Beside,
}

#[derive(Clone, Debug)]
struct Item {
    /// With its display path.
    entry: Entry,
    place: Place,
}

/// What a move takes, from [`plan`].
#[derive(Clone, Debug)]
pub struct Plan {
    what: What,
    /// The display path of the note or folder moved.
    root: String,
    items: Vec<Item>,
    /// The notes' texts, by id.
    texts: BTreeMap<String, String>,
    /// What the notes' links may point at in the vault moved from.
    candidates: Vec<(String, String)>,
    /// The originals to trash once the other vault has every copy.
    pub trash: Vec<String>,
}

/// One entry to make in the other vault, from [`Plan::place`].
#[derive(Clone, Debug, PartialEq)]
pub struct Copy {
    /// The original's id.
    pub from: String,
    pub kind: EntryKind,
    pub path: String,
    /// A note's text, its links to files renamed on the way following them
    /// (empty for the others).
    pub text: String,
    pub icon: Option<String>,
    /// Its place among its siblings, when they came along.
    pub order: Option<String>,
}

/// Where a move puts everything, from [`Plan::place`].
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    /// The path of the note or folder in the other vault.
    pub path: String,
    /// In the order to make them: folders (parents first), files, notes.
    pub copies: Vec<Copy>,
}

/// What moving `what` out of `r` takes.
pub fn plan(r: &VaultReplica, what: &What) -> Result<Plan, Unmovable> {
    let entries: Vec<Entry> = r
        .index
        .resolved_paths()
        .into_iter()
        .map(|(p, mut e)| {
            e.path = p;
            e
        })
        .collect();
    let (root, mut items) = match what {
        What::Note(id) => {
            let e = entries
                .iter()
                .find(|e| &e.id == id && e.kind == EntryKind::Note)
                .ok_or(Unmovable::Gone)?;
            let items = vec![Item {
                entry: e.clone(),
                place: Place::Root,
            }];
            (e.path.clone(), items)
        }
        What::Folder(folder) => {
            let folder = paths::normalize(folder).map_err(|_| Unmovable::Gone)?;
            let items: Vec<Item> = entries
                .iter()
                .filter_map(|e| {
                    let place = if e.path == folder && e.kind == EntryKind::Folder {
                        Place::Root
                    } else if paths::is_within(&e.path, &folder) {
                        Place::Within(e.path[folder.len() + 1..].to_string())
                    } else {
                        return None;
                    };
                    Some(Item {
                        entry: e.clone(),
                        place,
                    })
                })
                .collect();
            if items.is_empty() {
                return Err(Unmovable::Gone);
            }
            (folder, items)
        }
    };
    let mut texts = BTreeMap::new();
    for e in items.iter().map(|i| &i.entry) {
        if e.kind == EntryKind::Note {
            let note = r
                .notes
                .get(&e.id)
                .filter(|_| r.is_complete(&e.id))
                .ok_or(Unmovable::Loading)?;
            texts.insert(e.id.clone(), note.text());
        }
    }
    let candidates: Vec<(String, String)> = entries
        .iter()
        .filter(|e| e.kind != EntryKind::Folder)
        .map(|e| (e.id.clone(), e.path.clone()))
        .collect();
    let moving: BTreeSet<String> = items.iter().map(|i| i.entry.id.clone()).collect();
    let links = |e: &Entry, text: &str| markdown::linked_ids(text, &e.path, &candidates);
    // The files elsewhere its notes link to come along…
    let wanted: BTreeSet<String> = items
        .iter()
        .filter_map(|i| Some(links(&i.entry, texts.get(&i.entry.id)?)))
        .flatten()
        .collect();
    // …but those a note staying behind links to stay too.
    let kept: BTreeSet<String> = entries
        .iter()
        .filter(|e| e.kind == EntryKind::Note && !moving.contains(&e.id))
        .flat_map(|e| links(e, &r.note_text(&e.id)))
        .collect();
    for e in &entries {
        if e.kind == EntryKind::Attachment && wanted.contains(&e.id) && !moving.contains(&e.id) {
            items.push(Item {
                entry: e.clone(),
                place: Place::Beside,
            });
        }
    }
    let trash = items
        .iter()
        .filter(|i| i.entry.kind != EntryKind::Attachment || !kept.contains(&i.entry.id))
        .map(|i| i.entry.id.clone())
        .collect();
    Ok(Plan {
        what: what.clone(),
        root,
        items,
        texts,
        candidates,
        trash,
    })
}

impl Plan {
    /// The path moved from.
    pub fn from_path(&self) -> &str {
        &self.root
    }

    /// The note's title or the folder's name: the copy's, unless it gets
    /// another.
    pub fn name(&self) -> &str {
        paths::title(&self.root)
    }

    pub fn is_folder(&self) -> bool {
        matches!(self.what, What::Folder(_))
    }

    /// How many notes go.
    pub fn notes(&self) -> usize {
        self.texts.len()
    }

    /// The files that go (their contents are copied), by id.
    pub fn files(&self) -> Vec<String> {
        self.items
            .iter()
            .filter(|i| i.entry.kind == EntryKind::Attachment)
            .map(|i| i.entry.id.clone())
            .collect()
    }

    /// Where everything goes in `to`: into its folder `folder` (`""`: the
    /// top level), named `name`, under a free name if that is taken (any
    /// case). Inside a folder moved, everything keeps its path; a file
    /// from elsewhere goes beside the note moved, or at the top level of
    /// the folder moved. Wiki links between what moved are rewritten where
    /// they wouldn't find the copy there (Markdown links stay as written).
    pub fn place(&self, to: &VaultReplica, folder: &str, name: &str) -> Result<Placed, PathError> {
        let name = name.trim();
        if name.contains(['/', '\\']) {
            return Err(PathError::BadSegment(name.to_string()));
        }
        let mut taken: BTreeSet<String> = to
            .index
            .resolved_paths()
            .into_keys()
            .chain(to.index.folders())
            .map(|p| p.to_lowercase())
            .collect();
        let folder = match folder.trim().trim_matches('/') {
            "" => String::new(),
            f => paths::normalize(f)?,
        };
        let root = paths::join(&folder, name);
        let (root, base) = match self.what {
            What::Note(_) => {
                let root = paths::normalize_note(&root)?;
                let root = paths::unique(&root, |p| taken.contains(&p.to_lowercase()));
                (root, folder)
            }
            What::Folder(_) => {
                // Not `paths::unique`: a folder's name has no extension.
                let root = paths::normalize(&root)?;
                let root = std::iter::once(root.clone())
                    .chain((2..).map(|n| format!("{root} {n}")))
                    .find(|p| !taken.contains(&p.to_lowercase()))
                    .expect("infinite iterator");
                (root.clone(), root)
            }
        };
        // Nothing there is in a free folder: what was in the folder moved
        // keeps its path, and the files from elsewhere take what is left.
        taken.insert(root.to_lowercase());
        let mut placed: Vec<(&Item, String)> = Vec::new();
        for i in &self.items {
            let path = match &i.place {
                Place::Root => root.clone(),
                Place::Within(rel) => paths::join(&root, rel),
                Place::Beside => continue,
            };
            taken.insert(path.to_lowercase());
            placed.push((i, path));
        }
        for i in self
            .items
            .iter()
            .filter(|i| matches!(i.place, Place::Beside))
        {
            let name = paths::file_name(&i.entry.path);
            let path = paths::unique(&paths::join(&base, name), |p| {
                taken.contains(&p.to_lowercase())
            });
            taken.insert(path.to_lowercase());
            placed.push((i, path));
        }
        // What links may point at there: its own entries and the copies.
        let new_path: BTreeMap<String, String> = placed
            .iter()
            .map(|(i, p)| (i.entry.id.clone(), p.clone()))
            .collect();
        let there: Vec<(String, String)> = to
            .index
            .resolved_paths()
            .into_iter()
            .filter(|(_, e)| e.kind != EntryKind::Folder)
            .map(|(p, e)| (e.id, p))
            .chain(
                placed
                    .iter()
                    .filter(|(i, _)| i.entry.kind != EntryKind::Folder)
                    .map(|(i, p)| (i.entry.id.clone(), p.clone())),
            )
            .collect();
        let text_there = |i: &Item, at: &str| -> String {
            let Some(text) = self.texts.get(&i.entry.id) else {
                return String::new();
            };
            // A wiki link to something that came along that would find
            // nothing or something else there: its name, else its path.
            markdown::rewrite_links_with(text, |l| {
                let id = markdown::resolve(l, &i.entry.path, &self.candidates)?;
                let target = new_path.get(id)?;
                let finds = |t: &str| {
                    let l = markdown::Link {
                        target: t.to_string(),
                        ..l.clone()
                    };
                    markdown::resolve(&l, at, &there) == Some(id)
                };
                if finds(&l.target) {
                    return None;
                }
                let full = if paths::is_note(target) {
                    &target[..target.len() - 3]
                } else {
                    target
                };
                let short = paths::file_name(full);
                Some(if finds(short) { short } else { full }.to_string())
            })
            .unwrap_or_else(|| text.clone())
        };
        let rank = |k: EntryKind| match k {
            EntryKind::Folder => 0,
            EntryKind::Attachment => 1,
            EntryKind::Note => 2,
        };
        placed.sort_by(|(a, pa), (b, pb)| (rank(a.entry.kind), pa).cmp(&(rank(b.entry.kind), pb)));
        let copies = placed
            .iter()
            .map(|(i, path)| Copy {
                from: i.entry.id.clone(),
                kind: i.entry.kind,
                path: path.clone(),
                text: text_there(i, path),
                icon: i.entry.icon.clone(),
                // Among new siblings it has no place yet.
                order: match i.place {
                    Place::Within(_) => i.entry.order.clone(),
                    _ => None,
                },
            })
            .collect();
        Ok(Placed { path: root, copies })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::VaultMeta;
    use crate::keys::{VaultKey, VaultKeyring};
    use crate::protocol::Role;

    fn vault(entries: &[(&str, &str, EntryKind, &str)]) -> VaultReplica {
        let mut ring = VaultKeyring::new("v");
        ring.insert(1, VaultKey::generate());
        let mut r = VaultReplica::new("v", Role::Owner, ring, VaultMeta::named("V"));
        for (id, path, kind, text) in entries {
            r.index.create(id, path, *kind, 1);
            if *kind == EntryKind::Note {
                r.notes.entry(id.to_string()).or_default().set_text(text);
            }
        }
        r
    }

    fn paths_of(p: &Placed) -> Vec<(&str, &str)> {
        p.copies
            .iter()
            .map(|c| (c.from.as_str(), c.path.as_str()))
            .collect()
    }

    use EntryKind::{Attachment as A, Folder as F, Note as N};

    #[test]
    fn a_folder_moves_with_everything_in_it_and_the_files_its_notes_link_to() {
        let from = vault(&[
            ("f", "Projects", F, ""),
            ("g", "Projects/Garden", F, ""),
            ("e", "Projects/Empty", F, ""),
            (
                "n1",
                "Projects/Garden/Beds.md",
                N,
                "![[photo.png]] ![[plan.pdf]]",
            ),
            ("n2", "Projects/Roadmap.md", N, "[[Beds]] and ![[logo.png]]"),
            ("p", "Projects/Garden/photo.png", A, ""),
            ("x", "Projects/notes.txt", A, ""),
            ("plan", "attachments/plan.pdf", A, ""),
            ("logo", "attachments/logo.png", A, ""),
            ("o", "Other.md", N, "uses ![[logo.png]] and ![[photo.png]]"),
            ("o2", "Elsewhere.md", N, "nothing"),
        ]);
        from.index.set_icon("g", Some("🌱"));
        from.index
            .set_order([("n2", Some("a")), ("g", Some("b")), ("f", Some("c"))]);
        let p = plan(&from, &What::Folder("Projects".into())).unwrap();
        assert!(p.is_folder());
        assert_eq!((p.name(), p.notes()), ("Projects", 2));
        let mut files = p.files();
        files.sort();
        assert_eq!(files, ["logo", "p", "plan", "x"]);
        // Files a note staying behind uses stay, copied all the same.
        let mut trash = p.trash.clone();
        trash.sort();
        assert_eq!(trash, ["e", "f", "g", "n1", "n2", "plan", "x"]);

        let to = vault(&[("t", "Archive/old.md", N, "")]);
        let placed = p.place(&to, "/Archive/", "Projects").unwrap();
        assert_eq!(placed.path, "Archive/Projects");
        assert_eq!(
            paths_of(&placed),
            [
                ("f", "Archive/Projects"),
                ("e", "Archive/Projects/Empty"),
                ("g", "Archive/Projects/Garden"),
                ("p", "Archive/Projects/Garden/photo.png"),
                ("logo", "Archive/Projects/logo.png"),
                ("x", "Archive/Projects/notes.txt"),
                ("plan", "Archive/Projects/plan.pdf"),
                ("n1", "Archive/Projects/Garden/Beds.md"),
                ("n2", "Archive/Projects/Roadmap.md"),
            ]
        );
        let g = placed.copies.iter().find(|c| c.from == "g").unwrap();
        assert_eq!(
            (g.icon.as_deref(), g.order.as_deref()),
            (Some("🌱"), Some("b"))
        );
        // The folder moved has no place among its new siblings yet.
        assert_eq!(placed.copies[0].order, None);
        let n1 = placed.copies.iter().find(|c| c.from == "n1").unwrap();
        assert_eq!(n1.text, "![[photo.png]] ![[plan.pdf]]");
    }

    #[test]
    fn names_taken_there_are_avoided_and_links_follow_what_came_along() {
        let from = vault(&[
            (
                "n",
                "Trip/Day one.md",
                N,
                "![[beach.png]], ![[attachments/map.png|the map]], [[Trip/Day two#Lunch]], [[Old]] [x](../Old.md)",
            ),
            ("d", "Trip/Day two.md", N, "[[Day one]]"),
            ("b", "Trip/beach.png", A, ""),
            ("m2", "Trip/map.png", A, ""),
            ("m", "attachments/map.png", A, ""),
            ("o", "Old.md", N, ""),
        ]);
        let p = plan(&from, &What::Folder("Trip".into())).unwrap();
        // A folder of that name, in any case: the copy is "Trip 2". The map
        // from elsewhere takes the next free name beside the one inside.
        let to = vault(&[("x", "trip/old.md", N, ""), ("y", "Day one.md", N, "")]);
        let placed = p.place(&to, "", "Trip").unwrap();
        assert_eq!(placed.path, "Trip 2");
        assert_eq!(
            paths_of(&placed),
            [
                ("b", "Trip 2/beach.png"),
                ("m", "Trip 2/map 2.png"),
                ("m2", "Trip 2/map.png"),
                ("n", "Trip 2/Day one.md"),
                ("d", "Trip 2/Day two.md"),
            ]
        );
        let text = |id: &str| {
            placed
                .copies
                .iter()
                .find(|c| c.from == id)
                .unwrap()
                .text
                .clone()
        };
        // Links that would find nothing or something else there point at
        // the copies (by name, by path where the name is ambiguous there);
        // a link to a note left behind, or a Markdown link, stays.
        assert_eq!(
            text("n"),
            "![[beach.png]], ![[map 2.png|the map]], [[Day two#Lunch]], [[Old]] [x](../Old.md)"
        );
        assert_eq!(text("d"), "[[Trip 2/Day one]]");
    }

    #[test]
    fn a_note_moves_with_the_files_it_links_to() {
        let from = vault(&[
            ("n", "Projects/Plan.md", N, "![[photo.png]] [[Other]]"),
            ("p", "Projects/photo.png", A, ""),
            ("o", "Other.md", N, "![[photo.png]]"),
            ("q", "Projects/unlinked.png", A, ""),
        ]);
        from.index.set_icon("n", Some("📌"));
        let p = plan(&from, &What::Note("n".into())).unwrap();
        assert!(!p.is_folder());
        assert_eq!(
            (p.name(), p.notes(), p.files()),
            ("Plan", 1, vec!["p".to_string()])
        );
        // The photo stays: another note uses it.
        assert_eq!(p.trash, ["n"]);
        let to = vault(&[
            ("t", "Work/photo.png", A, ""),
            ("t2", "Work/Garden.md", N, ""),
        ]);
        let placed = p.place(&to, "Work", "Garden").unwrap();
        assert_eq!(placed.path, "Work/Garden 2.md");
        assert_eq!(
            paths_of(&placed),
            [("p", "Work/photo 2.png"), ("n", "Work/Garden 2.md")]
        );
        let n = &placed.copies[1];
        assert_eq!(n.text, "![[photo 2.png]] [[Other]]");
        assert_eq!(n.icon.as_deref(), Some("📌"));
        assert!(matches!(
            p.place(&to, "", "a/b"),
            Err(PathError::BadSegment(_))
        ));
    }

    #[test]
    fn nothing_moves_that_is_gone_or_has_not_all_arrived() {
        let mut from = vault(&[("n", "Plan.md", N, "x"), ("f", "Empty", F, "")]);
        assert_eq!(
            plan(&from, &What::Note("gone".into())).err(),
            Some(Unmovable::Gone)
        );
        assert_eq!(
            plan(&from, &What::Folder("Nope".into())).err(),
            Some(Unmovable::Gone)
        );
        // A note is no folder, nor a folder a note.
        assert_eq!(
            plan(&from, &What::Folder("Plan.md".into())).err(),
            Some(Unmovable::Gone)
        );
        assert_eq!(
            plan(&from, &What::Note("f".into())).err(),
            Some(Unmovable::Gone)
        );
        // An empty folder moves.
        assert_eq!(
            plan(&from, &What::Folder("Empty".into())).unwrap().trash,
            ["f"]
        );
        from.index.create("m", "Empty/Later.md", N, 1);
        assert_eq!(
            plan(&from, &What::Folder("Empty".into())).err(),
            Some(Unmovable::Loading)
        );
        from.notes.entry("m".into()).or_default();
        assert!(plan(&from, &What::Folder("Empty".into())).is_ok());
    }
}
