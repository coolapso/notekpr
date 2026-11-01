//! `notekpr setup`: this computer set up in one go — signed in, a folder for
//! the notes as Markdown files, notes already there brought into a vault,
//! and syncing in the background. It asks what its options don't say (on a
//! terminal) and does what `login`, `vault create` and `sync --dir` do,
//! which stay as they are for scripts.
//!
//! Notes already in the chosen folder (an Obsidian vault, say) are moved, as
//! they are and with everything else in it (`.obsidian` too), into a folder
//! of their own there — the vault's — or copied into one in another folder.
//! That folder gets the vault's marker, so the first sync adopts it and takes
//! its files in as it takes in any new file in a vault's folder: they stay
//! exactly as they were.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use notekpr_core::paths;
use notekpr_core::protocol::PrincipalKind;

use crate::api::{Api, Credentials};
use crate::cli::{LoginArgs, SetupArgs};
use crate::commands::{self, Ctx};
use crate::hub::VaultSummary;
use crate::mirror::{self, MirrorOptions};
use crate::profile::{self, DEFAULT_PROFILE, Dirs, Profile, ProfileLock};
use crate::{fsutil, manage, ui};

/// The vault notes are brought into, and the first one made, unless named
/// (the web app makes one called so for a new account).
pub const DEFAULT_VAULT: &str = "My notes";
/// The folder suggested for the Markdown files.
const DEFAULT_DIR: &str = "~/Notes";
/// The folder suggested for a copy of notes kept elsewhere.
const DEFAULT_COPY_DIR: &str = "~/notekpr";

pub async fn run(ctx: &Ctx, a: SetupArgs) -> Result<()> {
    let tty = ui::stdin_is_tty();
    if tty {
        ui::status(
            "Setting up notekpr on this computer: signing in, a folder for your notes, syncing.\n\
             (Ctrl-C stops at any point; what is done by then stays done.)\n",
        );
    }
    let profile = choose_profile(ctx, tty)?;
    let ctx = Ctx {
        dirs: ctx.dirs.clone(),
        profile,
        json: ctx.json,
        detached: false,
        attached: None,
    };
    sign_in(&ctx, &a.login, tty).await?;

    // From here on the profile's local copies are ours: a running `sync`
    // owns them otherwise.
    let lock = ProfileLock::acquire_within(&ctx.dirs, &ctx.profile, Duration::from_secs(3))
        .await?
        .ok_or_else(|| {
            anyhow!(
                "profile '{}' is in use by a running `notekpr sync` (or gateway): stop it while setting up{}, \
                 then run setup again",
                ctx.profile,
                if cfg!(target_os = "linux") {
                    format!(" (systemctl --user stop notekpr-sync@{})", ctx.profile)
                } else {
                    String::new()
                }
            )
        })?;
    let folder = choose_folder(&ctx, &a, tty)?;
    let done = set_up(&ctx, &a, tty, folder.as_ref()).await?;
    drop(lock);
    let background = background(&ctx, &a, tty)?;

    crate::out!("");
    crate::out!("notekpr is set up (profile '{}').", ctx.profile);
    if let Some((dir, name)) = &done.brought {
        crate::out!("Your notes: {} (the vault '{name}').", shown(dir));
        if done.obsidian {
            crate::out!(
                "Their Obsidian settings came along: in Obsidian, open {} as a vault (Open folder as vault).",
                shown(dir)
            );
        }
    }
    if let Some(f) = &folder {
        crate::out!(
            "Each vault is a folder in {}; new ones and those shared with you appear there by themselves.",
            shown(&f.root)
        );
    }
    let p = profile_flag(&ctx.profile);
    if !background {
        crate::out!(
            "To keep in sync, run `notekpr {p}sync` (Ctrl-C stops it){}.",
            if folder.is_some() {
                "; it remembers the folder"
            } else {
                ""
            }
        );
    }
    if !p.is_empty() {
        crate::out!(
            "Other notekpr commands need `{}` too, for this account.",
            p.trim_end()
        );
    }
    Ok(())
}

fn profile_flag(profile: &str) -> String {
    if profile == DEFAULT_PROFILE {
        String::new()
    } else {
        format!("--profile {profile} ")
    }
}

// ---------------------------------------------------------------- asking

/// `question [default]: `; the default when the answer is empty.
fn ask(question: &str, default: &str) -> Result<String> {
    let prompt = if default.is_empty() {
        format!("{question}: ")
    } else {
        format!("{question} [{default}]: ")
    };
    let a = ui::prompt_line(&prompt)?;
    Ok(if a.is_empty() { default.to_string() } else { a })
}

fn ask_required(question: &str) -> Result<String> {
    loop {
        let a = ask(question, "")?;
        if !a.is_empty() {
            return Ok(a);
        }
    }
}

/// A yes/no question answered yes unless "n".
fn ask_yes(question: &str) -> Result<bool> {
    let a = ui::prompt_line(&format!("{question} [Y/n] "))?;
    Ok(!matches!(a.to_ascii_lowercase().as_str(), "n" | "no"))
}

/// One of `options` (the first unless another is chosen): its index.
fn choose(question: &str, options: &[String]) -> Result<usize> {
    eprintln!("{question}");
    for (i, o) in options.iter().enumerate() {
        eprintln!("  {}) {o}", i + 1);
    }
    loop {
        let a = ui::prompt_line("Choose [1]: ")?;
        if a.is_empty() {
            return Ok(0);
        }
        if let Ok(k) = a.parse::<usize>()
            && (1..=options.len()).contains(&k)
        {
            return Ok(k - 1);
        }
        ui::warn(&format!("answer a number from 1 to {}", options.len()));
    }
}

fn home() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(std::env::var_os)
        .find(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// A typed folder made absolute, `~` as the home folder (no shell expands
/// an answer).
fn expand(p: &Path) -> Result<PathBuf> {
    let s = p.to_string_lossy();
    let s = s.trim();
    let path = match (s.strip_prefix('~'), home()) {
        (Some(rest), Some(h)) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            h.join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(s),
    };
    std::path::absolute(&path).with_context(|| format!("resolving {}", path.display()))
}

/// `p` for people: below the home folder as `~/…`.
pub fn shown(p: &Path) -> String {
    match home().and_then(|h| p.strip_prefix(&h).ok().map(Path::to_path_buf)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    }
}

// ---------------------------------------------------------------- the account

fn choose_profile(ctx: &Ctx, tty: bool) -> Result<String> {
    if ctx.profile != DEFAULT_PROFILE || !tty {
        return Ok(ctx.profile.clone());
    }
    loop {
        let name = ask(
            "Name for this account on this computer (a profile; another name matters only with several accounts)",
            DEFAULT_PROFILE,
        )?;
        if profile::valid_profile_name(&name) {
            return Ok(name);
        }
        ui::warn("a profile name is letters, digits, - and _ (at most 64)");
    }
}

/// Signed in already, or now: the server and, on a development server, the
/// user name are asked when not given.
async fn sign_in(ctx: &Ctx, login: &LoginArgs, tty: bool) -> Result<()> {
    let existing = ctx.store()?.load()?;
    if let Some(p) = &existing {
        if p.kind == PrincipalKind::Agent {
            bail!(
                "profile '{}' is an agent's; setup is for people: use another --profile",
                ctx.profile
            );
        }
        if signed_in(ctx, p).await {
            ui::status(&format!(
                "Signed in as {} on {} (profile '{}').",
                p.display_name.as_deref().unwrap_or("?"),
                p.server,
                ctx.profile
            ));
            return Ok(());
        }
    }
    let mut login = login.clone();
    if login.server.is_none() {
        login.server = match existing.map(|p| p.server) {
            Some(s) => Some(s),
            None if tty => Some(ask_required(
                "Address of your notekpr server, e.g. https://notes.example.com",
            )?),
            None => bail!("--server URL is needed to sign in"),
        };
    }
    // A development server signs people in by name.
    if login.dev_user.is_none()
        && tty
        && let Some(server) = &login.server
    {
        let server = profile::normalize_server_url(server, login.insecure_http)?;
        if let Ok(info) = Api::new(&server, Credentials::None)?.info().await
            && info.dev_mode
        {
            let suggested = if ctx.profile == DEFAULT_PROFILE {
                ""
            } else {
                ctx.profile.as_str()
            };
            let name = if info.oidc_enabled {
                ask(
                    "This is a development server: user name to sign in as (empty: your identity provider)",
                    suggested,
                )?
            } else {
                match suggested {
                    "" => ask_required("This is a development server: user name to sign in as")?,
                    s => ask("This is a development server: user name to sign in as", s)?,
                }
            };
            if !name.is_empty() {
                login.dev_user = Some(name);
            }
        }
    }
    let out = commands::sign_in(ctx, login).await?;
    if out.recovery_key.is_some() && tty {
        ui::prompt_line("Press Enter once your recovery key is stored safely. ")?;
    }
    Ok(())
}

/// Whether the profile can use the server as it is. Offline, as far as the
/// profile says.
async fn signed_in(ctx: &Ctx, p: &Profile) -> bool {
    if p.session_token.is_none() || p.device_id.is_none() || p.device_pending {
        return false;
    }
    let Ok(s) = ctx.session() else {
        return false;
    };
    match s.api.me().await {
        Ok(_) => true,
        Err(e) => e.is_network(),
    }
}

// ---------------------------------------------------------------- the folder

/// Where the Markdown files go, and the notes brought in.
#[derive(Debug)]
struct Folder {
    root: PathBuf,
    bring: Bring,
}

#[derive(Debug)]
enum Bring {
    Nothing,
    /// The notes already in the folder: moved into the vault's folder there.
    Move(Contents),
    /// The notes in this folder: copied into the vault's folder.
    Copy(PathBuf, Contents),
}

/// What a folder holds, besides vault folders and hidden files.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Contents {
    /// Folders of vaults (with their marker): it is a Markdown folder of
    /// notekpr's already.
    pub vault_folders: usize,
    /// Markdown files, other files and folders, outside vault folders (not
    /// counting what the sync leaves out).
    pub notes: usize,
    pub files: usize,
    pub folders: usize,
    /// Hidden entries at its top (`.obsidian`, `.git`, …).
    pub hidden: Vec<String>,
}

pub fn inspect(dir: &Path) -> Result<Contents> {
    let mut c = Contents::default();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(c),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    fn count(c: &mut Contents, name: &str) {
        if paths::is_note(name) {
            c.notes += 1;
        } else {
            c.files += 1;
        }
    }
    for e in rd {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if mirror::is_ignored_name(&name) {
            if name.starts_with('.') {
                c.hidden.push(name);
            }
            continue;
        }
        let ft = e.file_type()?;
        if ft.is_file() {
            count(&mut c, &name);
        } else if ft.is_dir() {
            if mirror::vault_marker(&e.path()).is_some() {
                c.vault_folders += 1;
                continue;
            }
            c.folders += 1;
            let inside = walkdir::WalkDir::new(e.path())
                .min_depth(1)
                .into_iter()
                .filter_entry(|w| {
                    w.file_name()
                        .to_str()
                        .is_none_or(|n| !mirror::is_ignored_name(n))
                })
                .filter_map(|w| w.ok());
            for w in inside {
                if w.file_type().is_dir() {
                    c.folders += 1;
                } else if w.file_type().is_file() {
                    count(&mut c, &w.file_name().to_string_lossy());
                }
            }
        }
    }
    c.hidden.sort();
    Ok(c)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "2 notes, 1 folder and .obsidian": what is there, nothing of what isn't.
fn describe(c: &Contents) -> String {
    let mut parts = vec![plural(c.notes, "note", "notes")];
    if c.files > 0 {
        parts.push(plural(c.files, "other file", "other files"));
    }
    if c.folders > 0 {
        parts.push(plural(c.folders, "folder", "folders"));
    }
    parts.extend(c.hidden.iter().cloned());
    match parts.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => parts.concat(),
    }
}

/// Refuse folders the notes can't go to: the top of the file system, the
/// home folder, and a folder holding notekpr's own settings or data, or in
/// them.
pub fn check_folder(dir: &Path, dirs: &Dirs) -> Result<()> {
    let dir = mirror::resolve_lenient(dir);
    if dir.parent().is_none() {
        bail!(
            "{} is the top of the file system: choose a folder for your notes",
            dir.display()
        );
    }
    if home().map(|h| mirror::resolve_lenient(&h)).as_deref() == Some(dir.as_path()) {
        bail!(
            "{} is your home folder: choose a folder in it for your notes, like {DEFAULT_DIR}",
            dir.display()
        );
    }
    for own in [&dirs.config, &dirs.data] {
        let own = mirror::resolve_lenient(own);
        if own.starts_with(&dir) || dir.starts_with(&own) {
            bail!(
                "{} holds notekpr's own settings or data ({}): choose another folder",
                shown(&dir),
                shown(&own)
            );
        }
    }
    Ok(())
}

/// The cloud storage that syncs `dir` (its files there or in a folder above
/// it), if any: it would upload the decrypted notes, and two tools syncing
/// the same files fight over them.
pub fn cloud_synced(dir: &Path) -> Option<&'static str> {
    let dir = mirror::resolve_lenient(dir);
    let text = dir.to_string_lossy();
    if text.contains("/Library/Mobile Documents/") || text.contains("/Library/CloudStorage/") {
        return Some("iCloud or another cloud drive");
    }
    if let Some(one) = std::env::var_os("OneDrive").filter(|v| !v.is_empty())
        && dir.starts_with(one)
    {
        return Some("OneDrive");
    }
    let home = home().map(|h| mirror::resolve_lenient(&h));
    for d in dir.ancestors() {
        if Some(d) == home.as_deref() || d.parent().is_none() {
            break;
        }
        let Ok(rd) = std::fs::read_dir(d) else {
            continue;
        };
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_ascii_lowercase();
            if n == ".nextcloudsync.log"
                || n == ".owncloudsync.log"
                || ((n.starts_with(".sync_") || n.starts_with("._sync_")) && n.ends_with(".db"))
            {
                return Some("Nextcloud or ownCloud");
            }
            if n == ".dropbox" || n == ".dropbox.cache" {
                return Some("Dropbox");
            }
            if n == ".stfolder" {
                return Some("Syncthing");
            }
        }
    }
    None
}

/// The folder for the Markdown files (`None`: only encrypted copies), and
/// what to do with notes already in it.
fn choose_folder(ctx: &Ctx, a: &SetupArgs, tty: bool) -> Result<Option<Folder>> {
    if a.no_dir {
        return Ok(None);
    }
    let remembered = ctx.store()?.require()?.sync.mirror_dir;
    let mut dir = match &a.dir {
        Some(d) => expand(d)?,
        None if tty => match ask_dir(remembered.as_deref())? {
            Some(d) => d,
            None => return Ok(None),
        },
        // Non-interactive and not said: as it is.
        None => match remembered.clone() {
            Some(d) => d,
            None => return Ok(None),
        },
    };
    loop {
        match folder_plan(ctx, a, tty, &dir, remembered.as_deref()) {
            Ok(Some(f)) => return Ok(Some(f)),
            Ok(None) => {}
            Err(e) if tty && a.dir.is_none() => ui::warn(&format!("{e:#}")),
            Err(e) => return Err(e),
        }
        dir = match ask_dir(None)? {
            Some(d) => d,
            None => return Ok(None),
        };
    }
}

fn ask_dir(default: Option<&Path>) -> Result<Option<PathBuf>> {
    let default = default
        .map(shown)
        .unwrap_or_else(|| DEFAULT_DIR.to_string());
    let a = ask(
        "Folder for your notes as Markdown files, each vault a folder in it (\"none\": only encrypted copies)",
        &default,
    )?;
    if a.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    expand(Path::new(&a)).map(Some)
}

/// What to do in `dir`. `None`: another folder is to be chosen.
fn folder_plan(
    ctx: &Ctx,
    a: &SetupArgs,
    tty: bool,
    dir: &Path,
    remembered: Option<&Path>,
) -> Result<Option<Folder>> {
    check_folder(dir, &ctx.dirs)?;
    let folder = |bring: Bring| -> Result<Option<Folder>> {
        Ok(Some(Folder {
            root: dir.to_path_buf(),
            bring,
        }))
    };
    if let Some(src) = &a.copy_from {
        let src = expand(src)?;
        let c = check_copy(&src, dir)?;
        return folder(Bring::Copy(src, c));
    }
    if remembered == Some(dir) {
        if a.move_in {
            bail!(
                "{} is your Markdown folder already: nothing to move in",
                shown(dir)
            );
        }
        return folder(Bring::Nothing);
    }
    let c = inspect(dir)?;
    if c.vault_folders > 0 || c.notes == 0 {
        if a.move_in {
            bail!("{} holds no notes of its own to move in", shown(dir));
        }
        if c.vault_folders == 0 && c.files + c.folders > 0 {
            ui::status(&format!(
                "{} holds other files (no notes): they stay as they are, not synced",
                shown(dir)
            ));
        }
        return folder(Bring::Nothing);
    }
    // Notes of its own: an Obsidian vault, say.
    if a.move_in {
        return folder(Bring::Move(c));
    }
    if !tty {
        bail!(
            "{} already holds notes ({}): say what to do with them — --move-in moves them into a vault there, \
             --copy-from {} with another --dir copies them",
            shown(dir),
            describe(&c),
            shown(dir)
        );
    }
    eprintln!("\n{} already holds notes: {}.", shown(dir), describe(&c));
    let choice = choose(
        "What should happen to them?",
        &[
            format!(
                "Move them, with everything else in {} (hidden files too), into a vault: they become {}/<vault>/ \
                 and sync from now on",
                shown(dir),
                shown(dir)
            ),
            format!(
                "Copy them into a vault in another folder; {} stays as it is",
                shown(dir)
            ),
            "Leave them alone: choose another folder".to_string(),
        ],
    )?;
    match choice {
        0 => folder(Bring::Move(c)),
        1 => loop {
            let to = ask(
                "Folder to sync the copy in (each vault a folder in it)",
                DEFAULT_COPY_DIR,
            )?;
            let to = expand(Path::new(&to))?;
            match check_folder(&to, &ctx.dirs).and_then(|()| check_copy(dir, &to)) {
                Ok(c) => {
                    return Ok(Some(Folder {
                        root: to,
                        bring: Bring::Copy(dir.to_path_buf(), c),
                    }));
                }
                Err(e) => ui::warn(&format!("{e:#}")),
            }
        },
        _ => Ok(None),
    }
}

/// Notes in `src` can be copied into a vault's folder in `root`: what is in
/// `src`.
fn check_copy(src: &Path, root: &Path) -> Result<Contents> {
    if !src.is_dir() {
        bail!("{} is not a folder", shown(src));
    }
    let (s, r) = (mirror::resolve_lenient(src), mirror::resolve_lenient(root));
    if r.starts_with(&s) {
        bail!(
            "{} is inside {}: the copy goes into another folder",
            shown(root),
            shown(src)
        );
    }
    let c = inspect(src)?;
    if c.notes == 0 {
        bail!("{} holds no notes (Markdown files)", shown(src));
    }
    let there = inspect(root)?;
    if there.vault_folders == 0 && there.notes > 0 {
        bail!(
            "{} holds notes of its own: choose an empty folder for the copy, or one notekpr uses already",
            shown(root)
        );
    }
    Ok(c)
}

// ---------------------------------------------------------------- the vault

/// The vault the notes go into.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Existing { id: String, name: String },
    New(String),
}

impl Target {
    fn name(&self) -> &str {
        match self {
            Target::Existing { name, .. } | Target::New(name) => name,
        }
    }
}

/// The vault called `name`: one of `vaults` (it must be writable), or a new
/// one. Its folder in `root` must not be there yet.
fn target_named(name: &str, vaults: &[VaultSummary], root: &Path) -> Result<(Target, PathBuf)> {
    let name = name.trim();
    if name.is_empty() {
        bail!("a vault needs a name");
    }
    if notekpr_core::quick::is_reserved_name(name) {
        bail!(notekpr_core::quick::reserved_name_message());
    }
    let same: Vec<_> = vaults
        .iter()
        .filter(|v| v.name.trim().eq_ignore_ascii_case(name))
        .collect();
    let target = match same.as_slice() {
        [] => Target::New(name.to_string()),
        [v] => {
            if !v.role.can_write() || v.archived || v.read_only.is_some() {
                bail!(
                    "you can't add notes to the vault '{}': name another",
                    v.name
                );
            }
            Target::Existing {
                id: v.id.clone(),
                name: v.name.clone(),
            }
        }
        _ => bail!("several of your vaults are called '{name}': name another"),
    };
    let dir_name = mirror::sanitize_dir_name(target.name(), "");
    if dir_name.is_empty() {
        bail!("'{name}' can't be a folder's name: name the vault another way");
    }
    let dir = mirror::disk_path(root, &dir_name)?;
    if dir.symlink_metadata().is_ok() {
        bail!(
            "{} is there already: name the vault another way",
            shown(&dir)
        );
    }
    Ok((target, dir))
}

/// Move everything in `root` — hidden files too — into its new folder
/// `dest`, as it is: renamed, never copied or rewritten. If one thing can't
/// be moved, what was moved goes back and nothing has changed.
pub fn move_into(root: &Path, dest: &Path) -> Result<()> {
    if dest.symlink_metadata().is_ok() {
        bail!("{} is there already", dest.display());
    }
    let names = std::fs::read_dir(root)
        .with_context(|| format!("reading {}", root.display()))?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    fsutil::create_private_dir(dest)?;
    let mut moved = Vec::new();
    for n in names {
        if let Err(e) = std::fs::rename(root.join(&n), dest.join(&n)) {
            for m in moved.iter().rev() {
                let _ = std::fs::rename(dest.join(m), root.join(m));
            }
            let _ = std::fs::remove_dir(dest);
            return Err(e).with_context(|| {
                format!(
                    "moving {} into {}; nothing was moved",
                    root.join(&n).display(),
                    dest.display()
                )
            });
        }
        moved.push(n);
    }
    Ok(())
}

/// Copy everything in `src` — hidden files too — into the new folder `dest`.
/// Symbolic links are left out, as the sync leaves them out: returns how
/// many.
pub fn copy_into(src: &Path, dest: &Path) -> Result<usize> {
    if dest.symlink_metadata().is_ok() {
        bail!("{} is there already", dest.display());
    }
    fsutil::create_private_dir(dest)?;
    let mut links = 0;
    for e in walkdir::WalkDir::new(src).min_depth(1) {
        let e = e.with_context(|| format!("copying {}", src.display()))?;
        let to = dest.join(e.path().strip_prefix(src)?);
        let ft = e.file_type();
        if ft.is_dir() {
            fsutil::create_private_dir(&to)?;
        } else if ft.is_file() {
            std::fs::copy(e.path(), &to).with_context(|| {
                format!(
                    "copying {} (the copy so far is in {})",
                    e.path().display(),
                    dest.display()
                )
            })?;
        } else {
            links += 1;
        }
    }
    Ok(links)
}

/// What [`set_up`] did.
#[derive(Default)]
struct Done {
    /// The folder the notes were brought into, and its vault.
    brought: Option<(PathBuf, String)>,
    /// Obsidian's settings came along.
    obsidian: bool,
}

/// The vault for the notes brought in (or a first one), the folder, the
/// first sync.
async fn set_up(ctx: &Ctx, a: &SetupArgs, tty: bool, folder: Option<&Folder>) -> Result<Done> {
    let hub = ctx.hub(&[]).await?;
    let vaults: Vec<VaultSummary> = hub
        .summaries()
        .await
        .into_iter()
        .filter(|s| !s.kind.is_section())
        .collect();
    drop(hub);
    let Some(folder) = folder else {
        first_vault(ctx, a, tty, &vaults).await?;
        return Ok(Done::default());
    };
    let root = &folder.root;
    let contents = match &folder.bring {
        Bring::Nothing => None,
        Bring::Move(c) | Bring::Copy(_, c) => Some(c),
    };
    let target = match contents {
        None => None,
        Some(_) => Some(loop {
            let name = match &a.vault {
                Some(v) => v.clone(),
                None if tty => ask(
                    "Vault for them: one of yours, or a new one is made",
                    DEFAULT_VAULT,
                )?,
                None => DEFAULT_VAULT.to_string(),
            };
            match target_named(&name, &vaults, root) {
                Ok(t) => break t,
                Err(e) if tty && a.vault.is_none() => ui::warn(&format!("{e:#}")),
                Err(e) => return Err(e),
            }
        }),
    };

    // Decrypted files, there: said and agreed to before anything changes.
    if let Some(cloud) = cloud_synced(root) {
        ui::warn(&format!(
            "{} looks synced by {cloud} (its files are there or in a folder above): it would upload your \
             decrypted notes, and two tools syncing the same files fight over them. Stop syncing that folder \
             there (unless those files are only left over), or choose another folder",
            shown(root)
        ));
    }
    let store = ctx.store()?;
    if !store.require()?.sync.plaintext_confirmed(root) && !a.plaintext_ok {
        eprintln!("\n{}\n", commands::plaintext_note(root));
        if !tty {
            bail!(
                "writing decrypted files to {} must be confirmed once: run again with --plaintext-ok",
                shown(root)
            );
        }
        if ui::prompt_line("Type 'yes' to continue: ")? != "yes" {
            bail!("cancelled; the folder is as it was (run `notekpr setup` again to go on)");
        }
    }

    let mut done = Done::default();
    if let (Some((target, dest)), Some(c)) = (&target, contents) {
        let (what, from, first) = match &folder.bring {
            Bring::Copy(src, _) => ("Copy", src, ""),
            _ => (
                "Move",
                root,
                " Close Obsidian and other apps using it first.",
            ),
        };
        let into = match target {
            Target::Existing { name, .. } => format!("your vault '{name}'"),
            Target::New(name) => format!("a new vault '{name}'"),
        };
        if tty
            && !ui::confirm(&format!(
                "{what} everything in {} ({}) into {}, {into}?{first}",
                shown(from),
                describe(c),
                shown(dest)
            ))
        {
            bail!("cancelled; nothing was moved or copied");
        }
        let id = match target {
            Target::Existing { id, .. } => id.clone(),
            Target::New(name) => {
                let session = ctx.session()?;
                let v = manage::create_vault(&session, name).await?;
                ui::status(&format!("created the vault '{name}'"));
                v.id().to_string()
            }
        };
        match &folder.bring {
            Bring::Copy(src, _) => {
                let links = copy_into(src, dest)?;
                if links > 0 {
                    ui::status(&format!(
                        "{} left out (the sync leaves them out too)",
                        plural(links, "symbolic link was", "symbolic links were")
                    ));
                }
            }
            _ => move_into(root, dest)?,
        }
        mirror::mark_vault_folder(dest, &id)
            .with_context(|| format!("marking {} as the vault's folder", dest.display()))?;
        done.brought = Some((dest.clone(), target.name().to_string()));
        done.obsidian = c.hidden.iter().any(|h| h == ".obsidian");
    } else {
        first_vault(ctx, a, tty, &vaults).await?;
    }
    store.update(|p| {
        p.sync.confirm_plaintext(root);
        p.sync.mirror_dir = Some(root.clone());
    })?;
    ui::status(&format!("syncing with {}…", shown(root)));
    commands::sync_once(ctx, &[], Some(root), MirrorOptions::default()).await?;
    Ok(done)
}

/// A first vault when there is none: what the web app does for a new account.
async fn first_vault(ctx: &Ctx, a: &SetupArgs, tty: bool, vaults: &[VaultSummary]) -> Result<()> {
    let name = match &a.vault {
        Some(n)
            if vaults
                .iter()
                .any(|v| v.name.trim().eq_ignore_ascii_case(n.trim())) =>
        {
            return Ok(());
        }
        Some(n) => n.clone(),
        None if !vaults.is_empty() => return Ok(()),
        None if tty => ask(
            "You have no vaults yet: name of your first one",
            DEFAULT_VAULT,
        )?,
        None => DEFAULT_VAULT.to_string(),
    };
    let session = ctx.session()?;
    manage::create_vault(&session, &name).await?;
    ui::status(&format!("created the vault '{}'", name.trim()));
    Ok(())
}

// ---------------------------------------------------------------- in the background

/// Syncing in the background, when wanted: whether it runs.
fn background(ctx: &Ctx, a: &SetupArgs, tty: bool) -> Result<bool> {
    let want = a.service
        || (!a.no_service
            && tty
            && service::available()
            && ask_yes(
                "Keep your notes in sync in the background, from now on and after restarts (a systemd user service)?",
            )?);
    if !want {
        return Ok(false);
    }
    if !service::available() {
        bail!(
            "there is no systemd user service manager here to sync in the background: run `notekpr {}sync` \
             instead",
            profile_flag(&ctx.profile)
        );
    }
    let unit = service::enable(&ctx.profile)?;
    ui::status(&format!(
        "{unit} runs now and whenever you log in (`journalctl --user -u {unit} -f` shows what it does, \
         `systemctl --user disable --now {unit}` stops it)"
    ));
    Ok(true)
}

/// The systemd user service `notekpr-sync@<profile>` (its unit is
/// packaging/systemd's, which the packages install).
#[cfg(target_os = "linux")]
mod service {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    use anyhow::{Context, Result, anyhow, bail};

    use crate::ui;

    const UNIT: &str = include_str!("../../../packaging/systemd/notekpr-sync@.service");
    const UNIT_NAME: &str = "notekpr-sync@.service";

    fn systemctl(args: &[&str]) -> Result<Output> {
        Command::new("systemctl")
            .arg("--user")
            .args(args)
            .output()
            .context("running systemctl")
    }

    fn run(args: &[&str]) -> Result<()> {
        let o = systemctl(args)?;
        if !o.status.success() {
            bail!(
                "systemctl --user {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
        Ok(())
    }

    /// Whether this session has a systemd user service manager.
    pub fn available() -> bool {
        systemctl(&["show-environment"]).is_ok_and(|o| o.status.success())
    }

    /// The unit, running `exe` (not one a package installed).
    pub fn unit_for(exe: &Path) -> String {
        let exe = exe.to_string_lossy().replace('%', "%%");
        let exe = if exe.contains(char::is_whitespace) {
            format!("\"{exe}\"")
        } else {
            exe
        };
        UNIT.replace("ExecStart=/usr/bin/notekpr ", &format!("ExecStart={exe} "))
    }

    fn unit_dir() -> Result<PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|v| !v.is_empty())
                    .map(|h| PathBuf::from(h).join(".config"))
            })
            .ok_or_else(|| anyhow!("no home folder to put the service in"))?;
        Ok(config.join("systemd").join("user"))
    }

    /// Enable and start the service of `profile` — its unit written first
    /// when no package installed one. Returns the service's name.
    pub fn enable(profile: &str) -> Result<String> {
        let installed = systemctl(&["cat", UNIT_NAME]).is_ok_and(|o| o.status.success());
        if !installed {
            let exe = std::env::current_exe().context("finding this program")?;
            let path = unit_dir()?.join(UNIT_NAME);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
            }
            std::fs::write(&path, unit_for(&exe))
                .with_context(|| format!("writing {}", path.display()))?;
            ui::status(&format!(
                "wrote {} (it runs {}; move that and run setup again)",
                super::shown(&path),
                exe.display()
            ));
            run(&["daemon-reload"])?;
        }
        let unit = format!("notekpr-sync@{profile}.service");
        run(&["enable", "--now", &unit])?;
        Ok(unit)
    }
}

#[cfg(not(target_os = "linux"))]
mod service {
    pub fn available() -> bool {
        false
    }

    pub fn enable(_profile: &str) -> anyhow::Result<String> {
        anyhow::bail!("syncing in the background is set up on Linux only so far")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn what_a_folder_holds() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        assert_eq!(inspect(&d.join("missing")).unwrap(), Contents::default());
        write(&d.join("Daily/2026-11-01.md"), "# Today\n");
        write(&d.join("Daily/old/2025.md"), "x");
        write(&d.join("img.png"), "png");
        write(&d.join("Prompt.md"), "p");
        write(&d.join(".obsidian/app.json"), "{}");
        write(&d.join("Daily/.trash/gone.md"), "left out");
        write(&d.join("Daily/note.md~"), "an editor's backup");
        write(&d.join("Family/.notekpr-vault"), "abc\n");
        write(&d.join("Family/Plan.md"), "a vault's own");
        assert_eq!(
            inspect(d).unwrap(),
            Contents {
                vault_folders: 1,
                notes: 3,
                files: 1,
                folders: 2,
                hidden: vec![".obsidian".into()],
            }
        );
        assert_eq!(
            describe(&inspect(d).unwrap()),
            "3 notes, 1 other file, 2 folders and .obsidian"
        );
        assert_eq!(
            describe(&Contents {
                notes: 1,
                ..Default::default()
            }),
            "1 note"
        );
    }

    #[test]
    fn moving_takes_everything_as_it_is() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("Notes");
        write(&root.join("Daily/a.md"), "a");
        write(&root.join(".obsidian/app.json"), "{}");
        write(&root.join("b.md"), "b");
        let dest = root.join("My notes");
        move_into(&root, &dest).unwrap();
        let mut top: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        top.sort();
        assert_eq!(top, ["My notes"]);
        assert_eq!(
            std::fs::read_to_string(dest.join("Daily/a.md")).unwrap(),
            "a"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join(".obsidian/app.json")).unwrap(),
            "{}"
        );
        assert_eq!(std::fs::read_to_string(dest.join("b.md")).unwrap(), "b");
        // Never over something there.
        assert!(move_into(&root, &dest).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn copying_leaves_the_folder_and_its_links() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("Notes");
        write(&src.join("Daily/a.md"), "a");
        write(&src.join(".obsidian/app.json"), "{}");
        std::os::unix::fs::symlink(src.join("Daily/a.md"), src.join("link.md")).unwrap();
        let dest = t.path().join("notekpr/My notes");
        assert_eq!(copy_into(&src, &dest).unwrap(), 1);
        assert_eq!(
            std::fs::read_to_string(dest.join("Daily/a.md")).unwrap(),
            "a"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join(".obsidian/app.json")).unwrap(),
            "{}"
        );
        assert!(dest.join("link.md").symlink_metadata().is_err());
        assert_eq!(
            std::fs::read_to_string(src.join("Daily/a.md")).unwrap(),
            "a"
        );
        assert!(src.join("link.md").symlink_metadata().is_ok());
    }

    #[test]
    fn some_folders_are_refused() {
        let t = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(&t.path().join("clients/alice"));
        assert!(check_folder(Path::new("/"), &dirs).is_err());
        if let Some(h) = home() {
            assert!(check_folder(&h, &dirs).is_err());
        }
        // Holding notekpr's own data, or in it.
        assert!(check_folder(&t.path().join("clients"), &dirs).is_err());
        assert!(check_folder(&dirs.data.join("notes"), &dirs).is_err());
        assert!(check_folder(&t.path().join("Notes"), &dirs).is_ok());
    }

    #[test]
    fn cloud_synced_folders_are_recognised() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("cloud/Notes");
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(cloud_synced(&d), None);
        write(&t.path().join("cloud/.sync_53b6c3915bef.db"), "");
        assert_eq!(cloud_synced(&d), Some("Nextcloud or ownCloud"));
        write(&d.join(".stfolder/x"), "");
        assert_eq!(cloud_synced(&d), Some("Syncthing"));
    }

    #[test]
    fn answers_are_expanded() {
        if let Some(h) = home() {
            assert_eq!(expand(Path::new("~/Notes")).unwrap(), h.join("Notes"));
            assert_eq!(expand(Path::new("~")).unwrap(), h);
            assert_eq!(shown(&h.join("Notes")), "~/Notes");
        }
        assert!(expand(Path::new("relative")).unwrap().is_absolute());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_unit_runs_this_program() {
        let u = service::unit_for(Path::new("/home/a b/bin/notekpr"));
        assert!(
            u.contains("ExecStart=\"/home/a b/bin/notekpr\" --profile %i sync\n"),
            "{u}"
        );
        assert!(!u.contains("/usr/bin/notekpr"));
        let u = service::unit_for(Path::new("/opt/50%/notekpr"));
        assert!(u.contains("ExecStart=/opt/50%%/notekpr --profile"), "{u}");
    }
}
