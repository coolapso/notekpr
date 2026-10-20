//! `coolpsonotes recover`: decrypt an encrypted export (from `GET /me/export`
//! or `coolpsonotes-server export-user`) into plain Markdown files, offline,
//! with nothing but the person's account key (unlocked from their recovery
//! key). No server, no login, no other app needed. A signed-in device can
//! also recover from its own local copies, without the server.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use coolpsonotes_core::keys::AccountSecret;
use coolpsonotes_core::protocol::EncryptedExport;
use coolpsonotes_core::recover::{self, RecoveredVault};

use crate::fsutil;
use crate::session::Session;

#[derive(Debug, Default)]
pub struct RecoverReport {
    pub vaults: usize,
    pub files: usize,
    pub problems: Vec<String>,
}

pub fn read_export(path: &Path) -> Result<EncryptedExport> {
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let export: EncryptedExport =
        serde_json::from_slice(&data).context("this is not a coolpsonotes encrypted export")?;
    if export.format != coolpsonotes_core::protocol::EXPORT_FORMAT {
        bail!(
            "export format {} is not supported by this version (expected {})",
            export.format,
            coolpsonotes_core::protocol::EXPORT_FORMAT
        );
    }
    Ok(export)
}

/// Download the signed-in account's encrypted export (everything it can
/// read, as the server stores it).
pub async fn download_export(session: &Session) -> Result<EncryptedExport> {
    session
        .api
        .get("/me/export")
        .await
        .context("downloading the encrypted export")
}

/// Decrypt an export with this device's keys instead of the recovery key.
pub fn recover_with_device(
    session: &Session,
    export: &EncryptedExport,
    out: &Path,
) -> Result<RecoverReport> {
    if export.user_id != session.user_id {
        bail!("this export belongs to another account");
    }
    recover_to(export, &session.account, out)
}

/// Decrypt `export` with `account` and write every vault into its own folder
/// under `out` (which must not exist or be empty).
pub fn recover_to(
    export: &EncryptedExport,
    account: &AccountSecret,
    out: &Path,
) -> Result<RecoverReport> {
    check_empty(out)?;
    write_recovered(&recover::decrypt_export(account, export), out)
}

/// Recover from this device's local copies, without the server: every vault
/// it keeps a copy of, as far as it synced (and with its unsent edits), and
/// the attachments it downloaded. Notes shared with the account one by one
/// aren't kept on devices: only an export has them.
pub fn recover_local(session: &Session, out: &Path) -> Result<RecoverReport> {
    check_empty(out)?;
    let mut vaults = Vec::new();
    let mut problems = Vec::new();
    for id in session.local_vault_ids() {
        match session.load_local(&id) {
            Ok(Some(v)) => vaults.push(recover::vault_files(&v.replica, v.name(), |a| {
                match v.store().load_blob(a) {
                    Ok(Some(ct)) => Ok(ct),
                    Ok(None) => Err("never downloaded on this device".into()),
                    Err(e) => Err(format!("{e:#}")),
                }
            })),
            Ok(None) => {}
            Err(e) => problems.push(format!("{e:#}")),
        }
    }
    let mut report = write_recovered(&vaults, out)?;
    report.problems.extend(problems);
    Ok(report)
}

fn check_empty(out: &Path) -> Result<()> {
    if out.exists() && std::fs::read_dir(out)?.next().is_some() {
        bail!("{} is not empty; choose a new folder", out.display());
    }
    Ok(())
}

/// Write every vault into its own folder under `out`.
fn write_recovered(vaults: &[RecoveredVault], out: &Path) -> Result<RecoverReport> {
    fsutil::create_private_dir(out)?;
    let mut report = RecoverReport::default();
    let mut used = std::collections::BTreeSet::new();
    for v in vaults {
        let dir_name = unique_dir_name(v, &mut used);
        let base = out.join(&dir_name);
        fsutil::create_private_dir(&base)?;
        report.vaults += 1;
        for f in &v.folders {
            match safe_join(&base, f) {
                Some(p) => fsutil::create_private_dir(&p)?,
                None => report
                    .problems
                    .push(format!("{dir_name}: skipped folder {f:?} (unsafe name)")),
            }
        }
        for f in &v.files {
            let Some(path) = safe_join(&base, &f.path) else {
                report
                    .problems
                    .push(format!("{dir_name}: skipped {:?} (unsafe name)", f.path));
                continue;
            };
            if let Some(parent) = path.parent() {
                fsutil::create_private_dir(parent)?;
            }
            write_private(&path, &f.data)?;
            report.files += 1;
        }
        report
            .problems
            .extend(v.problems.iter().map(|p| format!("{dir_name}: {p}")));
    }
    Ok(report)
}

fn unique_dir_name(v: &RecoveredVault, used: &mut std::collections::BTreeSet<String>) -> String {
    let base: String = v
        .name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.()".contains(c) {
                c
            } else {
                '_'
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
    let mut name = base.clone();
    if used.contains(&name.to_lowercase()) {
        let short: String = v.vault_id.chars().take(6).collect();
        name = format!("{base} ({short})");
        let mut n = 2;
        while used.contains(&name.to_lowercase()) {
            name = format!("{base} ({short}) {n}");
            n += 1;
        }
    }
    used.insert(name.to_lowercase());
    name
}

/// Join a vault-relative path onto `base`, refusing anything that could leave
/// it or that some filesystems treat specially (drive prefixes, reserved
/// device names, characters Windows forbids, trailing dots or spaces).
pub fn safe_join(base: &Path, rel: &str) -> Option<PathBuf> {
    const RESERVED: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    let mut out = base.to_path_buf();
    for seg in rel.split('/') {
        if seg.is_empty()
            || seg == "."
            || seg == ".."
            || seg.ends_with('.')
            || seg.ends_with(' ')
            || seg
                .chars()
                .any(|c| c.is_control() || "<>:\"\\|?*".contains(c))
            || RESERVED.contains(&seg.split('.').next().unwrap_or("").to_lowercase().as_str())
        {
            return None;
        }
        let mut comps = Path::new(seg).components();
        match (comps.next(), comps.next()) {
            (Some(Component::Normal(_)), None) => out.push(seg),
            _ => return None,
        }
    }
    out.starts_with(base).then_some(out)
}

fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    f.write_all(data)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_paths_are_refused() {
        let base = Path::new("/tmp/out");
        assert_eq!(
            safe_join(base, "Plans/Trip.md"),
            Some(base.join("Plans/Trip.md"))
        );
        for bad in [
            "../x.md",
            "a/../../x",
            "C:/x.md",
            "a/CON.md",
            "a/nul",
            "x.md.",
            "x ",
            "a:b.md",
            "a\\b",
            "",
            "a//b",
        ] {
            assert_eq!(safe_join(base, bad), None, "{bad}");
        }
    }
}
