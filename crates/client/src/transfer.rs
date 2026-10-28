//! Portable export and bulk import: a plain tree of Markdown files and
//! attachments (as a directory or a `.zip`), with no proprietary metadata, so
//! a vault stays usable without notekpr. The notes' comments are exported
//! as readable Markdown files under `_comments/`, which imports leave out
//! (comments can't be brought back: they are signed by their authors).
//!
//! Several vaults and the quick notes go together into one export, with a
//! manifest naming them (`notekpr_core::account_export`): what people take
//! to another server ([`export_account`], [`import_account`]).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use notekpr_core::account_export::{self, ExportedVault, MANIFEST, Manifest};
use notekpr_core::comments;
use notekpr_core::doc::EntryKind;
use notekpr_core::{paths, quick};

use crate::fsutil;
use crate::hub::Hub;
use crate::mirror::{disk_path, is_ignored_name, is_safe_vault_path};
use crate::session::SharedVault;

#[derive(Debug, Default, Clone)]
pub struct TransferReport {
    pub notes: usize,
    pub drawings: usize,
    pub attachments: usize,
    pub folders: usize,
    /// Notes whose comments were exported (one file each).
    pub comments: usize,
    pub unchanged: usize,
    /// Paths that were skipped, with the reason.
    pub skipped: Vec<(String, String)>,
}

enum Item {
    Note {
        path: String,
        text: String,
    },
    /// A drawing, as an Excalidraw file.
    Drawing {
        path: String,
        text: String,
    },
    /// A note's comments, as a Markdown file.
    Comments {
        path: String,
        text: String,
    },
    Attachment {
        path: String,
        id: String,
    },
    Folder {
        path: String,
    },
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
    let e = exported(hub, vault, zip, &mut report).await?;
    write_out(out, force, zip, &e.files, &e.folders, None)?;
    Ok(report)
}

/// One vault as exported: its files and empty folders (paths in the vault)
/// and its entries' icons by path.
struct Exported {
    files: Vec<(String, Vec<u8>)>,
    folders: Vec<String>,
    icons: BTreeMap<String, String>,
}

/// Everything of a vault an export takes (`zip`: into a `.zip`, which
/// keeps paths a directory can't).
async fn exported(
    hub: &Hub,
    vault: &SharedVault,
    zip: bool,
    report: &mut TransferReport,
) -> Result<Exported> {
    let mut icons = BTreeMap::new();
    // The names of the people who commented (asked first: no lock held).
    let commenters: Vec<String> = {
        let v = vault.lock().await;
        v.replica
            .comments
            .values()
            .flat_map(|log| log.entries().iter().map(|e| e.record.author.clone()))
            .collect()
    };
    let names = crate::ops::user_names(&hub.session, commenters).await;
    let items: Vec<Item> = {
        let v = vault.lock().await;
        let mut items = Vec::new();
        // Notes (path, id), and the paths files take.
        let mut notes = Vec::new();
        let mut taken = std::collections::BTreeSet::new();
        for (p, e) in v.paths() {
            let ok = if zip {
                paths::normalize(&p).is_ok_and(|n| n == paths::nfc(&p))
            } else {
                is_safe_vault_path(&p)
            };
            if !ok {
                report
                    .skipped
                    .push((p, "hidden or unsupported path".into()));
                continue;
            }
            if e.kind != EntryKind::Folder {
                taken.insert(p.to_lowercase());
            }
            if let Some(i) = &e.icon {
                icons.insert(p.clone(), i.clone());
            }
            items.push(match e.kind {
                EntryKind::Note => {
                    notes.push((p.clone(), e.id.clone()));
                    Item::Note {
                        text: v.note_text(&e.id),
                        path: p,
                    }
                }
                EntryKind::Attachment => Item::Attachment { path: p, id: e.id },
                EntryKind::Drawing => Item::Drawing {
                    text: notekpr_draw::file::write_excalidraw(&v.drawing(&e.id)),
                    path: p,
                },
                EntryKind::Folder => Item::Folder { path: p },
            });
        }
        // Comments after: the vault's own files keep their paths.
        for (path, id) in notes {
            let threads = v
                .replica
                .comment_threads(&id, |_| None, |u| u == v.owner_id);
            let name = |u: &str| names.get(u).cloned().unwrap_or_else(|| u.to_string());
            let Some(text) =
                comments::to_markdown(paths::title(&path), v.replica.note(&id), &threads, name)
            else {
                continue;
            };
            let at = paths::unique(&comments::export_path(&path), |c| {
                taken.contains(&c.to_lowercase())
            });
            taken.insert(at.to_lowercase());
            items.push(Item::Comments { path: at, text });
        }
        items
    };
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut folders: Vec<String> = Vec::new();
    for it in items {
        match it {
            Item::Note { path, text } => {
                files.push((path, text.into_bytes()));
                report.notes += 1;
            }
            Item::Drawing { path, text } => {
                files.push((path, text.into_bytes()));
                report.drawings += 1;
            }
            Item::Comments { path, text } => {
                files.push((path, text.into_bytes()));
                report.comments += 1;
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
    Ok(Exported {
        files,
        folders,
        icons,
    })
}

/// Write an export to `out`, a `.zip` or a directory, with an account
/// export's `manifest` at its top.
fn write_out(
    out: &Path,
    force: bool,
    zip: bool,
    files: &[(String, Vec<u8>)],
    folders: &[String],
    manifest: Option<&[u8]>,
) -> Result<()> {
    // Exports are decrypted: owner-only permissions (0600 files, 0700
    // folders), like the mirror.
    if zip {
        if out.exists() && !force {
            bail!(
                "{} already exists (use --force to overwrite)",
                out.display()
            );
        }
        let tmp = out.with_extension("zip.partial");
        let written = (|| -> Result<()> {
            let f = create_private(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
            let mut w = zip::ZipWriter::new(f);
            let file_opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o600);
            let dir_opts = file_opts.unix_permissions(0o700);
            if let Some(m) = manifest {
                w.start_file(MANIFEST, file_opts)?;
                w.write_all(m)?;
            }
            for d in folders {
                w.add_directory(format!("{d}/"), dir_opts)?;
            }
            for (p, data) in files {
                w.start_file(p.as_str(), file_opts)?;
                w.write_all(data)?;
            }
            w.finish()?.sync_all()?;
            std::fs::rename(&tmp, out).with_context(|| format!("replacing {}", out.display()))?;
            Ok(())
        })();
        if written.is_err() {
            // Never leave a half-written plaintext archive behind.
            let _ = std::fs::remove_file(&tmp);
        }
        written?;
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
            // Leftovers of an export that crashed.
            fsutil::remove_stale_temp_files(out, 64, std::time::Duration::from_secs(60));
        }
        fsutil::create_private_dir(out)?;
        if let Some(m) = manifest {
            fsutil::write_atomic(&out.join(MANIFEST), m, true)?;
        }
        for d in folders {
            fsutil::create_private_dir(&disk_path(out, d)?)?;
        }
        for (p, data) in files {
            // Every segment is checked again: nothing may land outside `out`.
            let target = disk_path(out, p)?;
            if let Some(parent) = target.parent() {
                fsutil::create_private_dir(parent)?;
            }
            fsutil::write_atomic(&target, data, true)
                .with_context(|| format!("writing {}", target.display()))?;
        }
    }
    Ok(())
}

/// What an export of vaults and quick notes held, or what an import of one
/// brought in.
#[derive(Debug, Default, Clone)]
pub struct AccountReport {
    /// The vaults' names (an import's as created: a name taken got a number).
    pub vaults: Vec<String>,
    /// Quick notes exported, or imported.
    pub quick_notes: usize,
    pub totals: TransferReport,
}

/// Export the vaults open in `hub`, but those `skip` names (or ids), and with
/// `quick_notes` the quick notes, as one export [`import_account`] reads
/// back — here or on another server.
pub async fn export_account(
    hub: &Hub,
    out: &Path,
    force: bool,
    skip: &[String],
    quick_notes: bool,
) -> Result<AccountReport> {
    let zip = is_zip(out);
    let mut report = AccountReport::default();
    let mut chosen = Vec::new();
    let mut quick = None;
    let mut skipped = BTreeSet::new();
    for v in hub.vaults() {
        let (is_quick, id, meta) = {
            let g = v.lock().await;
            (
                g.is_quick_notes(),
                g.id().to_string(),
                g.replica.meta.clone(),
            )
        };
        if is_quick {
            quick = Some(v);
            continue;
        }
        if let Some(s) = skip
            .iter()
            .find(|s| **s == id || s.eq_ignore_ascii_case(&meta.name))
        {
            skipped.insert(s.clone());
            continue;
        }
        chosen.push((meta, v));
    }
    if let Some(s) = skip.iter().find(|s| !skipped.contains(*s)) {
        bail!("no vault '{s}' to leave out");
    }
    chosen.sort_by_key(|(m, _)| m.name.to_lowercase());
    let names: Vec<&str> = chosen.iter().map(|(m, _)| m.name.as_str()).collect();
    let folders = account_export::folder_names(&names);
    let mut manifest = Manifest {
        format: account_export::FORMAT,
        created_at: notekpr_core::now_ms(),
        vaults: Vec::new(),
        quick_notes: None,
    };
    let (mut files, mut dirs) = (Vec::new(), Vec::new());
    let mut add = |folder: &str, e: Exported| {
        files.extend(
            e.files
                .into_iter()
                .map(|(p, d)| (format!("{folder}/{p}"), d)),
        );
        dirs.extend(e.folders.into_iter().map(|d| format!("{folder}/{d}")));
    };
    for ((meta, v), folder) in chosen.into_iter().zip(folders) {
        let e = exported(hub, &v, zip, &mut report.totals).await?;
        let icons = e.icons.clone();
        add(&folder, e);
        report.vaults.push(meta.name.clone());
        manifest.vaults.push(ExportedVault {
            folder,
            meta,
            icons,
        });
    }
    if quick_notes && let Some(q) = quick {
        let before = report.totals.notes;
        let e = exported(hub, &q, zip, &mut report.totals).await?;
        add(quick::NAME, e);
        report.quick_notes = report.totals.notes - before;
        manifest.quick_notes = Some(quick::NAME.to_string());
    }
    let m = serde_json::to_vec_pretty(&manifest)?;
    write_out(out, force, zip, &files, &dirs, Some(&m))?;
    Ok(report)
}

/// Create (or truncate) a file readable by its owner only.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

/// Files to import as (vault-relative path, bytes), plus empty folders.
type Collected = (Vec<(String, Vec<u8>)>, Vec<String>);

/// Whether a path to import is in the comments folder of an export: those
/// files aren't notes, and comments can't be brought back.
fn is_exported_comments(rel: &str) -> bool {
    rel.split('/').next() == Some(comments::EXPORT_DIR)
}

/// Leave out an export's comments, saying so once.
fn drop_comments(c: &mut Collected, report: &mut TransferReport) {
    let before = c.0.len() + c.1.len();
    c.0.retain(|(p, _)| !is_exported_comments(p));
    c.1.retain(|p| !is_exported_comments(p));
    if c.0.len() + c.1.len() < before {
        report.skipped.push((
            format!("{}/", comments::EXPORT_DIR),
            "an export's comments are not imported".into(),
        ));
    }
}

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
/// `skip_existing` — "existing" uses the same (case-insensitive) matching as
/// writing, so a case variant is never merged into. With `dry_run`, nothing
/// changes: the report says what an import would do.
pub async fn import(
    hub: &Hub,
    vault: &SharedVault,
    src: &Path,
    into: Option<&str>,
    skip_existing: bool,
    dry_run: bool,
) -> Result<TransferReport> {
    let mut report = TransferReport::default();
    let mut collected = collect(src, &mut report)?;
    drop_comments(&mut collected, &mut report);
    import_collected(
        hub,
        vault,
        collected,
        into,
        skip_existing,
        dry_run,
        &mut report,
    )
    .await?;
    Ok(report)
}

fn collect(src: &Path, report: &mut TransferReport) -> Result<Collected> {
    if is_zip(src) {
        collect_zip(src, report)
    } else if src.is_dir() {
        collect_dir(src, report)
    } else {
        bail!("{} is neither a directory nor a .zip file", src.display());
    }
}

/// [`import`] of files already collected.
async fn import_collected(
    hub: &Hub,
    vault: &SharedVault,
    collected: Collected,
    into: Option<&str>,
    skip_existing: bool,
    dry_run: bool,
    report: &mut TransferReport,
) -> Result<()> {
    let (mut files, dirs) = collected;
    // Notes first: a file in the quick notes needs a note linking to it.
    files.sort_by_key(|(rel, _)| !paths::is_note(rel));
    let prefix = match into {
        Some(f) if !f.trim_matches('/').is_empty() => paths::normalize(f)?,
        _ => String::new(),
    };
    let max = hub.limits().await.max_attachment_bytes;
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
            if dry_run {
                match v.paths().get(&p) {
                    Some(e) if e.kind != EntryKind::Folder => report
                        .skipped
                        .push((p, "exists and is not a folder".into())),
                    _ => report.folders += 1,
                }
                continue;
            }
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
            let existing = v.find(&norm);
            if paths::is_note(&norm) {
                let Ok(text) = String::from_utf8(data) else {
                    report.skipped.push((norm, "not valid UTF-8".into()));
                    continue;
                };
                if existing.is_some() && skip_existing {
                    report.unchanged += 1;
                    continue;
                }
                if dry_run {
                    match existing {
                        Some((_, e)) if e.kind != EntryKind::Note => report
                            .skipped
                            .push((norm, "exists and is not a note".into())),
                        Some((_, e)) if v.note_text(&e.id) == text => report.unchanged += 1,
                        _ => report.notes += 1,
                    }
                    continue;
                }
                match v.write_note(&norm, &text) {
                    Ok(r) if r.changed => report.notes += 1,
                    Ok(_) => report.unchanged += 1,
                    Err(e) => report.skipped.push((norm, format!("{e:#}"))),
                }
            } else if let Some(scene) = drawing_in(&norm, &data, &v) {
                if existing.is_some() && skip_existing {
                    report.unchanged += 1;
                    continue;
                }
                if dry_run {
                    match existing {
                        Some((_, e)) if e.kind != EntryKind::Drawing => report
                            .skipped
                            .push((norm, "exists and is not a drawing".into())),
                        _ => report.drawings += 1,
                    }
                    continue;
                }
                match v.write_drawing(&norm, &scene) {
                    Ok(r) if r.changed => report.drawings += 1,
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
                if existing.is_some() && skip_existing {
                    report.unchanged += 1;
                    continue;
                }
                if dry_run {
                    let same = existing
                        .filter(|(dp, _)| *dp == norm)
                        .and_then(|(_, e)| v.cached_attachment(&e.id).ok().flatten())
                        .is_some_and(|d| d == data);
                    if same {
                        report.unchanged += 1;
                    } else {
                        report.attachments += 1;
                    }
                    continue;
                }
                match v.add_attachment(&norm, &data, None) {
                    Ok(r) if r.changed => report.attachments += 1,
                    Ok(_) => report.unchanged += 1,
                    Err(e) => report.skipped.push((norm, format!("{e:#}"))),
                }
            }
        }
        if !dry_run {
            v.save()?;
        }
    }
    if !dry_run {
        hub.local_changed(&vault_id, vec![], true).await;
    }
    Ok(())
}

/// An `.excalidraw` file that is a drawing becomes one (not in the quick
/// notes, which hold notes); anything else is a file.
fn drawing_in(
    path: &str,
    data: &[u8],
    v: &crate::vault::Vault,
) -> Option<notekpr_draw::scene::Scene> {
    if !paths::is_drawing(path) || v.is_quick_notes() {
        return None;
    }
    let text = std::str::from_utf8(data).ok()?;
    let mut ids = notekpr_draw::restore::Ids::new(notekpr_core::now_ms() as u64);
    notekpr_draw::file::read_drawing(text, &mut ids)
        .ok()
        .map(|d| d.scene)
}

/// The manifest of an export of vaults and quick notes at `src` (a `.zip` or
/// a directory; also one folder down, when it was packed again around one):
/// its path in the export and its bytes. `None`: `src` isn't one.
fn read_manifest(src: &Path) -> Result<Option<(String, Vec<u8>)>> {
    if is_zip(src) {
        let f = std::fs::File::open(src).with_context(|| format!("opening {}", src.display()))?;
        let mut z = zip::ZipArchive::new(f).context("reading the zip archive")?;
        let name = z
            .file_names()
            .filter_map(|n| n.ok().map(|n| n.into_owned()))
            .filter(|n| {
                n == MANIFEST
                    || n.strip_suffix(MANIFEST)
                        .and_then(|d| d.strip_suffix('/'))
                        .is_some_and(|d| !d.is_empty() && !d.contains('/'))
            })
            .min_by_key(|n| n.len());
        let Some(name) = name else { return Ok(None) };
        let mut data = Vec::new();
        z.by_name(&name)?.read_to_end(&mut data)?;
        return Ok(Some((name, data)));
    }
    if !src.is_dir() {
        return Ok(None);
    }
    if let Ok(data) = std::fs::read(src.join(MANIFEST)) {
        return Ok(Some((MANIFEST.to_string(), data)));
    }
    for e in std::fs::read_dir(src)? {
        let e = e?;
        if e.file_type()?.is_dir()
            && let (Some(name), Ok(data)) = (
                e.file_name().to_str().map(str::to_string),
                std::fs::read(e.path().join(MANIFEST)),
            )
        {
            return Ok(Some((format!("{name}/{MANIFEST}"), data)));
        }
    }
    Ok(None)
}

/// Import an export made by [`export_account`] (on this server or another):
/// its vaults are created again — a name already taken gets a number — and
/// its quick notes join this account's, as many as there's room for.
/// `Ok(None)`: `src` isn't such an export.
pub async fn import_account(hub: &Hub, src: &Path, dry_run: bool) -> Result<Option<AccountReport>> {
    let Some(manifest) = read_manifest(src)? else {
        return Ok(None);
    };
    let mut report = AccountReport::default();
    let (mut files, dirs) = collect(src, &mut report.totals)?;
    files.push(manifest);
    // Empty folders travel as `folder/.keep` to their vault.
    files.extend(dirs.into_iter().map(|d| (format!("{d}/.keep"), Vec::new())));
    let unpacked = account_export::unpack(files).map_err(|e| anyhow!(e))?;
    if unpacked.other_files > 0 {
        report.totals.skipped.push((
            src.display().to_string(),
            format!(
                "{} file(s) outside its vaults and quick notes",
                unpacked.other_files
            ),
        ));
    }
    let split = |files: Vec<(String, Vec<u8>)>| -> Collected {
        let (kept, files): (Vec<_>, Vec<_>) = files
            .into_iter()
            .partition(|(p, _)| account_export::kept_folder(p).is_some());
        let dirs = kept
            .into_iter()
            .filter_map(|(p, _)| account_export::kept_folder(&p).map(str::to_string))
            .collect();
        (files, dirs)
    };
    let mut taken = BTreeSet::new();
    for v in hub.vaults() {
        let g = v.lock().await;
        if !g.is_quick_notes() {
            taken.insert(g.name().to_lowercase());
        }
    }
    for (exported, files) in unpacked.vaults {
        let name = account_export::unique_name(exported.meta.name.trim(), |n| {
            taken.contains(&n.to_lowercase()) || quick::is_reserved_name(n)
        });
        taken.insert(name.to_lowercase());
        let mut c = split(files);
        drop_comments(&mut c, &mut report.totals);
        report.vaults.push(name.clone());
        if dry_run {
            report.totals.folders += c.1.len();
            for (p, _) in &c.0 {
                if paths::is_note(p) {
                    report.totals.notes += 1;
                } else {
                    report.totals.attachments += 1;
                }
            }
            continue;
        }
        let id = crate::manage::create_vault(&hub.session, &name)
            .await?
            .id()
            .to_string();
        hub.open_listed(&hub.session.api.vaults().await?).await?;
        let v = hub
            .get(&id)
            .ok_or_else(|| anyhow!("the new vault '{name}' didn't open"))?;
        let meta = &exported.meta;
        if meta.description.is_some() || meta.icon.is_some() || meta.picture.is_some() {
            crate::manage::set_vault_looks(&hub.session, &v, meta).await?;
        }
        import_collected(hub, &v, c, None, false, false, &mut report.totals).await?;
        if !exported.icons.is_empty() {
            {
                let mut g = v.lock().await;
                for (path, icon) in &exported.icons {
                    if let Err(e) = g.set_icon(path, Some(icon)) {
                        report
                            .totals
                            .skipped
                            .push((format!("{name}/{path}"), format!("icon: {e:#}")));
                    }
                }
                g.save()?;
            }
            hub.local_changed(&id, vec![], true).await;
        }
    }
    if let Some(files) = unpacked.quick_notes.filter(|f| !f.is_empty()) {
        let (files, nested): (Vec<_>, Vec<_>) =
            files.into_iter().partition(|(p, _)| quick::is_top_level(p));
        for (p, _) in nested {
            report.totals.skipped.push((
                format!("{}/{p}", quick::NAME),
                "quick notes have no folders".into(),
            ));
        }
        let q = if dry_run {
            hub.quick_notes().await
        } else {
            Some(hub.ensure_quick_notes().await?)
        };
        // As many notes as there's room for (and every file).
        let room = match &q {
            Some(q) => {
                let g = q.lock().await;
                let count = g
                    .paths()
                    .values()
                    .filter(|e| e.kind == EntryKind::Note)
                    .count();
                g.note_limit
                    .max()
                    .map_or(usize::MAX, |m| (m as usize).saturating_sub(count))
            }
            None => usize::MAX,
        };
        let mut left = room;
        let mut fitting = Vec::new();
        for (p, data) in files {
            if paths::is_note(&p) {
                if left == 0 {
                    report.totals.skipped.push((
                        format!("{}/{p}", quick::NAME),
                        "no room left in your quick notes".into(),
                    ));
                    continue;
                }
                left -= 1;
            }
            fitting.push((p, data));
        }
        let before = report.totals.notes;
        match q {
            Some(q) if !dry_run => {
                import_collected(
                    hub,
                    &q,
                    (fitting, Vec::new()),
                    None,
                    false,
                    false,
                    &mut report.totals,
                )
                .await?
            }
            _ => {
                for (p, _) in &fitting {
                    if paths::is_note(p) {
                        report.totals.notes += 1;
                    } else {
                        report.totals.attachments += 1;
                    }
                }
            }
        }
        report.quick_notes = report.totals.notes - before;
    }
    Ok(Some(report))
}
