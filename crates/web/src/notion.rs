//! Importing from Notion: the HTML exports of pages or databases (their
//! `.zip`s), converted here by `notekpr-notion` and brought in like any
//! other files, into a folder of their own (`Notion`, `Notion 2`, …). What
//! an earlier import brought (the notes' `notion:` IDs) isn't imported
//! again, and links find it.

use std::collections::{BTreeSet, HashMap};

use notekpr_core::doc::EntryKind;
use notekpr_core::paths;
use notekpr_notion::{Export, Files, Item, Options};

use crate::state::{Route, navigate, step, success};
use crate::{actions, knowledge, util, zipfile};

/// Exports read, ready to show what they hold and to import.
pub struct Picked {
    pub export: Export,
    pub opts: Options,
    pub items: Vec<Item>,
}

/// Read the `.zip`s picked (and the zips Notion puts inside big ones) for
/// importing into `vault`.
pub async fn read(vault: &str, files: Vec<web_sys::File>) -> Result<Picked, String> {
    let mut exports = Vec::new();
    for f in files {
        let name = f.name();
        let Some(bytes) = util::read_file(&f).await else {
            return Err(format!("Could not read {name}."));
        };
        let mut entries = Vec::new();
        unzip(&name, &bytes, &mut entries)?;
        exports.push(entries);
        // Let the page breathe between big files.
        util::sleep(0).await;
    }
    let export = Export::read(exports).map_err(|e| sentence(&e.to_string()))?;
    let opts = options(vault);
    let items = export.items(&opts);
    Ok(Picked {
        export,
        opts,
        items,
    })
}

/// `s` as a sentence: a capital first, a full stop last.
fn sentence(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => format!("{}{}.", first.to_uppercase(), c.as_str()),
        None => String::new(),
    }
}

fn unzip(name: &str, bytes: &[u8], out: &mut Files) -> Result<(), String> {
    let entries = zipfile::read(bytes).map_err(|e| format!("{name}: {e}"))?;
    for e in entries {
        if e.name.to_lowercase().ends_with(".zip") {
            unzip(&e.name, &e.data, out)?;
        } else {
            out.push((e.name, e.data));
        }
    }
    Ok(())
}

/// What the vault holds, for an import into it: a folder of its own, the
/// pages imported before, its paths.
fn options(vault: &str) -> Options {
    let (vault_paths, folders) = step(|c| {
        let folders = c
            .replicas
            .get(vault)
            .map(|r| r.index.folders())
            .unwrap_or_default();
        let paths: Vec<String> = c
            .live_entries(vault)
            .into_iter()
            .filter(|e| e.kind != EntryKind::Folder)
            .map(|e| e.path)
            .collect();
        (paths, folders)
    });
    let existing = knowledge::notes_with_text(vault)
        .into_iter()
        .filter_map(|(_, path, text)| Some((notekpr_notion::id_of(&text)?, path)))
        .collect();
    let into = notekpr_notion::landing_folder(|name| {
        let name = name.to_lowercase();
        let (note, inside) = (format!("{name}.md"), format!("{name}/"));
        vault_paths.iter().chain(&folders).any(|p| {
            let p = p.to_lowercase();
            p == name || p == note || p.starts_with(&inside)
        })
    });
    Options {
        into,
        existing,
        vault_paths,
        ..Options::default()
    }
}

/// Import what was picked, but what's in `skip`, then open the report.
pub async fn import(vault: &str, picked: &Picked, skip: BTreeSet<String>) {
    let opts = Options {
        skip,
        ..picked.opts.clone()
    };
    let converted = picked.export.convert(&opts);
    let got = actions::import_files(vault, converted.files, false).await;
    let ids: HashMap<String, String> = step(|c| {
        c.live_entries(vault)
            .into_iter()
            .map(|e| (e.path, e.id))
            .collect()
    });
    for (path, icon) in &converted.icons {
        actions::set_entry_icon(vault, ids.get(path).map(String::as_str), path, icon);
    }
    let r = &converted.report;
    let mut what = vec![util::count(got.notes, "note", "notes")];
    if got.files > 0 {
        what.push(util::count(got.files, "file", "files"));
    }
    let already = if r.already > 0 {
        format!(
            " ({} already here from an earlier import)",
            util::count(r.already, "page", "pages")
        )
    } else {
        String::new()
    };
    success(format!(
        "Imported {} from Notion into {}{already}.",
        util::join_and(&what),
        opts.into
    ));
    if let Some(id) = ids.get(&paths::join(&opts.into, notekpr_notion::REPORT)) {
        navigate(Route::Note(vault.to_string(), id.clone()));
    }
}
