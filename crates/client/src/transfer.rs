//! Portable export and bulk import: a plain tree of Markdown files and
//! attachments (as a directory or a `.zip`), with no proprietary metadata, so
//! a vault stays usable without coolpsonotes.

use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use coolpsonotes_core::doc::EntryKind;
use coolpsonotes_core::paths;

use crate::fsutil;
use crate::hub::Hub;
use crate::mirror::{disk_path, is_ignored_name, is_safe_vault_path};
use crate::session::SharedVault;

#[derive(Debug, Default, Clone)]
pub struct TransferReport {
    pub notes: usize,
    pub attachments: usize,
    pub folders: usize,
    pub unchanged: usize,
    /// Paths that were skipped, with the reason.
    pub skipped: Vec<(String, String)>,
}

enum Item {
    Note { path: String, text: String },
    Attachment { path: String, id: String },
    Folder { path: String },
}

fn is_zip(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}

/// Export every live note, attachment and explicit folder of a vault.
pub async fn export(
    hub: &Hub,
    vault: &SharedVault,
    out: &Path,
    force: bool,
) -> Result<TransferReport> {
    let mut report = TransferReport::default();
    let zip = is_zip(out);
    let items: Vec<Item> = {
        let v = vault.lock().await;
        v.paths()
            .into_iter()
            .filter_map(|(p, e)| {
                let ok = if zip {
                    paths::normalize(&p).is_ok_and(|n| n == p)
                } else {
                    is_safe_vault_path(&p)
                };
                if !ok {
                    report
                        .skipped
                        .push((p, "hidden or unsupported path".into()));
                    return None;
                }
                Some(match e.kind {
                    EntryKind::Note => Item::Note {
                        text: v.note_text(&e.id),
                        path: p,
                    },
                    EntryKind::Attachment => Item::Attachment { path: p, id: e.id },
                    EntryKind::Folder => Item::Folder { path: p },
                })
            })
            .collect()
    };
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut folders: Vec<String> = Vec::new();
    for it in items {
        match it {
            Item::Note { path, text } => {
                files.push((path, text.into_bytes()));
                report.notes += 1;
            }
            Item::Attachment { path, id } => match hub.session.attachment(vault, &id).await {
                Ok(data) => {
                    files.push((path, data));
                    report.attachments += 1;
                }
                Err(e) => report
                    .skipped
                    .push((path, format!("attachment not available: {e:#}"))),
            },
            Item::Folder { path } => {
                folders.push(path);
                report.folders += 1;
            }
        }
    }
    if zip {
        if out.exists() && !force {
            bail!(
                "{} already exists (use --force to overwrite)",
                out.display()
            );
        }
        let tmp = out.with_extension("zip.partial");
        let f =
            std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        let mut w = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for d in &folders {
            w.add_directory(format!("{d}/"), opts)?;
        }
        for (p, data) in &files {
            w.start_file(p.as_str(), opts)?;
            w.write_all(data)?;
        }
        w.finish()?.sync_all()?;
        std::fs::rename(&tmp, out)?;
    } else {
        if out.exists() {
            let nonempty = std::fs::read_dir(out)
                .map(|mut r| r.next().is_some())
                .unwrap_or(true);
            if nonempty && !force {
                bail!(
                    "{} is not empty (use --force to write into it)",
                    out.display()
                );
            }
        }
        std::fs::create_dir_all(out)?;
        for d in &folders {
            std::fs::create_dir_all(disk_path(out, d)?)?;
        }
        for (p, data) in &files {
            // Every segment is checked again: nothing may land outside `out`.
            let target = disk_path(out, p)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            fsutil::write_atomic(&target, data, false)
                .with_context(|| format!("writing {}", target.display()))?;
        }
    }
    Ok(report)
}

/// Files to import as (vault-relative path, bytes), plus empty folders.
type Collected = (Vec<(String, Vec<u8>)>, Vec<String>);

fn collect_dir(src: &Path, report: &mut TransferReport) -> Result<Collected> {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let mut nonempty = std::collections::BTreeSet::new();
    let walker = walkdir::WalkDir::new(src)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.file_name().to_str().is_some_and(|n| !is_ignored_name(n)));
    for e in walker {
        let e = e?;
        let rel = e.path().strip_prefix(src).unwrap_or(e.path());
        let Some(rel) = rel
            .to_str()
            .map(|s| s.replace(std::path::MAIN_SEPARATOR, "/"))
        else {
            report.skipped.push((
                e.path().display().to_string(),
                "file name is not valid UTF-8".into(),
            ));
            continue;
        };
        if e.file_type().is_dir() {
            dirs.push(rel);
        } else if e.file_type().is_file() {
            let mut p = paths::parent(&rel);
            while !p.is_empty() {
                nonempty.insert(p.to_string());
                p = paths::parent(p);
            }
            files.push((rel, std::fs::read(e.path())?));
        }
    }
    dirs.retain(|d| !nonempty.contains(d));
    Ok((files, dirs))
}

fn collect_zip(src: &Path, report: &mut TransferReport) -> Result<Collected> {
    let f = std::fs::File::open(src).with_context(|| format!("opening {}", src.display()))?;
    let mut z = zip::ZipArchive::new(f).context("reading the zip archive")?;
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for i in 0..z.len() {
        let mut entry = z.by_index(i)?;
        let Some(name) = entry.enclosed_name() else {
            report.skipped.push((
                entry.name().map(|n| n.into_owned()).unwrap_or_default(),
                "unsafe path in archive".into(),
            ));
            continue;
        };
        let Some(rel) = name
            .to_str()
            .map(|s| s.replace('\\', "/").trim_end_matches('/').to_string())
        else {
            continue;
        };
        if rel.is_empty() || rel.split('/').any(is_ignored_name) {
            continue;
        }
        if entry.is_dir() {
            dirs.push(rel);
        } else {
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            files.push((rel, data));
        }
    }
    dirs.retain(|d| !files.iter().any(|(f, _)| paths::is_within(f, d)));
    Ok((files, dirs))
}

/// Import Markdown files and attachments (a directory or a `.zip`),
/// preserving folders. Existing notes are diff-updated (merged), unless
/// `skip_existing`.
pub async fn import(
    hub: &Hub,
    vault: &SharedVault,
    src: &Path,
    into: Option<&str>,
    skip_existing: bool,
) -> Result<TransferReport> {
    let mut report = TransferReport::default();
    let (files, dirs) = if is_zip(src) {
        collect_zip(src, &mut report)?
    } else if src.is_dir() {
        collect_dir(src, &mut report)?
    } else {
        bail!("{} is neither a directory nor a .zip file", src.display());
    };
    let prefix = match into {
        Some(f) if !f.trim_matches('/').is_empty() => paths::normalize(f)?,
        _ => String::new(),
    };
    let max = hub
        .session
        .server_info()
        .await
        .map(|i| i.max_attachment_bytes)
        .unwrap_or(u64::MAX);
    let vault_id;
    {
        let mut v = vault.lock().await;
        v.require_writable()?;
        if hub.read_only {
            bail!("read-only mode");
        }
        vault_id = v.id().to_string();
        for d in dirs {
            let p = paths::join(&prefix, &d);
            match v.create_folder(&p) {
                Ok(_) => report.folders += 1,
                Err(e) => report.skipped.push((p, format!("{e:#}"))),
            }
        }
        for (rel, data) in files {
            let p = paths::join(&prefix, &rel);
            let norm = match paths::normalize(&p) {
                Ok(n) => n,
                Err(e) => {
                    report.skipped.push((p, e.to_string()));
                    continue;
                }
            };
            if paths::is_note(&norm) {
                let Ok(text) = String::from_utf8(data) else {
                    report.skipped.push((norm, "not valid UTF-8".into()));
                    continue;
                };
                let exists = v.find(&norm).is_some_and(|(dp, _)| dp == norm);
                if exists && skip_existing {
                    report.unchanged += 1;
                    continue;
                }
                match v.write_note(&norm, &text) {
                    Ok(r) if r.changed => report.notes += 1,
                    Ok(_) => report.unchanged += 1,
                    Err(e) => report.skipped.push((norm, format!("{e:#}"))),
                }
            } else {
                if data.len() as u64 > max {
                    report
                        .skipped
                        .push((norm, format!("larger than the server limit of {max} bytes")));
                    continue;
                }
                if data.is_empty() {
                    report.skipped.push((norm, "empty file".into()));
                    continue;
                }
                let exists = v.find(&norm).is_some_and(|(dp, _)| dp == norm);
                if exists && skip_existing {
                    report.unchanged += 1;
                    continue;
                }
                match v.add_attachment(&norm, &data, None) {
                    Ok(r) if r.changed => report.attachments += 1,
                    Ok(_) => report.unchanged += 1,
                    Err(e) => report.skipped.push((norm, format!("{e:#}"))),
                }
            }
        }
        v.save()?;
    }
    hub.local_changed(&vault_id, vec![], true).await;
    Ok(report)
}
