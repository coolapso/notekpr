//! A note's attachments: where files added to it go, how it links to them,
//! and what goes along when notes move within their vault.
//!
//! Files added to a note (or drawing) go to `attachments/` in its folder
//! ([`folder_for`]; `attachments/` at the top for one there), each under a
//! name no other file of the vault has ([`new_path`]): a link by name
//! (`![[photo.png]]`) finds it, here and in Obsidian, which resolves links
//! the same way. The quick notes are flat: the apps keep their files at
//! the top.
//!
//! Moving a note or drawing, or moving or renaming a folder ([`along`]),
//! takes along the attachments only what moves uses — links to, embeds,
//! shows as a picture — wherever they are: to `attachments/` in the folder
//! it ends up in, or, from beside it, to beside it again. Files something
//! staying behind uses too stay where they are.
//! Links keep finding what they found: a wiki link that would find
//! something else after the move (another file with the same name), or
//! nothing, is rewritten — to the name, else the path ([`link_target`]).
//! Markdown links stay as written.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::doc::{Entry, EntryKind};
use crate::engine::VaultReplica;
use crate::markdown::{self, Link, LinkKind, Resolver};
use crate::paths;
use crate::pictures;

/// The name of the folder a note's files go in.
pub const FOLDER: &str = "attachments";

/// The folder files added to the note (or drawing) at `path` go to.
pub fn folder_for(path: &str) -> String {
    paths::join(paths::parent(path), FOLDER)
}

/// The path for a new file `name` in `folder`: that name, else a free one
/// (`photo 2.png`), so that no other entry of the vault (`taken`, their
/// paths) has its path or its name, in any case.
pub fn new_path<'a>(folder: &str, name: &str, taken: impl IntoIterator<Item = &'a str>) -> String {
    let mut paths_taken = HashSet::new();
    let mut names = HashSet::new();
    for p in taken {
        let p = p.to_lowercase();
        names.insert(paths::file_name(&p).to_string());
        paths_taken.insert(p);
    }
    paths::unique(&paths::join(folder, name), |p| {
        let p = p.to_lowercase();
        names.contains(paths::file_name(&p)) || paths_taken.contains(&p)
    })
}

/// The shortest wiki link target that finds the entry `id` at `path` from
/// the note at `from`, among `links`: its name (a note's without `.md`),
/// else its path.
pub fn link_target(links: &Resolver, id: &str, path: &str, from: &str) -> String {
    let full = if paths::is_note(path) {
        &path[..path.len() - 3]
    } else {
        path
    };
    let name = paths::file_name(full);
    let link = Link {
        kind: LinkKind::Wiki,
        target: name.to_string(),
        heading: None,
        alias: None,
        embed: false,
        start: 0,
        end: 0,
    };
    if links.resolve(&link, from) == Some(id) {
        name.to_string()
    } else {
        full.to_string()
    }
}

/// A move within a vault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Move {
    /// A note, drawing or file (by id) to this path.
    Entry { id: String, to: String },
    /// A folder, with everything in it, to this path.
    Folder { from: String, to: String },
}

/// What else a move changes, from [`along`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Along {
    /// The attachments that go along: id and new path.
    pub files: Vec<(String, String)>,
    /// The notes whose links are rewritten to keep finding what they
    /// found: id and new text.
    pub texts: Vec<(String, String)>,
}

/// The deepest folder holding all of `paths`.
fn common_folder<'a>(mut paths: impl Iterator<Item = &'a str>) -> String {
    let Some(first) = paths.next() else {
        return String::new();
    };
    let mut common: Vec<&str> = match paths::parent(first) {
        "" => vec![],
        f => f.split('/').collect(),
    };
    for p in paths {
        let segs: Vec<&str> = match paths::parent(p) {
            "" => vec![],
            f => f.split('/').collect(),
        };
        let same = common.iter().zip(&segs).take_while(|(a, b)| a == b).count();
        common.truncate(same);
    }
    common.join("/")
}

/// What moving `m` in `r` changes besides it: the attachments that go
/// along, and the links rewritten to keep finding what they found (only
/// links to entries `keep_links_to` says, links to files that go along
/// always). Worked out before the move, applied with it. Where a note or
/// drawing staying behind may use a file but can't tell yet (some of it
/// waits for a key), no file goes along.
pub fn along(r: &VaultReplica, m: &Move, keep_links_to: impl Fn(&Entry) -> bool) -> Along {
    let entries: Vec<Entry> = r
        .index
        .resolved_paths()
        .into_iter()
        .map(|(p, mut e)| {
            e.path = p;
            e
        })
        .collect();
    let by_id: HashMap<&str, &Entry> = entries.iter().map(|e| (e.id.as_str(), e)).collect();
    // Where what moves goes.
    let mut new_path: HashMap<String, String> = match m {
        Move::Entry { id, to } => by_id
            .get(id.as_str())
            .filter(|e| &e.path != to)
            .map(|e| (e.id.clone(), to.clone()))
            .into_iter()
            .collect(),
        Move::Folder { from, to } if !from.is_empty() && from != to => entries
            .iter()
            .filter(|e| &e.path == from || paths::is_within(&e.path, from))
            .map(|e| (e.id.clone(), format!("{to}{}", &e.path[from.len()..])))
            .collect(),
        Move::Folder { .. } => HashMap::new(),
    };
    if new_path.is_empty() {
        return Along::default();
    }
    let before = Resolver::new(
        entries
            .iter()
            .filter(|e| e.kind != EntryKind::Folder)
            .map(|e| (e.id.clone(), e.path.clone()))
            .collect(),
    );
    // Who uses each file: the notes linking to it, the drawings showing it.
    // (Some of a note's or drawing's content waiting for a key: unsure.)
    let mut users: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut unsure = false;
    for e in &entries {
        if !matches!(e.kind, EntryKind::Note | EntryKind::Drawing) {
            continue;
        }
        unsure |= !r.is_complete(&e.id);
        if e.kind == EntryKind::Note {
            for f in markdown::linked_ids(&r.note_text(&e.id), &e.path, &before) {
                users.entry(f).or_default().insert(e.id.clone());
            }
        } else if let Some(doc) = r.notes.get(&e.id) {
            for f in doc.drawing().files().values() {
                if let Some(f) = pictures::attachment_of(f) {
                    users.entry(f.to_string()).or_default().insert(e.id.clone());
                }
            }
        }
    }
    // The files only what moves uses go along.
    let mut going: Vec<(&Entry, String)> = Vec::new();
    if !unsure {
        for (file, used_by) in &users {
            let Some(f) = by_id.get(file.as_str()) else {
                continue;
            };
            if f.kind != EntryKind::Attachment
                || new_path.contains_key(file)
                || !used_by.iter().all(|u| new_path.contains_key(u))
            {
                continue;
            }
            let from = common_folder(used_by.iter().map(|u| by_id[u.as_str()].path.as_str()));
            let to = common_folder(used_by.iter().map(|u| new_path[u].as_str()));
            if from == to {
                continue;
            }
            let dir = if paths::parent(&f.path) == from {
                to
            } else {
                paths::join(&to, FOLDER)
            };
            let dest = paths::join(&dir, paths::file_name(&f.path));
            if dest != f.path {
                going.push((f, dest));
            }
        }
    }
    // Under a free name where theirs is taken there.
    let mut taken: HashSet<String> = entries
        .iter()
        .map(|e| new_path.get(&e.id).unwrap_or(&e.path).to_lowercase())
        .collect();
    going.sort_by(|a, b| a.0.path.cmp(&b.0.path));
    let mut files = Vec::new();
    for (f, dest) in going {
        taken.remove(&f.path.to_lowercase());
        let dest = paths::unique(&dest, |p| taken.contains(&p.to_lowercase()));
        taken.insert(dest.to_lowercase());
        new_path.insert(f.id.clone(), dest.clone());
        files.push((f.id.clone(), dest));
    }
    // Links that would find something else, or nothing, find it again.
    let after = Resolver::new(
        entries
            .iter()
            .filter(|e| e.kind != EntryKind::Folder)
            .map(|e| (e.id.clone(), new_path.get(&e.id).unwrap_or(&e.path).clone()))
            .collect(),
    );
    let went: HashSet<&str> = files.iter().map(|(id, _)| id.as_str()).collect();
    let mut texts = Vec::new();
    for e in entries.iter().filter(|e| e.kind == EntryKind::Note) {
        let at = new_path.get(&e.id).unwrap_or(&e.path);
        let text = r.note_text(&e.id);
        let rewritten = markdown::rewrite_links_with(&text, |l| {
            if l.kind != LinkKind::Wiki {
                return None;
            }
            let found = before.resolve(l, &e.path)?;
            if after.resolve(l, at) == Some(found) {
                return None;
            }
            let target = by_id.get(found)?;
            if !went.contains(found) && !keep_links_to(target) {
                return None;
            }
            let path = new_path.get(found).unwrap_or(&target.path);
            Some(link_target(&after, found, path, at))
        });
        if let Some(t) = rewritten {
            texts.push((e.id.clone(), t));
        }
    }
    Along { files, texts }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::VaultMeta;
    use crate::keys::{VaultKey, VaultKeyring};
    use crate::protocol::Role;
    use notekpr_draw::scene::Scene;

    const N: EntryKind = EntryKind::Note;
    const A: EntryKind = EntryKind::Attachment;
    const D: EntryKind = EntryKind::Drawing;
    const F: EntryKind = EntryKind::Folder;

    fn vault(entries: &[(&str, &str, EntryKind, &str)]) -> VaultReplica {
        let mut ring = VaultKeyring::new("v");
        ring.insert(1, VaultKey::generate());
        let mut r = VaultReplica::new("v", Role::Owner, ring, VaultMeta::named("V"));
        for (id, path, kind, text) in entries {
            r.index.create(id, path, *kind, 1);
            if *kind == N {
                r.notes.entry(id.to_string()).or_default().set_text(text);
            }
        }
        r
    }

    /// A drawing `id` showing the files `shows`.
    fn draw(r: &mut VaultReplica, id: &str, shows: &[&str]) {
        let mut scene = Scene::default();
        for (i, att) in shows.iter().enumerate() {
            let fid = format!("f{i}");
            scene
                .files
                .insert(fid.clone(), pictures::entry(&fid, "image/png", att, 1.0));
        }
        r.notes
            .entry(id.to_string())
            .or_default()
            .drawing()
            .set_scene(&scene);
    }

    fn note(id: &str, to: &str) -> Move {
        Move::Entry {
            id: id.into(),
            to: to.into(),
        }
    }

    fn folder(from: &str, to: &str) -> Move {
        Move::Folder {
            from: from.into(),
            to: to.into(),
        }
    }

    fn all(_: &Entry) -> bool {
        true
    }

    /// The files that go along, by id.
    fn files(a: &Along) -> Vec<(&str, &str)> {
        let mut f: Vec<(&str, &str)> = a
            .files
            .iter()
            .map(|(id, p)| (id.as_str(), p.as_str()))
            .collect();
        f.sort();
        f
    }

    fn text<'a>(a: &'a Along, id: &str) -> Option<&'a str> {
        a.texts
            .iter()
            .find(|(n, _)| n == id)
            .map(|(_, t)| t.as_str())
    }

    #[test]
    fn new_files_go_beside_their_note_under_a_name_of_their_own() {
        assert_eq!(folder_for("notekpr/TODO.md"), "notekpr/attachments");
        assert_eq!(folder_for("TODO.md"), "attachments");
        let taken = [
            "Trips/attachments/photo.png",
            "notekpr/attachments/Map.PNG",
            "notekpr/attachments",
        ];
        // A name another file has anywhere, in any case, is taken: a link
        // by name finds only the new one.
        assert_eq!(
            new_path("notekpr/attachments", "photo.png", taken),
            "notekpr/attachments/photo 2.png"
        );
        assert_eq!(
            new_path("notekpr/attachments", "map.png", taken),
            "notekpr/attachments/map 2.png"
        );
        assert_eq!(
            new_path("notekpr/attachments", "beach.png", taken),
            "notekpr/attachments/beach.png"
        );
    }

    #[test]
    fn a_link_is_the_name_where_that_finds_it_else_the_path() {
        let links = Resolver::new(vec![
            ("a".into(), "attachments/photo.png".into()),
            ("b".into(), "notekpr/attachments/photo.png".into()),
            ("c".into(), "notekpr/attachments/map.png".into()),
            ("n".into(), "notekpr/Plan.md".into()),
            ("m".into(), "Plan.md".into()),
        ]);
        let from = "notekpr/TODO.md";
        assert_eq!(
            link_target(&links, "c", "notekpr/attachments/map.png", from),
            "map.png"
        );
        assert_eq!(
            link_target(&links, "a", "attachments/photo.png", from),
            "photo.png"
        );
        // The shortest path wins a link by name: the other needs its path.
        assert_eq!(
            link_target(&links, "b", "notekpr/attachments/photo.png", from),
            "notekpr/attachments/photo.png"
        );
        assert_eq!(link_target(&links, "m", "Plan.md", from), "Plan");
        assert_eq!(
            link_target(&links, "n", "notekpr/Plan.md", from),
            "notekpr/Plan"
        );
    }

    #[test]
    fn a_note_takes_the_files_only_it_uses() {
        let r = vault(&[
            (
                "todo",
                "notekpr/TODO.md",
                N,
                "![[shot.png]] ![[logo.png]] [[spec.pdf]]",
            ),
            ("other", "Other.md", N, "![[logo.png]]"),
            // Today's place, the top's attachments/.
            ("shot", "attachments/shot.png", A, ""),
            // Another note uses it too.
            ("logo", "attachments/logo.png", A, ""),
            // Put in a folder of documents by hand: it goes along too.
            ("spec", "Docs/spec.pdf", A, ""),
            ("lone", "attachments/lone.png", A, ""),
        ]);
        let a = along(&r, &note("todo", "Archive/TODO.md"), all);
        assert_eq!(
            files(&a),
            [
                ("shot", "Archive/attachments/shot.png"),
                ("spec", "Archive/attachments/spec.pdf"),
            ]
        );
        // Links by name still find them.
        assert!(a.texts.is_empty(), "{:?}", a.texts);
        // To the top: the shot is in attachments/ there already.
        let a = along(&r, &note("todo", "TODO.md"), all);
        assert_eq!(files(&a), [("spec", "attachments/spec.pdf")]);
        // Renamed in its folder: nothing goes along.
        assert!(
            along(&r, &note("todo", "notekpr/Plans.md"), all)
                .files
                .is_empty()
        );
    }

    #[test]
    fn files_beside_a_note_stay_beside_it_and_its_own_attachments_follow() {
        let r = vault(&[
            (
                "todo",
                "notekpr/TODO.md",
                N,
                "![[a.png]] ![[b.png]] ![[c.png]]",
            ),
            ("a", "notekpr/a.png", A, ""),
            ("b", "notekpr/attachments/b.png", A, ""),
            // In another folder's attachments: still the note's own.
            ("c", "Trips/attachments/c.png", A, ""),
        ]);
        let a = along(&r, &note("todo", "Work/Plans/TODO.md"), all);
        assert_eq!(
            files(&a),
            [
                ("a", "Work/Plans/a.png"),
                ("b", "Work/Plans/attachments/b.png"),
                ("c", "Work/Plans/attachments/c.png"),
            ]
        );
        // A root note's go to the top's.
        let a = along(&r, &note("todo", "TODO.md"), all);
        assert_eq!(
            files(&a),
            [
                ("a", "a.png"),
                ("b", "attachments/b.png"),
                ("c", "attachments/c.png"),
            ]
        );
    }

    #[test]
    fn a_file_a_drawing_shows_goes_with_it_alone() {
        let mut r = vault(&[
            ("sk", "Garden/Beds.drawing", D, ""),
            ("other", "Plan.drawing", D, ""),
            ("p1", "attachments/Beds.png", A, ""),
            ("p2", "attachments/Shared.png", A, ""),
            ("n", "Notes.md", N, "![[Beds.png]]"),
        ]);
        draw(&mut r, "sk", &["p1", "p2"]);
        draw(&mut r, "other", &["p2"]);
        // A note links to p1, so p1 stays too.
        assert!(
            along(&r, &note("sk", "Yard/Beds.drawing"), all)
                .files
                .is_empty()
        );
        r.notes
            .get_mut("n")
            .unwrap()
            .set_text("no picture any more");
        let a = along(&r, &note("sk", "Yard/Beds.drawing"), all);
        assert_eq!(files(&a), [("p1", "Yard/attachments/Beds.png")]);
    }

    #[test]
    fn a_folder_moved_or_renamed_takes_its_notes_files_from_elsewhere() {
        let r = vault(&[
            ("f", "notekpr", F, ""),
            ("todo", "notekpr/TODO.md", N, "![[shot.png]] ![[both.png]]"),
            (
                "deep",
                "notekpr/sub/Deep.md",
                N,
                "![[both.png]] ![[in.png]]",
            ),
            ("in", "notekpr/attachments/in.png", A, ""),
            ("shot", "attachments/shot.png", A, ""),
            ("both", "attachments/both.png", A, ""),
        ]);
        let a = along(&r, &folder("notekpr", "Projects/notekpr"), all);
        // `in` moves with the folder anyway; `both` to the folder both
        // notes are in.
        assert_eq!(
            files(&a),
            [
                ("both", "Projects/notekpr/attachments/both.png"),
                ("shot", "Projects/notekpr/attachments/shot.png"),
            ]
        );
        let a = along(&r, &folder("notekpr", "Renamed"), all);
        assert_eq!(
            files(&a),
            [
                ("both", "Renamed/attachments/both.png"),
                ("shot", "Renamed/attachments/shot.png"),
            ]
        );
        assert!(a.texts.is_empty());
    }

    #[test]
    fn a_name_taken_there_gets_another_and_the_links_follow() {
        let r = vault(&[
            (
                "todo",
                "notekpr/TODO.md",
                N,
                "![[shot.png|the shot]] and [[shot.png#x]]",
            ),
            ("shot", "attachments/shot.png", A, ""),
            ("there", "Archive/attachments/shot.png", A, ""),
            (
                "old",
                "Archive/Old.md",
                N,
                "![[Archive/attachments/shot.png]]",
            ),
        ]);
        let a = along(&r, &note("todo", "Archive/TODO.md"), all);
        assert_eq!(files(&a), [("shot", "Archive/attachments/shot 2.png")]);
        assert_eq!(
            text(&a, "todo"),
            Some("![[shot 2.png|the shot]] and [[shot 2.png#x]]")
        );
        // Old's link still finds the file that was there.
        assert_eq!(text(&a, "old"), None);
    }

    #[test]
    fn links_never_turn_to_another_file_with_the_same_name() {
        let r = vault(&[
            ("todo", "notekpr/TODO.md", N, "![[photo.png]]"),
            ("trip", "Trips/2026/Lisbon.md", N, "![[photo.png]]"),
            // The trip's photo has the shortest path: links by name find it.
            ("tp", "Trips/2026/photo.png", A, ""),
            ("np", "notekpr/attachments/photo.png", A, ""),
        ]);
        // TODO's link finds the trip's photo too, so it stays.
        let a = along(&r, &note("trip", "Travel/Portugal/2026/Lisbon.md"), all);
        assert!(a.files.is_empty());
        assert!(a.texts.is_empty());
        let r = vault(&[
            (
                "todo",
                "notekpr/TODO.md",
                N,
                "![[notekpr/attachments/photo.png]] [[a/photo.png]]",
            ),
            ("trip", "Trips/2026/Lisbon.md", N, "![[photo.png]]"),
            ("tp", "Trips/2026/photo.png", A, ""),
            ("np", "notekpr/attachments/photo.png", A, ""),
        ]);
        let a = along(&r, &note("trip", "Travel/Portugal/2026/Lisbon.md"), all);
        assert_eq!(files(&a), [("tp", "Travel/Portugal/2026/photo.png")]);
        // By name, the trip's link would find np now: it gets the path.
        assert_eq!(
            text(&a, "trip"),
            Some("![[Travel/Portugal/2026/photo.png]]")
        );
        // A link by path still finds np; one to nothing stays as it is.
        assert_eq!(text(&a, "todo"), None);
    }

    #[test]
    fn moving_a_file_keeps_links_to_it_and_to_its_namesakes() {
        let r = vault(&[
            (
                "n",
                "Notes/A.md",
                N,
                "![[pic.png]] ![[Long/way/down/pic.png]]",
            ),
            ("short", "Notes/pic.png", A, ""),
            ("long", "Long/way/down/pic.png", A, ""),
        ]);
        // The short one goes deep: by name, `long` would win.
        let a = along(&r, &note("short", "Deeper/than/all/of/it/pic.png"), all);
        assert!(a.files.is_empty());
        assert_eq!(
            text(&a, "n"),
            Some("![[Deeper/than/all/of/it/pic.png]] ![[Long/way/down/pic.png]]")
        );
    }

    #[test]
    fn links_to_moved_notes_follow_them_where_asked() {
        let r = vault(&[
            ("plan", "a/Plan.md", N, "[[#Top]] and [[b/Plan]]"),
            ("other", "b/Plan.md", N, ""),
            (
                "idx",
                "Index.md",
                N,
                "[[Plan]], [[a/Plan|the plan]], [[Missing]]",
            ),
        ]);
        let a = along(&r, &note("plan", "zzz/later/Plan.md"), all);
        assert!(a.files.is_empty());
        // By name, Plan would find b/Plan now (the shortest path).
        assert_eq!(
            text(&a, "idx"),
            Some("[[zzz/later/Plan]], [[zzz/later/Plan|the plan]], [[Missing]]")
        );
        assert_eq!(text(&a, "plan"), None);
        // Only files' links, as asked.
        let a = along(&r, &note("plan", "zzz/later/Plan.md"), |e| {
            e.kind == EntryKind::Attachment
        });
        assert!(a.texts.is_empty());
    }

    #[test]
    fn markdown_links_stay_as_written() {
        let r = vault(&[
            (
                "todo",
                "notekpr/TODO.md",
                N,
                "![x](../attachments/shot.png)",
            ),
            ("shot", "attachments/shot.png", A, ""),
        ]);
        let a = along(&r, &note("todo", "Archive/deep/TODO.md"), all);
        assert_eq!(files(&a), [("shot", "Archive/deep/attachments/shot.png")]);
        assert!(a.texts.is_empty());
    }

    #[test]
    fn nothing_goes_along_while_a_user_may_be_missing() {
        let mut r = vault(&[
            ("todo", "notekpr/TODO.md", N, "![[shot.png]]"),
            ("shot", "attachments/shot.png", A, ""),
            ("sk", "Sketch.drawing", D, ""),
        ]);
        // A drawing nothing of which came shows nothing.
        let a = along(&r, &note("todo", "Archive/TODO.md"), all);
        assert_eq!(files(&a), [("shot", "Archive/attachments/shot.png")]);
        // Some of it waits for a key: it might show the file.
        r.dropped.insert(7, "sk".into());
        let a = along(&r, &note("todo", "Archive/TODO.md"), all);
        assert!(a.files.is_empty());
        r.dropped.clear();
        draw(&mut r, "sk", &[]);
        let a = along(&r, &note("todo", "Archive/TODO.md"), all);
        assert_eq!(files(&a), [("shot", "Archive/attachments/shot.png")]);
        // A note too.
        r.dropped.insert(8, "todo".into());
        assert!(
            along(&r, &note("todo", "Gone/TODO.md"), all)
                .files
                .is_empty()
        );
    }

    #[test]
    fn quick_notes_and_nothing_moved_change_nothing() {
        let r = vault(&[
            ("q", "2026-11-04 09.15.md", N, "![[shot.png]]"),
            ("shot", "shot.png", A, ""),
        ]);
        // The quick notes are flat: a note is only renamed.
        assert_eq!(along(&r, &note("q", "Groceries.md"), all), Along::default());
        assert_eq!(
            along(&r, &note("q", "2026-11-04 09.15.md"), all),
            Along::default()
        );
        assert_eq!(along(&r, &note("gone", "x.md"), all), Along::default());
        assert_eq!(along(&r, &folder("", "x"), all), Along::default());
    }
}
