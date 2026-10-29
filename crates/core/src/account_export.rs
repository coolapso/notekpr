//! What people take from one server to another: the vaults they choose and
//! their Personal storage (quick notes, saved shapes), as Markdown (and
//! `.drawing` files) in one `.zip` — a folder per vault, one for the quick
//! notes, the saved shapes as an `.excalidrawlib` file, and a small manifest
//! ([`MANIFEST`]) saying which folder is which vault. Importing it creates
//! those vaults again (named as they were, with their icon and picture) and
//! fills the Personal storage. Nothing else comes along: no history,
//! comments (only as readable Markdown under `_comments/`), members or
//! shares — the new server starts fresh.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::engine::VaultMeta;
use crate::{paths, quick};

/// The manifest's path in the `.zip` (hidden: imports into a single vault,
/// and Markdown tools, pass it by).
pub const MANIFEST: &str = ".notekpr-export.json";
/// Format version of [`Manifest`].
pub const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Manifest {
    pub format: u32,
    /// Unix milliseconds.
    pub created_at: i64,
    pub vaults: Vec<ExportedVault>,
    /// The folder holding the quick notes, when they were exported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quick_notes: Option<String>,
    /// The file holding the saved shapes (an `.excalidrawlib`, beside the
    /// folders), when they were exported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_shapes: Option<String>,
}

/// What the saved shapes' file is called in an export.
pub const SAVED_SHAPES_FILE: &str = "Saved shapes.excalidrawlib";

/// One vault of an export.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ExportedVault {
    /// Its folder in the `.zip`.
    pub folder: String,
    /// Its name, description, icon and picture.
    pub meta: VaultMeta,
    /// Icons of its notes, files and folders, by path.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub icons: BTreeMap<String, String>,
}

/// Folder names for vaults with these names, in order: the names, with what
/// some file systems can't store replaced, unique (ignoring case) and never
/// the quick notes' own folder.
pub fn folder_names(names: &[&str]) -> Vec<String> {
    let mut used: BTreeSet<String> = BTreeSet::from([quick::NAME.to_lowercase()]);
    names
        .iter()
        .map(|name| {
            let base: String = name
                .chars()
                .map(|c| {
                    if c.is_control() || "/\\:*?\"<>|".contains(c) {
                        '_'
                    } else {
                        c
                    }
                })
                .collect::<String>()
                .trim_matches(|c: char| c == '.' || c.is_whitespace())
                .to_string();
            let base = if base.is_empty() {
                "Vault".to_string()
            } else {
                base
            };
            let name = unique_name(&base, |n| used.contains(&n.to_lowercase()));
            used.insert(name.to_lowercase());
            name
        })
        .collect()
}

/// `name`, or `name 2`, `name 3`, … whichever isn't `taken`.
pub fn unique_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_string();
    }
    (2..)
        .map(|n| format!("{name} {n}"))
        .find(|n| !taken(n))
        .expect("an unused number")
}

/// Files as unzipped: path and bytes.
pub type Files = Vec<(String, Vec<u8>)>;

/// An export read back: each vault's files (paths within the vault), the
/// quick notes' files, the saved shapes' file, and how many files belonged
/// to none.
#[derive(Clone, Debug, Default)]
pub struct Unpacked {
    pub vaults: Vec<(ExportedVault, Files)>,
    pub quick_notes: Option<Files>,
    pub saved_shapes: Option<Vec<u8>>,
    pub other_files: usize,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UnpackError {
    #[error(
        "this isn't an export of vaults and quick notes (it has no {MANIFEST}): import it into a vault instead"
    )]
    NoManifest,
    #[error("this export is from a newer version of notekpr: update to import it")]
    Newer,
    #[error("the export's {MANIFEST} is damaged: {0}")]
    Damaged(String),
}

/// Sort an export's files (as unzipped: `path`, bytes) by vault, from its
/// manifest — also when the `.zip` was made again around a folder holding
/// the export.
pub fn unpack(files: Files) -> Result<Unpacked, UnpackError> {
    let at = files
        .iter()
        .filter(|(p, _)| p == MANIFEST || p.strip_suffix(MANIFEST).is_some_and(is_top_folder))
        .min_by_key(|(p, _)| p.len())
        .ok_or(UnpackError::NoManifest)?;
    let prefix = at.0[..at.0.len() - MANIFEST.len()].to_string();
    let manifest: serde_json::Value =
        serde_json::from_slice(&at.1).map_err(|e| UnpackError::Damaged(e.to_string()))?;
    if manifest
        .get("format")
        .and_then(|f| f.as_u64())
        .is_some_and(|f| f > FORMAT as u64)
    {
        return Err(UnpackError::Newer);
    }
    let manifest: Manifest =
        serde_json::from_value(manifest).map_err(|e| UnpackError::Damaged(e.to_string()))?;
    let mut by_folder: BTreeMap<String, Files> = BTreeMap::new();
    let mut other_files = 0;
    let mut saved_shapes = None;
    for (path, data) in files {
        let Some(rest) = path.strip_prefix(&prefix) else {
            other_files += 1;
            continue;
        };
        if rest == MANIFEST {
            continue;
        }
        if manifest.saved_shapes.as_deref() == Some(rest) {
            saved_shapes = Some(data);
            continue;
        }
        match rest.split_once('/') {
            Some((folder, inner)) if !inner.is_empty() => by_folder
                .entry(folder.to_string())
                .or_default()
                .push((inner.to_string(), data)),
            _ => other_files += 1,
        }
    }
    let mut out = Unpacked::default();
    for v in manifest.vaults {
        let files = by_folder.remove(&v.folder).unwrap_or_default();
        out.vaults.push((v, files));
    }
    if let Some(q) = &manifest.quick_notes {
        out.quick_notes = Some(by_folder.remove(q).unwrap_or_default());
    }
    out.saved_shapes = saved_shapes;
    out.other_files = other_files + by_folder.values().map(Vec::len).sum::<usize>();
    Ok(out)
}

/// `prefix` (what comes before the manifest) is one folder: `Export/`.
fn is_top_folder(prefix: &str) -> bool {
    prefix
        .strip_suffix('/')
        .is_some_and(|f| !f.is_empty() && !f.contains('/'))
}

/// Whether a path of an export is an empty folder's placeholder
/// (`folder/.keep`): its folder, if so.
pub fn kept_folder(path: &str) -> Option<&str> {
    path.strip_suffix("/.keep")
        .filter(|f| paths::normalize(f).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(p: &str) -> (String, Vec<u8>) {
        (p.to_string(), p.as_bytes().to_vec())
    }

    fn manifest(vaults: &[(&str, &str)], quick: Option<&str>) -> Vec<u8> {
        with_shapes(vaults, quick, None)
    }

    fn with_shapes(vaults: &[(&str, &str)], quick: Option<&str>, s: Option<&str>) -> Vec<u8> {
        serde_json::to_vec(&Manifest {
            format: FORMAT,
            created_at: 1,
            vaults: vaults
                .iter()
                .map(|(folder, name)| ExportedVault {
                    folder: folder.to_string(),
                    meta: VaultMeta::named(name),
                    icons: BTreeMap::from([("Plan.md".to_string(), "📌".to_string())]),
                })
                .collect(),
            quick_notes: quick.map(str::to_string),
            saved_shapes: s.map(str::to_string),
        })
        .unwrap()
    }

    #[test]
    fn folders_are_safe_unique_and_never_the_quick_notes() {
        assert_eq!(
            folder_names(&[
                "Family",
                "family",
                "a/b: c?",
                " .. ",
                "Quick notes",
                "Family",
                "Drawings"
            ]),
            [
                "Family",
                "family 2",
                "a_b_ c_",
                "Vault",
                "Quick notes 2",
                "Family 3",
                "Drawings"
            ]
        );
        assert_eq!(
            unique_name("Work", |n| n == "Work" || n == "Work 2"),
            "Work 3"
        );
        assert_eq!(unique_name("Work", |_| false), "Work");
    }

    #[test]
    fn an_export_is_sorted_by_vault() {
        let files = vec![
            (
                MANIFEST.to_string(),
                manifest(
                    &[("Family", "Family: ours"), ("Work", "Work")],
                    Some("Quick notes"),
                ),
            ),
            file("Family/Plan.md"),
            file("Family/Trips/2026.md"),
            file("Family/Empty/.keep"),
            file("Work/Notes.md"),
            file("Quick notes/2026-10-27 14.32.md"),
            file("Stray.md"),
            file("Elsewhere/x.md"),
        ];
        let u = unpack(files).unwrap();
        assert_eq!(u.vaults.len(), 2);
        assert_eq!(u.vaults[0].0.meta.name, "Family: ours");
        assert_eq!(u.vaults[0].0.icons["Plan.md"], "📌");
        let paths: Vec<&str> = u.vaults[0].1.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, ["Plan.md", "Trips/2026.md", "Empty/.keep"]);
        assert_eq!(u.vaults[1].1.len(), 1);
        assert_eq!(u.quick_notes.unwrap()[0].0, "2026-10-27 14.32.md");
        assert_eq!(u.other_files, 2);
        assert_eq!(kept_folder("Empty/.keep"), Some("Empty"));
        assert_eq!(kept_folder("Plan.md"), None);
    }

    #[test]
    fn the_saved_shapes_are_a_file_beside_the_folders() {
        let files = vec![
            (
                format!("Export/{MANIFEST}"),
                with_shapes(
                    &[("Work", "Work")],
                    Some("Quick notes"),
                    Some(SAVED_SHAPES_FILE),
                ),
            ),
            file("Export/Work/Plan.drawing"),
            file(&format!("Export/{SAVED_SHAPES_FILE}")),
        ];
        let u = unpack(files).unwrap();
        assert_eq!(
            u.saved_shapes.as_deref(),
            Some(format!("Export/{SAVED_SHAPES_FILE}").as_bytes())
        );
        assert_eq!(u.vaults[0].1[0].0, "Plan.drawing");
        assert_eq!((u.quick_notes.unwrap().len(), u.other_files), (0, 0));
        // Exports without them (0.6.0's) have none.
        let u = unpack(vec![(MANIFEST.to_string(), manifest(&[], None))]).unwrap();
        assert!(u.saved_shapes.is_none());
    }

    #[test]
    fn an_export_zipped_again_around_its_folder_reads_the_same() {
        let files = vec![
            (
                format!("Export/{MANIFEST}"),
                manifest(&[("Work", "Work")], None),
            ),
            file("Export/Work/Notes.md"),
        ];
        let u = unpack(files).unwrap();
        assert_eq!(u.vaults[0].1[0].0, "Notes.md");
        assert!(u.quick_notes.is_none());
    }

    #[test]
    fn what_is_not_an_export_says_so() {
        assert_eq!(
            unpack(vec![file("Plan.md")]).err(),
            Some(UnpackError::NoManifest)
        );
        // Deeper than one folder isn't taken for a manifest either.
        assert_eq!(
            unpack(vec![(format!("a/b/{MANIFEST}"), manifest(&[], None))]).err(),
            Some(UnpackError::NoManifest)
        );
        let newer = br#"{"format": 99, "created_at": 1, "vaults": []}"#.to_vec();
        assert_eq!(
            unpack(vec![(MANIFEST.to_string(), newer)]).err(),
            Some(UnpackError::Newer)
        );
        assert!(matches!(
            unpack(vec![(MANIFEST.to_string(), b"{".to_vec())]),
            Err(UnpackError::Damaged(_))
        ));
    }
}
