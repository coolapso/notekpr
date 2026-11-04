//! A note's attachments: where files added to it go, and how it links to
//! them.
//!
//! Files added to a note (or drawing) go to `attachments/` in its folder
//! ([`folder_for`]; `attachments/` at the top for one there), each under a
//! name no other file of the vault has ([`new_path`]): a link by name
//! (`![[photo.png]]`) finds it, here and in Obsidian, which resolves links
//! the same way. The quick notes are flat: the apps keep their files at
//! the top.

use std::collections::HashSet;

use crate::markdown::{Link, LinkKind, Resolver};
use crate::paths;

/// The name of the folder a note's files go in.
pub const FOLDER: &str = "attachments";

/// The folder files added to the note (or drawing) at `path` go to.
pub fn folder_for(path: &str) -> String {
    paths::join(paths::parent(path), FOLDER)
}

/// Whether the file at `path` is in an attachments folder.
pub fn in_attachments(path: &str) -> bool {
    paths::file_name(paths::parent(path)).eq_ignore_ascii_case(FOLDER)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_files_go_beside_their_note_under_a_name_of_their_own() {
        assert_eq!(folder_for("notekpr/TODO.md"), "notekpr/attachments");
        assert_eq!(folder_for("TODO.md"), "attachments");
        assert!(in_attachments("notekpr/attachments/a.png"));
        assert!(in_attachments("Attachments/a.png"));
        assert!(!in_attachments("attachments"));
        assert!(!in_attachments("Docs/a.png"));
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
}
