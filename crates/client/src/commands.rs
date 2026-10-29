//! Implementations of the CLI commands (thin wrappers over the library).

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use notekpr_core::doc::{Entry, EntryKind};
use notekpr_core::engine::PinStatus;
use notekpr_core::protocol::{DeviceStatus, Limit, MIB, PrincipalKind, Role};
use serde::Serialize;
use serde_json::json;
use tokio::sync::watch;

use crate::account::{self, AgentSetupOptions, LoginOptions};
use crate::api::ApiError;
use crate::audit::AuditLog;
use crate::cli::*;
use crate::control;
use crate::gateway::{self, Gateway};
use crate::hub::{Hub, HubOptions};
use crate::manage;
use crate::mirror::{Mirror, MirrorOptions};
use crate::note_shares;
use crate::ops;
use crate::profile::{Dirs, ProfileLock, ProfileStore};
use crate::session::Session;
use crate::vault::ListKind;
use crate::{fsutil, livesync, recover, transfer, ui};

pub struct Ctx {
    pub dirs: Dirs,
    pub profile: String,
    pub json: bool,
    /// Another process (a sync daemon, the gateway) owns this profile's
    /// local data: read its local copies, never write them.
    pub detached: bool,
    /// Run by that process for another one ([`control`]): use its hub.
    pub attached: Option<Arc<Hub>>,
}

impl Ctx {
    fn store(&self) -> Result<ProfileStore> {
        ProfileStore::new(&self.dirs, &self.profile)
    }

    fn session(&self) -> Result<Arc<Session>> {
        Ok(Arc::new(Session::open(&self.dirs, &self.profile)?))
    }

    /// A hub for one-shot commands: catches up and pushes the given vaults
    /// (or, detached, only reads the local copies).
    async fn hub(&self, vaults: &[&str]) -> Result<Arc<Hub>> {
        self.open_hub(vaults, false).await
    }

    /// A hub with the person's quick notes open (made the first time), and
    /// `vaults` too.
    async fn hub_with_quick_notes(&self, vaults: &[&str]) -> Result<Arc<Hub>> {
        self.open_hub(vaults, true).await
    }

    async fn open_hub(&self, vaults: &[&str], quick_notes: bool) -> Result<Arc<Hub>> {
        if let Some(hub) = &self.attached {
            for v in vaults {
                if hub.resolve(v).await.is_err() {
                    bail!(
                        "the running notekpr process (sync daemon or gateway) doesn't keep vault '{v}' on \
                         this device; stop it, or run it for this vault too"
                    );
                }
            }
            return Ok(hub.clone());
        }
        let opts = HubOptions {
            vaults: vaults.iter().map(|s| s.to_string()).collect(),
            read_only: false,
            offline_ok: true,
            catch_up: !self.detached,
            detached: self.detached,
            quick_notes,
        };
        Hub::start(self.session()?, opts).await
    }

    fn print_json(&self, v: &impl Serialize) {
        crate::out!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
    }
}

/// How a command uses the profile's local data (see [`ProfileLock`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    /// Only the server and the profile file.
    Free,
    /// Reads local copies: if a daemon owns them, read them without syncing.
    Read,
    /// Changes local copies: needs the lock (waits briefly for it).
    Write,
    /// Owns the local copies while running (sync, gateway).
    Daemon,
}

fn access(c: &Command) -> Access {
    match c {
        Command::Login(_)
        | Command::ExportEncrypted { .. }
        | Command::Profiles
        | Command::Devices(_)
        | Command::Recovery(_)
        | Command::Shared(_)
        | Command::Agent(_) => Access::Free,
        Command::Logout { forget, .. } => {
            if *forget {
                Access::Write
            } else {
                Access::Free
            }
        }
        Command::Status
        | Command::Ls { .. }
        | Command::Cat { .. }
        | Command::Search { .. }
        | Command::Backlinks { .. }
        | Command::Export { .. }
        | Command::Recover(_)
        | Command::NoteShares { .. }
        | Command::Trash(TrashCmd::List { .. })
        | Command::Vault(VaultCmd::List) => Access::Read,
        Command::History(h) => match h.cmd {
            Some(HistoryCmd::Restore { .. }) => Access::Write,
            _ => Access::Read,
        },
        Command::Quick(q) if q.list => Access::Read,
        Command::Shapes(ShapesCmd::Ls | ShapesCmd::Export { .. }) => Access::Read,
        Command::Comments(CommentsCmd::List { .. }) | Command::Mentions(_) => Access::Read,
        Command::Sync(a) if a.no_dir => Access::Free,
        Command::Sync(a) if a.once => Access::Write,
        Command::Sync(_) => Access::Daemon,
        Command::Gateway(a) if matches!(a.cmd, Some(GatewayCmd::Token { .. })) => Access::Free,
        Command::Gateway(_) => Access::Daemon,
        _ => Access::Write,
    }
}

/// How long a one-shot command waits for another one to finish.
const LOCK_WAIT: Duration = Duration::from_secs(3);

/// Take the profile lock the command needs. Returns the guard (held until
/// the command ends) and whether to work detached (read-only view).
async fn lock_profile(
    dirs: &Dirs,
    profile: &str,
    access: Access,
) -> Result<(Option<ProfileLock>, bool)> {
    let in_use = || {
        anyhow!(
            "profile '{profile}' is in use by another notekpr process (a running `sync` daemon or \
             `gateway`); two processes would overwrite each other's local copies. Edit the mirrored files or use \
             the gateway instead, or stop that process and try again"
        )
    };
    match access {
        Access::Free => Ok((None, false)),
        Access::Daemon => match ProfileLock::try_acquire(dirs, profile)? {
            Some(l) => Ok((Some(l), false)),
            None => Err(in_use()),
        },
        // Reading never waits: the owner keeps the local copies current.
        Access::Read => match ProfileLock::try_acquire(dirs, profile)? {
            Some(l) => Ok((Some(l), false)),
            None => {
                ui::status(
                    "another notekpr process (sync daemon or gateway) keeps this profile in sync; showing its \
                     local copies (read-only)",
                );
                Ok((None, true))
            }
        },
        // Another one-shot command usually finishes within moments.
        Access::Write => match ProfileLock::acquire_within(dirs, profile, LOCK_WAIT).await? {
            Some(l) => Ok((Some(l), false)),
            None => Err(in_use()),
        },
    }
}

/// Upload what is queued (one vault, or all), then count what still exists
/// only here: queued and oversized updates, attachments not uploaded.
/// `None` if that could not be checked (signed out, no keys).
async fn push_and_count_unsynced(ctx: &Ctx, vault: Option<&str>) -> Option<usize> {
    let refs: Vec<&str> = vault.into_iter().collect();
    let hub = match ctx.hub(&refs).await {
        Ok(h) => h,
        Err(e) => {
            tracing::debug!("cannot check for unsynced changes: {e:#}");
            return None;
        }
    };
    let mut n = 0;
    for v in hub.vaults() {
        let g = v.lock().await;
        n += g.replica.pending.len() + g.outbox.held_count() + g.pending_blobs.len();
    }
    Some(n)
}

fn local_vault_ids(data_dir: &Path) -> std::collections::BTreeSet<String> {
    std::fs::read_dir(data_dir.join("vaults"))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().to_str().map(str::to_string))
                .filter(|n| crate::store::valid_id(n))
                .collect()
        })
        .unwrap_or_default()
}

/// This profile's mirror folders of `ids`, if they are to be deleted too
/// (`--remove-mirror`, or confirmed on a terminal); otherwise says that they
/// stay and returns `None`.
fn mirror_folders_to_remove(
    ctx: &Ctx,
    ids: &std::collections::BTreeSet<String>,
    yes: bool,
    remove_mirror: bool,
) -> Result<Option<Vec<std::path::PathBuf>>> {
    let Some(root) = ctx.store()?.load()?.and_then(|p| p.sync.mirror_dir) else {
        return Ok(None);
    };
    let folders = crate::mirror::mirrored_folders(&root, ids);
    if folders.is_empty() {
        return Ok(None);
    }
    let list = folders
        .iter()
        .map(|f| f.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let remove = remove_mirror
        || (!yes
            && ui::confirm(&format!(
                "Also delete the decrypted Markdown files in {list}?"
            )));
    if !remove {
        ui::status(&format!(
            "the decrypted Markdown files in {list} were left in place; delete them yourself if you no longer \
             need them (or use --remove-mirror)"
        ));
        return Ok(None);
    }
    Ok(Some(folders))
}

fn remove_mirror_folders(folders: &[std::path::PathBuf]) {
    for f in folders {
        match std::fs::remove_dir_all(f) {
            Ok(()) => ui::status(&format!("deleted {}", f.display())),
            Err(e) => ui::warn(&format!("could not delete {}: {e}", f.display())),
        }
        // The mirror directory itself goes too once it is empty.
        if let Some(root) = f.parent() {
            let _ = std::fs::remove_dir(root);
        }
    }
}

fn op<T>(r: ops::OpResult<T>) -> Result<T> {
    r.map_err(|e| anyhow!("{e}"))
}

fn confirm_or_flag(yes: bool, prompt: &str) -> Result<()> {
    if yes || ui::confirm(prompt) {
        return Ok(());
    }
    if !ui::stdin_is_tty() {
        bail!("not confirmed (pass --yes to run non-interactively)");
    }
    bail!("cancelled")
}

pub async fn run(cli: Cli) -> Result<()> {
    run_in(cli, Dirs::from_env()?).await
}

/// [`run`] with explicit configuration and data roots (tests).
pub async fn run_in(cli: Cli, dirs: Dirs) -> Result<()> {
    ui::set_quiet(cli.quiet);
    let access = access(&cli.command);
    // A running daemon owns the profile: it runs changing commands, right
    // away. Otherwise whoever holds it is another one-shot command, which is
    // waited for (it usually finishes within moments).
    let mut free = None;
    if access == Access::Write {
        free = ProfileLock::try_acquire(&dirs, &cli.profile)?;
        if free.is_none() && control::forward(&dirs, cli.clone()).await? {
            return Ok(());
        }
    }
    // Held until the command returns; released by the OS on exit or crash.
    let got = match free {
        Some(l) => Ok((Some(l), false)),
        None => lock_profile(&dirs, &cli.profile, access).await,
    };
    let (_lock, detached) = match got {
        Ok(l) => l,
        // A daemon that started meanwhile.
        Err(e) if access == Access::Write => {
            return match control::forward(&dirs, cli).await? {
                true => Ok(()),
                false => Err(e),
            };
        }
        Err(e) => return Err(e),
    };
    let ctx = Ctx {
        dirs,
        profile: cli.profile.clone(),
        json: cli.json,
        detached,
        attached: None,
    };
    execute(ctx, cli.command).await
}

/// Whether a command changes the profile's local copies (and so needs them
/// to itself, or the process that owns them to run it).
pub fn changes_local_copies(c: &Command) -> bool {
    access(c) == Access::Write
}

/// Run a changing command for another process, on this daemon's `hub`
/// ([`control`]): what it prints is returned, `stdin` is what it reads.
/// (Boxed: the daemons that call it are commands too.)
pub fn run_for_other(
    cli: Cli,
    dirs: Dirs,
    hub: Arc<Hub>,
    stdin: Option<Vec<u8>>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = (Result<()>, ui::Captured)> + Send>> {
    Box::pin(async move {
        let ctx = Ctx {
            dirs,
            profile: cli.profile.clone(),
            json: cli.json,
            detached: false,
            attached: Some(hub),
        };
        ui::capture(stdin, cli.quiet, execute(ctx, cli.command)).await
    })
}

async fn execute(ctx: Ctx, command: Command) -> Result<()> {
    match command {
        Command::Login(a) => login(&ctx, a).await,
        Command::Logout {
            forget,
            yes,
            remove_mirror,
        } => {
            let mut mirror = None;
            if forget {
                // Upload what can be uploaded first; say what would be lost.
                let unsynced = push_and_count_unsynced(&ctx, None).await;
                let mut prompt =
                    "Remove this device from your account and delete all local data of this profile?"
                        .to_string();
                match unsynced {
                    Some(0) => {}
                    Some(n) => {
                        ui::warn(&format!(
                            "{n} local change(s) could not be uploaded (offline, or refused by the server) and \
                             will be LOST"
                        ));
                        prompt =
                            format!("{n} change(s) were never uploaded and will be lost. {prompt}");
                    }
                    None => ui::warn(
                        "could not check for changes that were never uploaded (signed out or offline?); any \
                         would be lost",
                    ),
                }
                confirm_or_flag(yes, &prompt)?;
                let ids = local_vault_ids(&ctx.store()?.data_dir());
                mirror = mirror_folders_to_remove(&ctx, &ids, yes, remove_mirror)?;
            }
            account::logout(&ctx.dirs, &ctx.profile, forget).await?;
            if let Some(folders) = mirror {
                remove_mirror_folders(&folders);
            }
            crate::out!(
                "{}",
                if forget {
                    "Signed out; this device and its local data were removed."
                } else {
                    "Signed out."
                }
            );
            Ok(())
        }
        Command::DeleteAccount { yes, remove_mirror } => {
            let server = ctx.store()?.load()?.map(|p| p.server).unwrap_or_default();
            confirm_or_flag(
                yes,
                &format!(
                    "Delete your account on {server} for good? The vaults you own go — also for the people you \
                     share them with —, you leave the others, and your quick notes, agents and devices go too; \
                     this profile's local data is deleted. Signing in again starts a new, empty account. \
                     (To keep your notes, first: notekpr export --out notes.zip)"
                ),
            )?;
            let ids = local_vault_ids(&ctx.store()?.data_dir());
            let mirror = mirror_folders_to_remove(&ctx, &ids, yes, remove_mirror)?;
            account::delete_account(&ctx.dirs, &ctx.profile).await?;
            if let Some(folders) = mirror {
                remove_mirror_folders(&folders);
            }
            crate::out!("Your account was deleted, and this profile with it.");
            Ok(())
        }
        Command::Status => status(&ctx).await,
        Command::Profiles => {
            for p in ctx.dirs.profiles() {
                let store = ProfileStore::new(&ctx.dirs, &p)?;
                match store.load() {
                    Ok(Some(pr)) => crate::out!(
                        "{p:<16} {:<6} {:<24} {}",
                        pr.kind.as_str(),
                        pr.display_name.unwrap_or_default(),
                        pr.server
                    ),
                    _ => crate::out!("{p:<16} (unreadable)"),
                }
            }
            Ok(())
        }
        Command::Devices(c) => devices(&ctx, c).await,
        Command::Recovery(RecoveryCmd::Rotate { yes }) => {
            let s = ctx.session()?;
            confirm_or_flag(
                yes,
                "Create a new recovery key? The current one will stop working.",
            )?;
            let rk = manage::rotate_recovery(&s).await?;
            show_recovery_key(&rk);
            Ok(())
        }
        Command::Vault(c) => vault(&ctx, c).await,
        Command::Ls {
            vault,
            folder,
            recursive,
        } => ls(&ctx, &vault, folder.as_deref(), recursive).await,
        Command::Cat { vault, path } => cat(&ctx, &vault, &path).await,
        Command::ShareNote {
            vault,
            path,
            user,
            role,
            accept_new_key,
            without_comments,
        } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let role = match role {
                ShareRole::Editor => Role::Editor,
                ShareRole::Viewer => Role::Viewer,
            };
            let out = note_shares::share_note(
                &hub.session,
                &v,
                &path,
                &user,
                role.clone(),
                accept_new_key,
            )
            .await?;
            if !without_comments {
                let n = note_shares::carry_comments(&v, &path).await?;
                if n > 0 {
                    let id = v.lock().await.id().to_string();
                    report_push(hub.local_changed(&id, vec![], false).await);
                    crate::out!("its {n} comment record(s) go with it");
                }
            }
            if out.updated_role {
                crate::out!(
                    "'{path}' is now shared with {} as {}",
                    out.user.display_name,
                    role.as_str()
                );
            } else {
                crate::out!(
                    "shared '{path}' with {} as {} (they cannot see anything else in the vault)",
                    out.user.display_name,
                    role.as_str()
                );
            }
            match (&out.fingerprint, out.pin) {
                (Some(fp), Some(PinStatus::New)) => crate::out!(
                    "Their key fingerprint: {fp} (now pinned; compare it with them over a trusted channel)"
                ),
                (Some(fp), _) => {
                    crate::out!("Their key fingerprint: {fp} (matches the pinned one)")
                }
                _ => {}
            }
            Ok(())
        }
        Command::UnshareNote { vault, path, user } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let u = note_shares::unshare_note(&hub.session, &v, &path, &user).await?;
            crate::out!(
                "stopped sharing '{path}' with {}; the note's key was rotated",
                u.display_name
            );
            Ok(())
        }
        Command::RotateNoteKey { vault, path } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let (vid, nid) = {
                let g = v.lock().await;
                manage::require_owner(&g)?;
                let (_, e) = g.find_note(&path)?;
                if !g.replica.is_shared_object(&e.id) {
                    bail!("'{path}' is not shared individually");
                }
                (g.id().to_string(), e.id.clone())
            };
            note_shares::rotate_note_key(&hub.session, &v, &vid, &nid).await?;
            crate::out!("the key of '{path}' was renewed for its current recipients");
            Ok(())
        }
        Command::NoteShares { vault, path } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let (vid, nid, shared) = {
                let g = v.lock().await;
                let (_, e) = g.find_note(&path)?;
                (
                    g.id().to_string(),
                    e.id.clone(),
                    g.replica.is_shared_object(&e.id),
                )
            };
            if !shared {
                crate::out!("'{path}' is not shared individually");
                return Ok(());
            }
            let list = note_shares::note_shares(&hub.session, &vid, &nid).await?;
            if ctx.json {
                ctx.print_json(&list.iter().map(|s| json!({ "user_id": s.user.id, "name": s.user.display_name, "role": s.role })).collect::<Vec<_>>());
            } else {
                for s in list {
                    crate::out!(
                        "{:<8} {}{}",
                        s.role.as_str(),
                        s.user.display_name,
                        s.user.email.map(|e| format!(" <{e}>")).unwrap_or_default()
                    );
                }
            }
            Ok(())
        }
        Command::Shared(c) => {
            let s = ctx.session()?;
            match c {
                SharedCmd::List => {
                    let list = note_shares::shared_with_me(&s).await?;
                    if ctx.json {
                        ctx.print_json(&list.iter().map(|n| json!({ "id": n.info.object_id, "vault_id": n.info.vault_id, "title": n.title, "icon": n.icon, "role": n.info.role, "owner": n.info.owner.display_name })).collect::<Vec<_>>());
                    } else if list.is_empty() {
                        crate::out!("no notes are shared with you individually");
                    } else {
                        for n in list {
                            crate::out!(
                                "{:<34} {:<6} {:<24} {}",
                                n.info.object_id,
                                n.info.role.as_str(),
                                n.info.owner.display_name,
                                match &n.icon {
                                    Some(i) => format!("{i} {}", n.title),
                                    None => n.title,
                                }
                            );
                        }
                    }
                }
                SharedCmd::Cat { note } => {
                    let (_, text) = note_shares::read_shared(&s, &note).await?;
                    let mut out = std::io::stdout().lock();
                    out.write_all(text.as_bytes())?;
                    out.flush()?;
                }
                SharedCmd::Write { note, append } => {
                    let mut data = String::new();
                    std::io::stdin()
                        .read_to_string(&mut data)
                        .context("reading stdin")?;
                    let n = note_shares::write_shared(&s, &note, &data, append).await?;
                    crate::ui::status(&format!("updated '{}'", n.title));
                }
                SharedCmd::Comments(c) => shared_comments(&ctx, &s, c).await?,
            }
            Ok(())
        }
        Command::Write {
            vault,
            path,
            attachment,
            append,
            if_match,
        } => write(&ctx, &vault, &path, attachment, append, if_match.as_deref()).await,
        Command::Quick(a) => quick(&ctx, a).await,
        Command::Shapes(c) => shapes(&ctx, c).await,
        Command::Comments(c) => comments(&ctx, c).await,
        Command::Mentions(a) => mentions(&ctx, a).await,
        Command::Mkdir { vault, path } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let vid = {
                let mut g = v.lock().await;
                g.create_folder(&path)?;
                g.save()?;
                g.id().to_string()
            };
            report_push(hub.local_changed(&vid, vec![], true).await);
            Ok(())
        }
        Command::Mv {
            vault,
            from,
            to,
            to_vault: Some(target),
            ..
        } => {
            let hub = ctx.hub(&[&vault, &target]).await?;
            let (folder, name) = if to.is_empty() || to.ends_with('/') {
                (to.trim_end_matches('/').to_string(), None)
            } else {
                let to = to.trim_matches('/');
                (
                    notekpr_core::paths::parent(to).to_string(),
                    Some(notekpr_core::paths::title(to).to_string()),
                )
            };
            let m = op(ops::move_to_vault(
                &hub,
                Some(&vault),
                &from,
                &target,
                &folder,
                name.as_deref(),
            )
            .await)?;
            if ctx.json {
                ctx.print_json(&m);
                return Ok(());
            }
            let tname = hub.resolve(&m.vault).await?.lock().await.name().to_string();
            if m.kind == notekpr_core::doc::EntryKind::Folder {
                crate::out!(
                    "moved to '{tname}': {}/ ({} notes, {} files)",
                    m.path,
                    m.notes,
                    m.attachments
                );
            } else {
                crate::out!("moved to '{tname}': {}", m.path);
            }
            if !m.removed {
                ui::warn(&format!(
                    "the copy in '{tname}' isn't on the server yet, so the original stays in '{vault}'"
                ));
            }
            Ok(())
        }
        Command::Mv {
            vault,
            from,
            to,
            update_links,
            to_vault: None,
        } => {
            let hub = ctx.hub(&[&vault]).await?;
            // Recipients of a shared note see its new name (ops does it).
            let out = op(ops::move_document(&hub, &vault, &from, &to, update_links).await)?;
            crate::out!("moved to {}", out.to);
            Ok(())
        }
        Command::Icon {
            vault,
            path,
            icon,
            remove,
        } => {
            let hub = ctx.hub(&[&vault]).await?;
            let icon = if remove { None } else { icon };
            let (_, id) = op(ops::set_icon(&hub, &vault, &path, icon.as_deref()).await)?;
            let v = hub.resolve(&vault).await?;
            note_shares::refresh_meta(&hub.session, &v, &id).await?;
            match icon {
                Some(i) => crate::out!("{i} {path}"),
                None => crate::out!("removed the icon of {path}"),
            }
            Ok(())
        }
        Command::Order {
            vault,
            path,
            before,
            after,
            reset,
        } => {
            let hub = ctx.hub(&[&vault]).await?;
            if reset {
                let n = op(ops::sort_folder(&hub, &vault, &path).await)?;
                crate::out!("{path}: A–Z ({n} item(s) had a place set by hand)");
                return Ok(());
            }
            let (other, after) = match (before, after) {
                (Some(o), _) => (o, false),
                (None, Some(o)) => (o, true),
                (None, None) => bail!("say where: --before OTHER or --after OTHER"),
            };
            op(ops::place(&hub, &vault, &path, &other, after).await)?;
            let side = if after { "after" } else { "before" };
            crate::out!("{path} is now {side} {other}");
            Ok(())
        }
        Command::Rm { vault, path } => {
            let hub = ctx.hub(&[&vault]).await?;
            let (_, ids) = op(ops::delete_document(&hub, &vault, &path).await)?;
            crate::out!(
                "moved {} item(s) to the trash (restore with `notekpr trash restore`)",
                ids.len()
            );
            Ok(())
        }
        Command::Trash(c) => trash(&ctx, c).await,
        Command::Search {
            query,
            vault,
            limit,
        } => {
            let q = query.join(" ");
            let refs: Vec<&str> = vault.as_deref().into_iter().collect();
            let hub = ctx.hub(&refs).await?;
            let hits = op(ops::search(&hub, &q, vault.as_deref(), limit).await)?;
            if ctx.json {
                ctx.print_json(&hits);
            } else if hits.is_empty() {
                crate::out!("no matches");
            } else {
                for h in hits {
                    crate::out!(
                        "{}:{}  ({:.1})\n    {}",
                        h.vault_name,
                        h.path,
                        h.score,
                        h.snippet
                    );
                }
            }
            Ok(())
        }
        Command::Backlinks { vault, path } => {
            let hub = ctx.hub(&[&vault]).await?;
            let docs = op(ops::backlinks(&hub, &vault, &path).await)?;
            if ctx.json {
                ctx.print_json(&docs);
            } else {
                for d in docs {
                    crate::out!("{}", d.path);
                }
            }
            Ok(())
        }
        Command::History(h) => history(&ctx, h).await,
        Command::Export {
            vault: Some(vault),
            out,
            force,
            ..
        } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let r = transfer::export(&hub, &v, &out, force).await?;
            print_transfer("exported", &r);
            Ok(())
        }
        Command::Export {
            vault: None,
            out,
            force,
            skip,
            no_quick_notes,
        } => {
            let hub = ctx.hub(&[]).await?;
            let r = transfer::export_account(&hub, &out, force, &skip, !no_quick_notes).await?;
            print_account_transfer("exported", &r, &out);
            Ok(())
        }
        Command::ExportEncrypted { out, force } => {
            if out.exists() && !force {
                bail!("{} exists (use --force to replace it)", out.display());
            }
            let s = ctx.session()?;
            let export = recover::download_export(&s).await?;
            let json = serde_json::to_vec(&export)?;
            fsutil::write_atomic(&out, &json, true)
                .with_context(|| format!("writing {}", out.display()))?;
            crate::out!(
                "Saved an encrypted export of {} vault(s) and {} shared note(s) to {} ({}). Keep it with your \
                 backups; `notekpr recover --from {}` turns it into Markdown files with your recovery key.",
                export.vaults.len(),
                export.shared_notes.len(),
                out.display(),
                ui::fmt_size(json.len() as u64),
                out.display()
            );
            Ok(())
        }
        Command::Recover(a) => recover_cmd(&ctx, a).await,
        Command::Import {
            vault: source,
            source: None,
            into,
            skip_existing,
            dry_run,
        } => {
            if into.is_some() || skip_existing {
                bail!(
                    "--into and --skip-existing are for importing into a vault: notekpr import <vault> <source>"
                );
            }
            let source = std::path::PathBuf::from(source);
            let hub = ctx.hub(&[]).await?;
            let Some(r) = transfer::import_account(&hub, &source, dry_run).await? else {
                bail!(
                    "{} isn't an export of vaults (it has no {}): to import files into a vault, name it — \
                     notekpr import <vault> {}",
                    source.display(),
                    notekpr_core::account_export::MANIFEST,
                    source.display()
                );
            };
            print_account_transfer(
                if dry_run {
                    "dry run: would import"
                } else {
                    "imported"
                },
                &r,
                &source,
            );
            Ok(())
        }
        Command::Import {
            vault,
            source: Some(source),
            into,
            skip_existing,
            dry_run,
        } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let r = transfer::import(&hub, &v, &source, into.as_deref(), skip_existing, dry_run)
                .await?;
            print_transfer(
                if dry_run {
                    "dry run: would import"
                } else {
                    "imported"
                },
                &r,
            );
            Ok(())
        }
        Command::Sync(a) => sync(&ctx, a).await,
        Command::Agent(c) => agent(&ctx, c).await,
        Command::Gateway(a) => gateway_cmd(&ctx, a).await,
    }
}

/// Let changing commands be handed to this daemon ([`control`]); without it
/// they are refused while it runs, as before. A gateway logs them.
async fn serve_control(
    ctx: &Ctx,
    hub: &Arc<Hub>,
    audit: Option<Arc<AuditLog>>,
) -> Option<control::Control> {
    match control::serve(&ctx.dirs, &ctx.profile, hub.clone(), audit).await {
        Ok(c) => Some(c),
        Err(e) => {
            ui::warn(&format!(
                "other notekpr commands can't hand changes to this process: {e:#}"
            ));
            None
        }
    }
}

async fn recover_cmd(ctx: &Ctx, a: RecoverArgs) -> Result<()> {
    let mut local = false;
    let report = match &a.from {
        Some(file) => {
            let export = recover::read_export(file)?;
            let key = match &a.recovery_key_file {
                Some(f) => std::fs::read_to_string(f)
                    .with_context(|| format!("reading {}", f.display()))?,
                None => match std::env::var("NOTEKPR_RECOVERY_KEY") {
                    Ok(k) if !k.trim().is_empty() => k,
                    _ => {
                        if !ui::stdin_is_tty() {
                            bail!(
                                "no recovery key: pass --recovery-key-file or set NOTEKPR_RECOVERY_KEY"
                            );
                        }
                        ui::prompt_secret("Recovery key: ")?
                    }
                },
            };
            let account = notekpr_core::recover::unlock_with_recovery_key(&export, key.trim())
                .map_err(|e| anyhow!("the recovery key does not open this export: {e}"))?;
            recover::recover_to(&export, &account, &a.out)?
        }
        None => {
            let s = ctx.session()?;
            if a.local {
                local = true;
                recover::recover_local(&s, &a.out)?
            } else {
                match recover::download_export(&s).await {
                    Ok(export) => recover::recover_with_device(&s, &export, &a.out)?,
                    // No server (or no longer signed in): what this device has.
                    Err(e)
                        if e.downcast_ref::<ApiError>().is_some_and(|e| {
                            e.is_offline() || e.is_unauthorized() || matches!(e, ApiError::Auth(_))
                        }) =>
                    {
                        ui::warn(&format!(
                            "{e:#}; recovering from this device's local copies instead (they may miss the latest changes)"
                        ));
                        local = true;
                        recover::recover_local(&s, &a.out)?
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    };
    if ctx.json {
        ctx.print_json(&json!({
            "vaults": report.vaults,
            "files": report.files,
            "problems": report.problems,
            "source": if local { "device" } else { "export" },
        }));
    } else {
        crate::out!(
            "Recovered {} file(s) from {} vault(s) into {}{}.",
            report.files,
            report.vaults,
            a.out.display(),
            if local {
                ", from this device's copies"
            } else {
                ""
            }
        );
        if !report.problems.is_empty() {
            crate::out!("Not recovered:");
            for p in &report.problems {
                crate::out!("  - {p}");
            }
        }
        if local {
            ui::status(
                "notes shared with you one by one aren't kept on devices: only an export has them",
            );
        }
        ui::status("these files are decrypted plain text: keep the folder private");
    }
    Ok(())
}

fn show_recovery_key(rk: &str) {
    let bar = "=".repeat(72);
    eprintln!(
        "\n{bar}\n  YOUR RECOVERY KEY (shown only this once):\n\n      {rk}\n\n  Write it down or store it in a password \
         manager. It is the only way to\n  get your notes back if you lose every device. Nobody - not even the\n  \
         server administrator - can recover your notes without it.\n{bar}\n"
    );
}

fn report_push(o: crate::session::PushOutcome) {
    report_pending(o.remaining, o.offline, o.read_only, o.vault_full, false);
}

/// What happens to changes saved here and not uploaded yet (`quick_notes`:
/// they were made in the quick notes).
fn report_pending(
    remaining: usize,
    offline: bool,
    read_only: bool,
    vault_full: bool,
    quick_notes: bool,
) {
    if read_only {
        ui::status(&format!(
            "saved locally; the server is read-only right now: {remaining} change(s) will be uploaded when it accepts changes again"
        ));
    } else if vault_full && quick_notes {
        ui::status(&format!(
            "saved locally; your personal storage is full: {remaining} change(s) will be uploaded once it has room (delete quick notes or saved shapes, or ask your administrator for more room)"
        ));
    } else if vault_full {
        ui::status(&format!(
            "saved locally; the vault is full: {remaining} change(s) will be uploaded once it has room (delete something in it for good, or ask your administrator to raise its limit: `notekpr vault list` shows its id)"
        ));
    } else if offline || remaining > 0 {
        ui::status(&format!(
            "saved locally; {remaining} change(s) will be uploaded when the server is reachable"
        ));
    }
}

fn print_account_transfer(verb: &str, r: &transfer::AccountReport, at: &std::path::Path) {
    crate::out!(
        "{verb} {} vault(s){}{}{}: {}",
        r.vaults.len(),
        if r.vaults.is_empty() {
            String::new()
        } else {
            format!(" ({})", r.vaults.join(", "))
        },
        if r.quick_notes > 0 {
            format!(" and {} quick note(s)", r.quick_notes)
        } else {
            String::new()
        },
        if r.saved_shapes > 0 {
            format!(" and {} saved shape(s)", r.saved_shapes)
        } else {
            String::new()
        },
        at.display()
    );
    print_transfer("in all:", &r.totals);
}

fn print_transfer(verb: &str, r: &transfer::TransferReport) {
    crate::out!(
        "{verb} {} note(s), {}{} attachment(s), {} folder(s){}{}",
        r.notes,
        if r.drawings > 0 {
            format!("{} drawing(s), ", r.drawings)
        } else {
            String::new()
        },
        r.attachments,
        r.folders,
        if r.comments > 0 {
            format!(", comments on {} note(s)", r.comments)
        } else {
            String::new()
        },
        if r.unchanged > 0 {
            format!(", {} unchanged", r.unchanged)
        } else {
            String::new()
        }
    );
    for (p, why) in &r.skipped {
        ui::warn(&format!("skipped {p}: {why}"));
    }
}

// ---------------------------------------------------------------- login & status

async fn login(ctx: &Ctx, a: LoginArgs) -> Result<()> {
    let store = ctx.store()?;
    let server = match a
        .server
        .or_else(|| store.load().ok().flatten().map(|p| p.server))
    {
        Some(s) => s,
        None => bail!(
            "--server URL is required for the first login of profile '{}'",
            ctx.profile
        ),
    };
    let out = account::login(
        &ctx.dirs,
        &ctx.profile,
        LoginOptions {
            server,
            insecure_http: a.insecure_http,
            dev_user: a.dev_user,
            device_name: a.device_name,
            recovery_key: match &a.recovery_key_file {
                Some(f) => Some(
                    std::fs::read_to_string(f)
                        .with_context(|| format!("reading {}", f.display()))?
                        .trim()
                        .to_string(),
                ),
                None => std::env::var("NOTEKPR_RECOVERY_KEY")
                    .ok()
                    .filter(|k| !k.trim().is_empty()),
            },
            interactive: true,
            approval_timeout: None,
        },
    )
    .await?;
    if let Some(rk) = &out.recovery_key {
        show_recovery_key(rk);
        if let Err(e) = account::recovery_key_shown(&ctx.dirs, &ctx.profile) {
            tracing::warn!("could not clear the pending account setup: {e:#}");
        }
    }
    let how = if out.recovery_key.is_some() {
        "account keys created on this device"
    } else if out.reused_device {
        "session renewed"
    } else if out.recovered {
        "device unlocked with the recovery key"
    } else {
        "device approved"
    };
    crate::out!(
        "Signed in as {} (profile '{}'): {how}.",
        out.user.display_name,
        ctx.profile
    );
    crate::out!("Account key fingerprint: {}", out.fingerprint);
    Ok(())
}

async fn status(ctx: &Ctx) -> Result<()> {
    let store = ctx.store()?;
    let p = store.require()?;
    if ctx.json {
        let mut out = json!({
            "profile": ctx.profile, "server": p.server, "kind": p.kind, "user_id": p.user_id,
            "display_name": p.display_name, "device_id": p.device_id, "device_pending": p.device_pending,
            "signed_in": p.session_token.is_some() || p.agent.is_some(),
            "mirror_dir": p.sync.mirror_dir,
        });
        if let Ok(s) = ctx.session() {
            let hub = Hub::start(
                s.clone(),
                HubOptions {
                    offline_ok: true,
                    detached: ctx.detached,
                    ..Default::default()
                },
            )
            .await?;
            out["online"] = json!(hub.is_online());
            out["fingerprint"] = json!(s.fingerprint());
            out["vaults"] = serde_json::to_value(hub.summaries().await)?;
        }
        ctx.print_json(&out);
        return Ok(());
    }
    crate::out!("Profile:   {} ({})", ctx.profile, p.kind.as_str());
    crate::out!("Server:    {}", p.server);
    crate::out!(
        "User:      {} ({})",
        p.display_name.clone().unwrap_or_default(),
        p.user_id.clone().unwrap_or_default()
    );
    if p.device_pending {
        crate::out!("Device:    waiting for approval — run `notekpr login` again to continue");
        return Ok(());
    }
    let session = ctx.session()?;
    crate::out!("Key:       {}", session.fingerprint());
    match (&p.session_token, p.kind) {
        (None, PrincipalKind::Human) => {
            crate::out!("Session:   signed out (local copies only) — run `notekpr login`")
        }
        (_, PrincipalKind::Agent) => crate::out!(
            "Session:   agent client credentials ({})",
            p.agent.as_ref().map(|a| a.client_id.as_str()).unwrap_or("")
        ),
        (Some(_), _) => {
            if let Some(exp) = p.session_expires_at {
                crate::out!("Session:   valid until {}", ui::fmt_time(exp));
            }
        }
    }
    let hub = Hub::start(
        session.clone(),
        HubOptions {
            offline_ok: true,
            detached: ctx.detached,
            ..Default::default()
        },
    )
    .await?;
    match session.server_info().await {
        Ok(i) => crate::out!("Online:    yes ({} {})", i.name, i.version),
        Err(e) => crate::out!("Online:    no ({e})"),
    }
    if p.kind == PrincipalKind::Human
        && hub.is_online()
        && let Ok(devs) = manage::devices(&session).await
    {
        crate::out!("Devices:");
        for d in devs {
            let cur = if Some(&d.id) == p.device_id.as_ref() {
                " (this device)"
            } else {
                ""
            };
            crate::out!("  {:<10} {}{cur}  [{}]", d.status.as_str(), d.name, d.id);
        }
    }
    crate::out!("Vaults:");
    for s in hub.summaries().await {
        let ro = s
            .read_only
            .as_ref()
            .map(|r| format!(" READ-ONLY: {r}"))
            .unwrap_or_default();
        let last = s
            .last_sync
            .map(ui::fmt_time)
            .unwrap_or_else(|| "never".into());
        let big = if s.too_large_changes > 0 {
            format!(" {} TOO LARGE to upload (kept here)", s.too_large_changes)
        } else {
            String::new()
        };
        crate::out!(
            "  {:<24} {:<7} {:>4} docs  {:>3} pending{big}  {}  synced {last}{}{ro}  [{}]",
            s.name,
            s.role.as_str(),
            s.documents,
            s.pending_changes,
            storage_text(&s),
            if s.archived { " (archived)" } else { "" },
            s.id
        );
    }
    for id in hub.waiting_for_keys() {
        crate::out!("  (shared with you, waiting for its key)  [{id}]");
    }
    match &p.sync.mirror_dir {
        Some(d) => crate::out!("Mirror:    {} (plaintext files)", d.display()),
        None => crate::out!("Mirror:    off (encrypted local copies only)"),
    }
    Ok(())
}

// ---------------------------------------------------------------- devices

async fn devices(ctx: &Ctx, c: DevicesCmd) -> Result<()> {
    let s = ctx.session()?;
    if s.kind == PrincipalKind::Agent {
        bail!("agents have no devices");
    }
    match c {
        DevicesCmd::List => {
            let list = manage::devices(&s).await?;
            if ctx.json {
                ctx.print_json(&list);
                return Ok(());
            }
            for d in &list {
                let cur = if Some(&d.id) == s.profile.device_id.as_ref() {
                    "  <- this device"
                } else {
                    ""
                };
                crate::out!(
                    "{:<10} {:<36} {:<30} added {}{cur}",
                    d.status.as_str(),
                    d.id,
                    d.name,
                    ui::fmt_time(d.created_at)
                );
            }
            let pending: Vec<_> = list
                .iter()
                .filter(|d| d.status == DeviceStatus::Pending)
                .collect();
            if !pending.is_empty() {
                crate::out!(
                    "\n{} device(s) waiting: `notekpr devices approve <id>`",
                    pending.len()
                );
            }
            Ok(())
        }
        DevicesCmd::Approve { id, yes } => {
            let (d, code) = manage::pending_device(&s, &id).await?;
            crate::out!("Device:            {}", d.name);
            crate::out!("Verification code: {code}");
            confirm_or_flag(yes, "Does the new device show exactly this code?")?;
            manage::approve_device(&s, &d).await?;
            crate::out!("Approved '{}'.", d.name);
            Ok(())
        }
        DevicesCmd::Revoke { id, yes } => {
            if Some(&id) == s.profile.device_id.as_ref() {
                ui::warn("this is the device you are using; it will be signed out");
            }
            confirm_or_flag(yes, &format!("Remove device {id} from your account?"))?;
            manage::revoke_device(&s, &id).await?;
            crate::out!(
                "Device removed. If it may be compromised, rotate the keys of your shared vaults (`vault rotate`)."
            );
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- vaults

/// What a vault takes on the server, of how much it may (`FULL`: its
/// changes wait here until it has room; the administrator who can give it
/// more finds it by its id).
fn storage_text(s: &crate::hub::VaultSummary) -> String {
    let used = ui::fmt_size(s.storage_bytes);
    let of = match s.storage_limit_mb.max() {
        Some(mb) => format!("{used} of {}", ui::fmt_size(mb.saturating_mul(MIB))),
        None => used,
    };
    if s.full { format!("{of} FULL") } else { of }
}

async fn vault(ctx: &Ctx, c: VaultCmd) -> Result<()> {
    match c {
        VaultCmd::List => {
            let hub = ctx.hub(&[]).await?;
            let list = hub.summaries().await;
            if ctx.json {
                ctx.print_json(&list);
                return Ok(());
            }
            // The quick notes are no vault: a line of their own.
            let (sections, list): (Vec<_>, Vec<_>) =
                list.into_iter().partition(|s| s.kind.is_section());
            if list.is_empty() {
                crate::out!("no vaults yet — create one with `notekpr vault create <name>`");
            }
            for s in list {
                let name = match &s.icon {
                    Some(i) => format!("{i} {}", s.name),
                    None => s.name.clone(),
                };
                // Drawings, when there are some or a limit to them.
                let drawings = if s.drawings > 0 || s.drawing_limit != Limit::Unlimited {
                    format!(
                        ", {}",
                        notekpr_core::drawings::count_text(s.drawings, s.drawing_limit)
                    )
                } else {
                    String::new()
                };
                crate::out!(
                    "{:<28} {:<7} {:>4} docs{drawings}  {}{}{}  [{}]",
                    name,
                    s.role.as_str(),
                    s.documents,
                    storage_text(&s),
                    if s.archived { "  (archived)" } else { "" },
                    s.read_only
                        .map(|r| format!("  READ-ONLY: {r}"))
                        .unwrap_or_default(),
                    s.id
                );
            }
            for id in hub.waiting_for_keys() {
                crate::out!("{:<28} (waiting for its key from the owner)  [{id}]", "?");
            }
            for s in sections {
                crate::out!(
                    "\n{}, {} (`notekpr quick --list`)",
                    quick_count(s.documents, s.note_limit),
                    storage_text(&s)
                );
            }
            Ok(())
        }
        VaultCmd::Create { name } => {
            let s = ctx.session()?;
            let v = manage::create_vault(&s, &name).await?;
            crate::out!("created vault '{}' [{}]", v.name(), v.id());
            Ok(())
        }
        VaultCmd::Rename { vault, name } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            manage::rename_vault(&hub.session, &v, &name).await?;
            crate::out!("renamed to '{name}'");
            Ok(())
        }
        VaultCmd::Icon {
            vault,
            icon,
            remove,
        } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let icon = if remove { None } else { icon };
            manage::set_vault_icon(&hub.session, &v, icon.as_deref()).await?;
            match icon {
                Some(i) => crate::out!("{i} {vault}"),
                None => crate::out!("removed the icon of {vault}"),
            }
            Ok(())
        }
        VaultCmd::Archive { vault, undo } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            manage::set_archived(&hub.session, &v, !undo).await?;
            crate::out!("{}", if undo { "unarchived" } else { "archived" });
            Ok(())
        }
        VaultCmd::ViewersComment { vault, setting } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let on = setting == "on";
            manage::set_viewers_comment(&hub.session, &v, on).await?;
            crate::out!(
                "viewers {} comment",
                if on { "can" } else { "can no longer" }
            );
            Ok(())
        }
        VaultCmd::Delete { vault, yes } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let name = v.lock().await.name().to_string();
            if !yes {
                if !ui::stdin_is_tty() {
                    bail!("pass --yes to delete without a prompt");
                }
                let typed = ui::prompt_line(&format!(
                    "This permanently deletes '{name}' and all its documents for every member.\nType the vault name to confirm: "
                ))?;
                if typed != name {
                    bail!("cancelled");
                }
            }
            manage::delete_vault(&hub.session, &v).await?;
            crate::out!("deleted '{name}'");
            Ok(())
        }
        VaultCmd::Members { vault } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            manage::grant_pending(&hub).await;
            let list = manage::members(&hub.session, &v).await?;
            if ctx.json {
                let j: Vec<_> = list
                    .iter()
                    .map(|m| {
                        json!({
                            "user_id": m.member.user.id, "name": m.member.user.display_name, "kind": m.member.user.kind,
                            "role": m.member.role, "has_key": m.member.has_current_key,
                            "fingerprint": m.key.as_ref().map(|k| k.fingerprint.clone()),
                            "pin": m.key.as_ref().map(|k| manage::pin_label(k.pin)),
                            "vouched_by_owner": m.signed.then_some(m.vouched),
                        })
                    })
                    .collect();
                ctx.print_json(&j);
                return Ok(());
            }
            for m in list {
                let fp = match (&m.key, &m.key_error) {
                    (Some(k), _) => format!("{}  ({})", k.fingerprint, manage::pin_label(k.pin)),
                    (None, Some(e)) => format!("KEY INVALID: {e}"),
                    (None, None) => "no keys yet".into(),
                };
                let key = if !m.vouched {
                    "  [NOT ADDED BY THE OWNER — the server lists them, the signed membership log doesn't]"
                } else if m.member.has_current_key {
                    ""
                } else {
                    "  [waiting for vault key]"
                };
                crate::out!(
                    "{:<24} {:<6} {:<7} {fp}{key}",
                    m.member.user.display_name,
                    m.member.user.kind.as_str(),
                    m.member.role.as_str()
                );
            }
            Ok(())
        }
        VaultCmd::Share {
            vault,
            user,
            role,
            accept_new_key,
        } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let role = match role {
                MemberRole::Admin => Role::Admin,
                MemberRole::Editor => Role::Editor,
                MemberRole::Viewer => Role::Viewer,
            };
            let out = manage::share(&hub.session, &v, &user, role.clone(), accept_new_key).await?;
            let who = format!("{} ({})", out.user.display_name, out.user.kind.as_str());
            if out.updated_role {
                crate::out!("{who} is now {}", role.as_str());
            } else {
                crate::out!("shared with {who} as {}", role.as_str());
            }
            match (&out.fingerprint, out.pin) {
                (Some(fp), Some(PinStatus::New)) => crate::out!(
                    "Their key fingerprint: {fp}\nCompare it with them (or with `agent setup` output for an agent) over a \
                     trusted channel. It is now pinned; you will be warned if it ever changes."
                ),
                (Some(fp), _) => {
                    crate::out!("Their key fingerprint: {fp} (matches the pinned one)")
                }
                (None, _) => crate::out!(
                    "{} has not set up encryption yet; the vault key is shared automatically by your client \
                     (run `notekpr sync` or `vault members`) once they have.",
                    out.user.display_name
                ),
            }
            if !out.key_shared && out.fingerprint.is_some() {
                ui::warn("the vault key was not shared");
            }
            Ok(())
        }
        VaultCmd::Unshare { vault, user } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let (u, epoch) = manage::unshare(&hub.session, &v, &user).await?;
            crate::out!(
                "removed {} and rotated the vault key (epoch {epoch}). They keep whatever they already downloaded, \
                 but cannot read anything written from now on.",
                u.display_name
            );
            Ok(())
        }
        VaultCmd::Rotate { vault } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let epoch = hub.session.rotate_keys(&v).await?;
            crate::out!("vault key rotated (epoch {epoch})");
            Ok(())
        }
        VaultCmd::Leave {
            vault,
            yes,
            remove_mirror,
        } => {
            // Opening the hub catches up and uploads queued changes first.
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let (name, vid, unsynced) = {
                let g = v.lock().await;
                (
                    g.name().to_string(),
                    g.id().to_string(),
                    g.replica.pending.len() + g.outbox.held_count() + g.pending_blobs.len(),
                )
            };
            let mut prompt = format!("Leave '{name}' and delete its local copy?");
            if unsynced > 0 {
                ui::warn(&format!(
                    "{unsynced} change(s) to '{name}' could not be uploaded and will be LOST"
                ));
                prompt =
                    format!("{unsynced} change(s) were never uploaded and will be lost. {prompt}");
            }
            confirm_or_flag(yes, &prompt)?;
            let ids = [vid].into_iter().collect();
            let mirror = mirror_folders_to_remove(ctx, &ids, yes, remove_mirror)?;
            manage::leave_vault(&hub.session, &v).await?;
            if let Some(folders) = mirror {
                remove_mirror_folders(&folders);
            }
            crate::out!("left '{name}'");
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- documents

async fn ls(ctx: &Ctx, vault: &str, folder: Option<&str>, recursive: bool) -> Result<()> {
    let hub = ctx.hub(&[vault]).await?;
    let v = hub.resolve(vault).await?;
    let g = v.lock().await;
    let list = g.list(folder.unwrap_or(""), recursive)?;
    if ctx.json {
        let j: Vec<_> = list
            .iter()
            .map(|l| match &l.entry {
                Some(e) if l.kind != ListKind::Folder => {
                    serde_json::to_value(ops::doc_info(&g, &l.path, e)).unwrap_or_default()
                }
                _ => json!({ "path": l.path, "kind": "folder" }),
            })
            .collect();
        ctx.print_json(&j);
        return Ok(());
    }
    // Names line up: when some entries have an icon, the others get as much
    // room.
    let icon_width = list
        .iter()
        .filter_map(|l| l.entry.as_ref()?.icon.as_deref())
        .map(ui::display_width)
        .max();
    let icon = |e: Option<&Entry>| match (icon_width, e.and_then(|e| e.icon.as_deref())) {
        (None, _) => String::new(),
        (Some(w), Some(i)) => format!("{i}{} ", " ".repeat(w - ui::display_width(i))),
        (Some(w), None) => " ".repeat(w + 1),
    };
    for l in list {
        match (l.kind, &l.entry) {
            (ListKind::Folder, e) => {
                crate::out!("{:>10}  {:<20}  {}{}/", "", "", icon(e.as_ref()), l.path)
            }
            (_, Some(e)) => {
                let size = if e.kind == EntryKind::Note {
                    g.note_text(&e.id).len() as u64
                } else {
                    e.size.unwrap_or(0)
                };
                crate::out!(
                    "{:>10}  {:<20}  {}{}",
                    ui::fmt_size(size),
                    ui::fmt_time(e.modified),
                    icon(Some(e)),
                    l.path
                )
            }
            _ => {}
        }
    }
    Ok(())
}

async fn cat(ctx: &Ctx, vault: &str, path: &str) -> Result<()> {
    let hub = ctx.hub(&[vault]).await?;
    let v = hub.resolve(vault).await?;
    let (_, e) = v
        .lock()
        .await
        .find(path)
        .ok_or_else(|| anyhow!("no document '{path}'"))?;
    let data = match e.kind {
        EntryKind::Note => {
            let doc = op(ops::read_document(&hub, vault, path).await)?;
            if ctx.json {
                ctx.print_json(&doc);
                return Ok(());
            }
            doc.content.into_bytes()
        }
        EntryKind::Attachment => hub.session.attachment(&v, &e.id).await?,
        // As a .drawing file, its pictures inside (with `--json`: its
        // details, file and words).
        EntryKind::Drawing if ctx.json => {
            ctx.print_json(&op(ops::read_document(&hub, vault, path).await)?);
            return Ok(());
        }
        EntryKind::Drawing => ops::drawing_file(&hub, &v, &e.id).await.into_bytes(),
        EntryKind::Folder => bail!("'{path}' is a folder; use `ls`"),
    };
    let mut out = std::io::stdout().lock();
    out.write_all(&data)?;
    out.flush()?;
    Ok(())
}

async fn write(
    ctx: &Ctx,
    vault: &str,
    path: &str,
    attachment: bool,
    append: bool,
    if_match: Option<&str>,
) -> Result<()> {
    let data = ui::read_stdin().context("reading stdin")?;
    let hub = ctx.hub(&[vault]).await?;
    let out = if attachment {
        op(ops::put_attachment(&hub, vault, path, &data, None).await)?
    } else if notekpr_core::paths::is_drawing(path) && !append {
        // A drawing: its file (ours, Excalidraw's or Obsidian's) on stdin,
        // or a PNG or SVG with the drawing inside.
        op(ops::put_drawing(&hub, vault, path, &data).await)?
    } else {
        let text = String::from_utf8(data)
            .map_err(|_| anyhow!("stdin is not UTF-8 text; use --attachment for binary files"))?;
        if append {
            op(ops::append_document(&hub, vault, path, &text).await)?
        } else {
            op(ops::put_document(&hub, vault, path, &text, if_match).await)?
        }
    };
    hub.snapshot_idle(Duration::ZERO, livesync::SESSION_MIN_UPDATES)
        .await;
    let what = if out.created {
        "created"
    } else if out.changed {
        "updated"
    } else {
        "unchanged"
    };
    crate::out!("{what} {}", out.path);
    let quick_notes = hub.resolve(vault).await?.lock().await.is_quick_notes();
    report_pending(
        out.pending_changes,
        false,
        out.read_only,
        out.vault_full,
        quick_notes,
    );
    Ok(())
}

/// Words joined with spaces, else stdin (asking for it on a terminal).
fn text_arg(words: &[String], what: &str) -> Result<String> {
    let text = if words.is_empty() {
        if ui::stdin_is_tty() {
            eprintln!("Type your {what}, then press Ctrl+D:");
        }
        String::from_utf8(ui::read_stdin().context("reading stdin")?)
            .map_err(|_| anyhow!("stdin is not UTF-8 text"))?
    } else {
        words.join(" ")
    };
    if text.trim().is_empty() {
        bail!("nothing to say: pass the text as an argument or on stdin");
    }
    Ok(text.trim_end().to_string())
}

async fn comments(ctx: &Ctx, c: CommentsCmd) -> Result<()> {
    use crate::vault::CommentAct;
    let (vault, note) = match &c {
        CommentsCmd::List { vault, note, .. }
        | CommentsCmd::Add { vault, note, .. }
        | CommentsCmd::Reply { vault, note, .. }
        | CommentsCmd::Edit { vault, note, .. }
        | CommentsCmd::Delete { vault, note, .. }
        | CommentsCmd::Resolve { vault, note, .. }
        | CommentsCmd::Reopen { vault, note, .. } => (vault.clone(), note.clone()),
    };
    let hub = ctx.hub(&[&vault]).await?;
    let (id, act) = match c {
        CommentsCmd::List { all, .. } => {
            let list = op(ops::comments(&hub, &vault, &note).await)?;
            print_threads(ctx, list, all);
            return Ok(());
        }
        CommentsCmd::Add { on, text, .. } => {
            let body = text_arg(&text, "comment")?;
            let sent = op(ops::add_comment(&hub, &vault, &note, &on, &body).await)?;
            print_sent(ctx, &hub.session, "commented", &sent).await;
            return Ok(());
        }
        CommentsCmd::Reply { id, text, .. } => (id, CommentAct::Reply(text_arg(&text, "reply")?)),
        CommentsCmd::Edit { id, text, .. } => (id, CommentAct::Edit(text_arg(&text, "comment")?)),
        CommentsCmd::Delete { id, .. } => (id, CommentAct::Delete),
        CommentsCmd::Resolve { id, .. } => (id, CommentAct::Resolve),
        CommentsCmd::Reopen { id, .. } => (id, CommentAct::Reopen),
    };
    let what = match &act {
        CommentAct::Reply(_) => "replied",
        CommentAct::Edit(_) => "changed",
        CommentAct::Delete => "deleted",
        CommentAct::Resolve => "resolved",
        CommentAct::Reopen => "reopened",
    };
    let sent = op(ops::comment_action(&hub, &vault, &note, &id, act).await)?;
    print_sent(ctx, &hub.session, what, &sent).await;
    Ok(())
}

fn print_threads(ctx: &Ctx, mut list: ops::NoteComments, all: bool) {
    if !all {
        list.threads.retain(|t| t.resolved_by.is_none());
    }
    if ctx.json {
        ctx.print_json(&list);
        return;
    }
    if list.threads.is_empty() {
        crate::out!(
            "no {}comments on {}",
            if all { "" } else { "open " },
            list.path
        );
    }
    for t in &list.threads {
        let state = match (&t.resolved_by, t.on_text) {
            (Some(_), _) => " (resolved)",
            (None, false) => " (its text was deleted)",
            _ => "",
        };
        crate::out!("“{}”{state}", t.quote);
        for (i, c) in t.comments.iter().enumerate() {
            let who = c.author_name.clone().unwrap_or_else(|| c.author.clone());
            let mark = match (c.verified, c.pending, c.edited) {
                (false, _, _) => " (unverified)",
                (_, true, _) => " (not sent yet)",
                (_, _, true) => " (edited)",
                _ => "",
            };
            let indent = if i == 0 { "  " } else { "    ↳ " };
            crate::out!(
                "{indent}[{}] {who}, {}{mark}: {}",
                &c.id[..c.id.len().min(8)],
                ui::fmt_time(c.created),
                c.body.replace('\n', "\n      ")
            );
        }
    }
}

/// Say what was sent, whom it mentions, and which `@…` mention nobody.
async fn print_sent(ctx: &Ctx, session: &Session, what: &str, sent: &ops::CommentSent) {
    if ctx.json {
        ctx.print_json(sent);
        return;
    }
    let m = &sent.mentioned;
    let mut line = format!("{what} [{}]", &sent.id[..sent.id.len().min(8)]);
    if !m.mentions.is_empty() {
        let names = ops::user_names(session, m.mentions.clone()).await;
        let who: Vec<String> = m
            .mentions
            .iter()
            .map(|u| names.get(u).cloned().unwrap_or_else(|| u.clone()))
            .collect();
        line.push_str(&format!(", mentioning {}", who.join(", ")));
    }
    crate::out!("{line}");
    if m.people_unknown {
        ui::warn("couldn't look up who may be mentioned (offline?): nobody was mentioned");
    } else {
        for w in &m.not_mentioned {
            ui::warn(&format!(
                "@{w}: nobody here by that name, not mentioned (only the vault's members can be)"
            ));
        }
    }
}

async fn shared_comments(ctx: &Ctx, s: &Session, c: SharedCommentsCmd) -> Result<()> {
    use crate::vault::CommentAct;
    use note_shares::SharedCommentAct as Act;
    let (note, act, what) = match c {
        SharedCommentsCmd::List { note, all } => {
            let list = note_shares::shared_comments(s, &note).await?;
            print_threads(ctx, list, all);
            return Ok(());
        }
        SharedCommentsCmd::Add { note, on, text } => (
            note,
            Act::Add {
                on,
                text: text_arg(&text, "comment")?,
            },
            "commented",
        ),
        SharedCommentsCmd::Reply { note, id, text } => (
            note,
            Act::On {
                id,
                act: CommentAct::Reply(text_arg(&text, "reply")?),
            },
            "replied",
        ),
        SharedCommentsCmd::Edit { note, id, text } => (
            note,
            Act::On {
                id,
                act: CommentAct::Edit(text_arg(&text, "comment")?),
            },
            "changed",
        ),
        SharedCommentsCmd::Delete { note, id } => (
            note,
            Act::On {
                id,
                act: CommentAct::Delete,
            },
            "deleted",
        ),
        SharedCommentsCmd::Resolve { note, id } => (
            note,
            Act::On {
                id,
                act: CommentAct::Resolve,
            },
            "resolved",
        ),
        SharedCommentsCmd::Reopen { note, id } => (
            note,
            Act::On {
                id,
                act: CommentAct::Reopen,
            },
            "reopened",
        ),
    };
    let sent = note_shares::shared_comment(s, &note, act).await?;
    print_sent(ctx, s, what, &sent).await;
    Ok(())
}

/// `mentions`: comments that mention you, newest first.
async fn mentions(ctx: &Ctx, a: MentionsArgs) -> Result<()> {
    let refs: Vec<&str> = a.vault.as_deref().into_iter().collect();
    let hub = ctx.hub(&refs).await?;
    let (mut list, known) = op(crate::mentions::list(&hub, a.vault.as_deref()).await)?;
    if !known {
        ui::warn("couldn't read which mentions you have read (offline?): all are shown as new");
    }
    if a.unread {
        list.retain(|m| m.unread);
    }
    let marked = if a.mark_read {
        crate::mentions::mark_read(&hub.session, &list).await?;
        list.len()
    } else {
        0
    };
    list.truncate(a.limit);
    if ctx.json {
        ctx.print_json(&list);
    } else if list.is_empty() {
        crate::out!(
            "{}",
            if a.unread {
                "no new mentions"
            } else {
                "nobody has mentioned you"
            }
        );
    } else {
        for m in &list {
            let who = m.author_name.clone().unwrap_or_else(|| m.author.clone());
            crate::out!(
                "{}{}:{}  [{}] {who}, {}{}",
                if m.unread { "* " } else { "  " },
                m.vault,
                m.path,
                &m.comment[..m.comment.len().min(8)],
                ui::fmt_time(m.created),
                if m.resolved { " (resolved)" } else { "" }
            );
            crate::out!("    on “{}”: {}", m.quote, m.body.replace('\n', "\n    "));
        }
    }
    if marked > 0 {
        ui::status(&format!("marked {marked} mention(s) read"));
    }
    Ok(())
}

/// `quick`: add a quick note, list them, delete one for good or move one to
/// a vault. Only the quick notes are opened (and the vault moved to).
async fn quick(ctx: &Ctx, a: QuickArgs) -> Result<()> {
    if a.list {
        let hub = ctx.hub_with_quick_notes(&[]).await?;
        let q = op(ops::list_quick_notes(&hub, a.limit).await)?;
        if ctx.json {
            ctx.print_json(&q);
        } else if q.count == 0 {
            crate::out!("no quick notes yet — add one with `notekpr quick <text>`");
        } else {
            crate::out!("{}", quick_count(q.count, q.limit));
            for n in &q.notes {
                crate::out!(
                    "{:<18}  {}{}",
                    n.title,
                    n.preview,
                    if n.waiting_for_room {
                        "  (on this device until there's room)"
                    } else {
                        ""
                    }
                );
            }
            if q.notes.len() < q.count {
                crate::out!("… and {} older (--limit)", q.count - q.notes.len());
            }
        }
        return Ok(());
    }
    if let Some(note) = &a.delete {
        let hub = ctx.hub_with_quick_notes(&[]).await?;
        let path = op(ops::delete_quick_note(&hub, note).await)?;
        crate::out!("deleted '{}' for good", notekpr_core::paths::title(&path));
        return Ok(());
    }
    if let Some(note) = &a.move_note {
        let to = a.to.clone().unwrap_or_default();
        let hub = ctx.hub_with_quick_notes(&[&to]).await?;
        let m = op(ops::move_to_vault(
            &hub,
            None,
            note,
            &to,
            a.folder.as_deref().unwrap_or(""),
            a.name.as_deref(),
        )
        .await)?;
        if ctx.json {
            ctx.print_json(&m);
            return Ok(());
        }
        let name = hub.resolve(&m.vault).await?.lock().await.name().to_string();
        crate::out!("moved to '{name}': {}", m.path);
        if !m.removed {
            ui::warn(&format!(
                "the copy in '{name}' isn't on the server yet, so the quick note is kept; delete it with \
                 `notekpr quick --delete \"{}\"` once it is",
                notekpr_core::paths::title(&m.from_path)
            ));
        }
        return Ok(());
    }
    let text = if a.text.is_empty() {
        if ui::stdin_is_tty() {
            eprintln!("Type your quick note, then press Ctrl+D:");
        }
        String::from_utf8(ui::read_stdin().context("reading stdin")?)
            .map_err(|_| anyhow!("stdin is not UTF-8 text"))?
    } else {
        a.text.join(" ")
    };
    if text.trim().is_empty() {
        bail!("nothing to save: pass the text as an argument or on stdin");
    }
    let hub = ctx.hub_with_quick_notes(&[]).await?;
    let out = op(ops::create_quick_note(&hub, &text).await)?;
    hub.snapshot_idle(Duration::ZERO, livesync::SESSION_MIN_UPDATES)
        .await;
    if ctx.json {
        ctx.print_json(&out);
    } else {
        let q = op(ops::list_quick_notes(&hub, 0).await)?;
        crate::out!(
            "added {} ({})",
            notekpr_core::paths::title(&out.path),
            quick_count(q.count, q.limit)
        );
    }
    report_pending(
        out.pending_changes,
        false,
        out.read_only,
        out.vault_full,
        true,
    );
    Ok(())
}

async fn shapes(ctx: &Ctx, c: ShapesCmd) -> Result<()> {
    // They are kept in the Personal storage, with the quick notes.
    let hub = ctx.hub_with_quick_notes(&[]).await?;
    match c {
        ShapesCmd::Ls => {
            let items = ops::list_library(&hub).await;
            if ctx.json {
                ctx.print_json(&items);
            } else if items.is_empty() {
                crate::out!(
                    "no saved shapes yet — save some in a drawing, or with `notekpr shapes import <file.excalidrawlib>`"
                );
            } else {
                for i in &items {
                    let mut kinds: Vec<(&str, usize)> = Vec::new();
                    for k in &i.elements {
                        match kinds.iter_mut().find(|(n, _)| n == k) {
                            Some((_, c)) => *c += 1,
                            None => kinds.push((k, 1)),
                        }
                    }
                    let kinds: Vec<String> = kinds
                        .into_iter()
                        .map(|(k, c)| {
                            if c == 1 {
                                k.to_string()
                            } else {
                                format!("{c} × {k}")
                            }
                        })
                        .collect();
                    crate::out!(
                        "{:<8}  {:<20}  {:<24}  {}",
                        i.id.chars().take(8).collect::<String>(),
                        ui::fmt_time(i.created),
                        i.name.as_deref().unwrap_or("(no name)"),
                        kinds.join(", ")
                    );
                }
            }
        }
        ShapesCmd::Import { file } => {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let r = op(ops::import_library(&hub, &text).await)?;
            if ctx.json {
                ctx.print_json(&r);
            } else {
                let mut said = vec![format!("saved {} shape(s)", r.added)];
                if r.already > 0 {
                    said.push(format!("{} saved already", r.already));
                }
                crate::out!("{}", said.join("; "));
                if r.too_large > 0 {
                    ui::warn(&format!(
                        "{} shape(s) were left out: too large for this server",
                        r.too_large
                    ));
                }
            }
            report_pending(r.pending_changes, false, r.read_only, r.vault_full, false);
        }
        ShapesCmd::Export { out, force } => {
            let items = ops::library(&hub).await;
            let file = notekpr_draw::file::write_library(&items);
            match out {
                None => {
                    let mut o = std::io::stdout().lock();
                    o.write_all(file.as_bytes())?;
                    o.write_all(b"\n")?;
                    o.flush()?;
                }
                Some(out) => {
                    if out.exists() && !force {
                        bail!("{} exists (use --force to replace it)", out.display());
                    }
                    fsutil::write_atomic(&out, file.as_bytes(), true)
                        .with_context(|| format!("writing {}", out.display()))?;
                    crate::out!("wrote {} shape(s) to {}", items.len(), out.display());
                }
            }
        }
    }
    Ok(())
}

/// "3 of 20 quick notes".
fn quick_count(count: usize, limit: Option<notekpr_core::protocol::Limit>) -> String {
    match limit.and_then(|l| l.max()) {
        Some(max) => format!("{count} of {max} quick notes"),
        None if count == 1 => "1 quick note".to_string(),
        None => format!("{count} quick notes"),
    }
}

async fn trash(ctx: &Ctx, c: TrashCmd) -> Result<()> {
    match c {
        TrashCmd::List { vault } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let list = v.lock().await.trashed();
            // Deleted for good, still kept by the server (owner and admins).
            let gone = match ops::deleted_for_good(&hub, &vault).await {
                Ok(g) => g,
                Err(e) => {
                    ui::warn(&format!("could not list what was deleted for good: {e}"));
                    vec![]
                }
            };
            if ctx.json {
                #[derive(Serialize)]
                struct Item {
                    #[serde(flatten)]
                    entry: Entry,
                    #[serde(skip_serializing_if = "Option::is_none")]
                    kept_until: Option<i64>,
                }
                let items: Vec<Item> = list
                    .into_iter()
                    .map(|entry| Item {
                        entry,
                        kept_until: None,
                    })
                    .chain(gone.into_iter().map(|(entry, until)| Item {
                        entry,
                        kept_until: Some(until),
                    }))
                    .collect();
                ctx.print_json(&items);
                return Ok(());
            }
            if list.is_empty() {
                crate::out!("the trash is empty");
            }
            for e in list {
                crate::out!(
                    "{}  {:<10}  {}  [{}]",
                    ui::fmt_time(e.trashed.unwrap_or(0)),
                    e.kind.as_str(),
                    e.path,
                    e.id
                );
            }
            if !gone.is_empty() {
                crate::out!(
                    "deleted for good, can be undone (`notekpr trash undo <vault> <entry>`):"
                );
                for (e, until) in gone {
                    crate::out!(
                        "{}  {:<10}  {}  [{}]  kept until {}",
                        ui::fmt_time(e.purged.unwrap_or(0)),
                        e.kind.as_str(),
                        e.path,
                        e.id,
                        ui::fmt_time(until)
                    );
                }
            }
            Ok(())
        }
        TrashCmd::Undo { vault, entry } => {
            let hub = ctx.hub(&[&vault]).await?;
            let e = op(ops::undo_purge(&hub, &vault, &entry).await)?;
            crate::out!(
                "{} is back in the trash (`notekpr trash restore` takes it out)",
                e.path
            );
            Ok(())
        }
        TrashCmd::Restore { vault, entry } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let (vid, e) = {
                let mut g = v.lock().await;
                let e = g.restore(&entry)?;
                g.save()?;
                (g.id().to_string(), e)
            };
            report_push(hub.local_changed(&vid, vec![], true).await);
            crate::out!("restored {}", e.path);
            Ok(())
        }
        TrashCmd::Empty { vault, yes } => {
            let hub = ctx.hub(&[&vault]).await?;
            confirm_or_flag(yes, "Delete everything in the trash for good?")?;
            let n = op(ops::empty_trash(&hub, &vault).await)?;
            crate::out!("deleted {n} item(s) for good");
            Ok(())
        }
    }
}

async fn history(ctx: &Ctx, h: HistoryArgs) -> Result<()> {
    match h.cmd {
        Some(HistoryCmd::Show {
            vault,
            path,
            revision,
        }) => {
            let hub = ctx.hub(&[&vault]).await?;
            let text = op(ops::revision(&hub, &vault, &path, &revision).await)?;
            ui::out_text(&text);
            Ok(())
        }
        Some(HistoryCmd::Restore {
            vault,
            path,
            revision,
        }) => {
            let hub = ctx.hub(&[&vault]).await?;
            let text = op(ops::revision(&hub, &vault, &path, &revision).await)?;
            let out = op(ops::put_document(&hub, &vault, &path, &text, None).await)?;
            crate::out!("{} restored to revision {revision}", out.path);
            Ok(())
        }
        None => {
            let (Some(vault), Some(path)) = (h.vault, h.path) else {
                bail!(
                    "usage: notekpr history <vault> <path>  |  history show <vault> <path> <revision>"
                );
            };
            let hub = ctx.hub(&[&vault]).await?;
            let revs = op(ops::history(&hub, &vault, &path).await)?;
            if ctx.json {
                ctx.print_json(&revs);
                return Ok(());
            }
            if revs.is_empty() {
                crate::out!(
                    "no stored revisions yet (they are created as a document accumulates edits)"
                );
            }
            for r in revs {
                crate::out!(
                    "{}  {:>10}  {}",
                    ui::fmt_time(r.created_at),
                    ui::fmt_size(r.size as u64),
                    r.id
                );
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- sync daemon

fn plaintext_note(dir: &Path) -> String {
    format!(
        "Plaintext mirror: decrypted Markdown files are written to {}.\n\
         They are protected only by file permissions and your disk encryption — anyone (or any program)\n\
         that can read this directory can read these notes, and backup/cloud-sync tools may copy them.\n\
         The encrypted copies and everything on the server stay end-to-end encrypted.",
        dir.display()
    )
}

/// Resolve on Ctrl-C or SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
}

type Task = tokio::task::JoinHandle<Result<()>>;

/// Wait for `stop`, or for a background task to end — which they never do on
/// their own. The process then exits with the reason (so a service manager
/// restarts it) instead of carrying on half-dead, e.g. syncing replicas while
/// the Markdown mirror silently stopped. Returns the tasks still running.
async fn supervise(
    stop: impl std::future::Future<Output = ()>,
    mut live: Task,
    mut mirror: Option<Task>,
) -> (Result<()>, Option<Task>, Option<Task>) {
    type Joined = std::result::Result<Result<()>, tokio::task::JoinError>;
    enum Ended {
        Stop,
        Live(Joined),
        Mirror(Joined),
    }
    let ended = {
        let mirror_done = async {
            match mirror.as_mut() {
                Some(t) => t.await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = stop => Ended::Stop,
            r = &mut live => Ended::Live(r),
            r = mirror_done => Ended::Mirror(r),
        }
    };
    match ended {
        Ended::Stop => (Ok(()), Some(live), mirror),
        Ended::Live(r) => {
            let r = match r {
                Ok(r) => r,
                Err(e) => Err(anyhow!("the sync task failed: {e}")),
            };
            (r, None, mirror)
        }
        Ended::Mirror(r) => {
            let e = match r {
                Ok(Ok(())) => anyhow!("the Markdown mirror stopped unexpectedly"),
                Ok(Err(e)) => e.context("the Markdown mirror failed"),
                Err(e) => anyhow!("the Markdown mirror failed: {e}"),
            };
            (Err(e), Some(live), None)
        }
    }
}

async fn sync(ctx: &Ctx, a: SyncArgs) -> Result<()> {
    let store = ctx.store()?;
    let profile = store.require()?;
    if a.no_dir {
        store.update(|p| p.sync.mirror_dir = None)?;
        crate::out!("mirroring disabled (existing files are left in place)");
        return Ok(());
    }
    let dir = match a.dir.clone().or(profile.sync.mirror_dir.clone()) {
        Some(d) => {
            Some(std::path::absolute(&d).with_context(|| format!("resolving {}", d.display()))?)
        }
        None => None,
    };
    // A remembered mirror directory that is gone is never re-created
    // silently: every note in it would look deleted.
    if a.dir.is_none()
        && !a.restore_missing
        && let Some(d) = &dir
        && d.symlink_metadata().is_err()
    {
        bail!(
            "the remembered mirror directory {} does not exist (moved, renamed or not mounted?). Nothing was \
             changed. Put it back and start again, use --dir to mirror somewhere else, --restore-missing to write \
             a fresh copy there, or `notekpr sync --no-dir` to stop mirroring",
            d.display()
        );
    }
    let mirror_opts = MirrorOptions {
        allow_mass_delete: a.allow_mass_delete,
        restore_missing: a.restore_missing,
        ..Default::default()
    };
    if let Some(d) = &dir {
        // Confirmed once per directory, not once per profile.
        if !profile.sync.plaintext_confirmed(d) && !a.plaintext_ok {
            eprintln!("{}\n", plaintext_note(d));
            if !ui::stdin_is_tty() {
                bail!(
                    "writing decrypted files to {} must be confirmed once: re-run with --plaintext-ok",
                    d.display()
                );
            }
            let answer = ui::prompt_line("Type 'yes' to enable the plaintext mirror: ")?;
            if answer != "yes" {
                bail!(
                    "cancelled; run `notekpr sync` without --dir to keep only encrypted local copies"
                );
            }
        } else if a.dir.is_some() {
            ui::status(&plaintext_note(d));
        }
        if a.dir.is_none() {
            // Plain `sync` reuses the remembered mirror: say so.
            ui::status(&format!(
                "mirroring decrypted files into {} (remembered from an earlier `sync --dir`; \
                 `notekpr sync --no-dir` stops that)",
                d.display()
            ));
        }
        store.update(|p| {
            p.sync.confirm_plaintext(d);
            p.sync.mirror_dir = Some(d.clone());
        })?;
    }
    let session = ctx.session()?;
    let hub = Hub::start(
        session,
        HubOptions {
            vaults: a.vaults.clone(),
            read_only: false,
            offline_ok: true,
            catch_up: true,
            detached: false,
            quick_notes: false,
        },
    )
    .await?;
    if a.once {
        if let Some(d) = &dir {
            let mut m = Mirror::with_options(hub.clone(), d, mirror_opts).await?;
            let mut r = m.reconcile_all(true).await?;
            if r.held > 0
                && ui::confirm(&format!(
                    "{} mirrored file(s) disappeared at once (see above). Move the notes to the trash?",
                    r.held
                ))
            {
                m.set_options(MirrorOptions {
                    allow_mass_delete: true,
                    ..mirror_opts
                });
                r = m.reconcile_all(true).await?;
            }
            ui::status(&format!(
                "mirror: {} edit(s) and {} new file(s) uploaded, {} moved, {} trashed, {} file(s) written, {} conflict copies",
                r.ingested, r.created, r.renamed, r.trashed, r.written, r.conflicts
            ));
            if r.held > 0 {
                ui::warn(&format!(
                    "{} missing file(s) were NOT moved to the trash (held for safety)",
                    r.held
                ));
            }
        }
        let mut pending = 0;
        let mut read_only = false;
        let mut full = Vec::new();
        for id in hub.vault_ids() {
            match hub.sync_vault(&id).await {
                Ok(o) => {
                    pending += o.remaining;
                    read_only |= o.read_only;
                    if o.vault_full
                        && let Some(v) = hub.get(&id)
                    {
                        let g = v.lock().await;
                        full.push(if g.is_quick_notes() {
                            "your quick notes".to_string()
                        } else {
                            format!("'{}' [{id}]", g.name())
                        });
                    }
                }
                Err(e) => ui::warn(&format!("vault {id}: {e:#}")),
            }
        }
        if read_only {
            ui::warn(&format!(
                "the server is read-only right now: {pending} change(s) stay queued and are uploaded when it accepts changes again"
            ));
        }
        if !full.is_empty() {
            ui::warn(&format!(
                "full, so its changes stay queued until it has room (delete something in it for good, or ask your administrator to raise its limit, giving the id): {}",
                full.join(", ")
            ));
        }
        hub.snapshot_idle(Duration::ZERO, livesync::SESSION_MIN_UPDATES)
            .await;
        hub.save_all().await;
        let n = hub.vault_ids().len();
        if hub.is_online() {
            crate::out!(
                "synced {n} vault(s){}",
                if pending > 0 {
                    format!("; {pending} change(s) still queued")
                } else {
                    String::new()
                }
            );
        } else {
            crate::out!(
                "offline: {n} local vault(s) up to date with your edits; {pending} change(s) queued for upload"
            );
        }
        return Ok(());
    }

    // While running, changing commands are handed to us (they can't open
    // the local copies we own).
    let _control = serve_control(ctx, &hub, None).await;
    let (stop_tx, stop_rx) = watch::channel(false);
    let live = tokio::spawn(livesync::run(hub.clone(), stop_rx.clone()));
    let mirror_task = match &dir {
        Some(d) => {
            let mut m = Mirror::with_options(hub.clone(), d, mirror_opts).await?;
            m.reconcile_all(false).await?;
            ui::status(&format!(
                "mirroring {} vault(s) into {}",
                hub.vault_ids().len(),
                d.display()
            ));
            Some(tokio::spawn(m.run(stop_rx.clone())))
        }
        None => {
            ui::status(
                "keeping encrypted local copies in sync (no plaintext mirror; use --dir to add one)",
            );
            None
        }
    };
    ui::status("press Ctrl-C to stop");
    let (result, live, mirror_task) = supervise(shutdown_signal(), live, mirror_task).await;
    if let Err(e) = &result {
        ui::warn(&format!("stopping: {e:#}"));
    }
    let _ = stop_tx.send(true);
    if let Some(t) = mirror_task {
        let _ = tokio::time::timeout(Duration::from_secs(5), t).await;
    }
    if let Some(t) = live {
        let _ = tokio::time::timeout(Duration::from_secs(5), t).await;
    }
    for id in hub.vault_ids() {
        let _ = tokio::time::timeout(Duration::from_secs(10), hub.push(&id)).await;
    }
    // The editing session ends with the process: keep a revision of what changed.
    hub.snapshot_idle(Duration::ZERO, livesync::SESSION_MIN_UPDATES)
        .await;
    hub.save_all().await;
    ui::status("stopped");
    result
}

// ---------------------------------------------------------------- agents

fn write_secret_file(path: &Path, secret: &str) -> Result<()> {
    if path.exists() {
        bail!(
            "{} already exists; choose another --secret-out",
            path.display()
        );
    }
    crate::fsutil::create_private_file(path, format!("{secret}\n").as_bytes())
        .with_context(|| format!("writing {}", path.display()))
}

async fn resolve_agent(s: &Session, reference: &str) -> Result<notekpr_core::protocol::AgentInfo> {
    let list = manage::agents(s).await?;
    let mut found: Vec<_> = list
        .into_iter()
        .filter(|a| a.id == reference || a.display_name.eq_ignore_ascii_case(reference))
        .collect();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => bail!("no agent '{reference}' (see `notekpr agent list`)"),
        _ => bail!("several agents are named '{reference}'; use the id"),
    }
}

async fn agent(ctx: &Ctx, c: AgentCmd) -> Result<()> {
    match c {
        AgentCmd::Setup {
            server,
            client_id,
            client_secret_file,
            insecure_http,
        } => {
            let out = account::agent_setup(
                &ctx.dirs,
                &ctx.profile,
                AgentSetupOptions {
                    server,
                    insecure_http,
                    client_id,
                    client_secret_file,
                },
            )
            .await?;
            crate::out!(
                "Agent '{}' is set up in profile '{}'{}.",
                out.display_name,
                ctx.profile,
                if out.created_keys {
                    " (new key pair created)"
                } else {
                    ""
                }
            );
            crate::out!("Agent key fingerprint: {}", out.fingerprint);
            crate::out!(
                "Ask the owner to check this fingerprint before sharing a vault with the agent\n\
                 (`notekpr vault share <vault> \"{}\"` shows the same value).",
                out.display_name
            );
            Ok(())
        }
        AgentCmd::Create { name, secret_out } => {
            let s = ctx.session()?;
            let created = manage::create_agent(&s, &name).await?;
            write_secret_file(&secret_out, &created.credential.client_secret)?;
            crate::out!(
                "created agent '{}' [{}]",
                created.agent.display_name,
                created.agent.id
            );
            crate::out!("client id:     {}", created.credential.client_id);
            crate::out!(
                "client secret: written to {} (mode 0600)",
                secret_out.display()
            );
            crate::out!(
                "On the agent's machine run:\n  notekpr --profile <name> agent setup --server {} --client-id {} \
                 --client-secret-file <file>",
                s.profile.server,
                created.credential.client_id
            );
            Ok(())
        }
        AgentCmd::List => {
            let s = ctx.session()?;
            let list = manage::agents(&s).await?;
            if ctx.json {
                ctx.print_json(&list);
                return Ok(());
            }
            for a in list {
                let fp = match &a.account_key {
                    Some(k) => k
                        .verify()
                        .map(|p| p.fingerprint())
                        .unwrap_or_else(|_| "INVALID KEY".into()),
                    None => "not set up yet".into(),
                };
                let creds: Vec<&str> = a
                    .credentials
                    .iter()
                    .filter(|c| c.revoked_at.is_none())
                    .map(|c| c.client_id.as_str())
                    .collect();
                crate::out!(
                    "{:<24} {fp}  credentials: {}  [{}]",
                    a.display_name,
                    creds.join(", "),
                    a.id
                );
            }
            Ok(())
        }
        AgentCmd::Fingerprint { agent } => {
            let s = ctx.session()?;
            let a = resolve_agent(&s, &agent).await?;
            match manage::agent_fingerprint(&s, &a.id).await? {
                Some(fp) => crate::out!("{fp}"),
                None => crate::out!("'{}' has not run `agent setup` yet", a.display_name),
            }
            Ok(())
        }
        AgentCmd::RotateSecret {
            agent,
            secret_out,
            keep_old,
            yes,
        } => {
            let s = ctx.session()?;
            let a = resolve_agent(&s, &agent).await?;
            if secret_out.exists() {
                bail!(
                    "{} already exists; choose another --secret-out",
                    secret_out.display()
                );
            }
            if !keep_old {
                confirm_or_flag(
                    yes,
                    &format!(
                        "Revoke the current credentials of '{}'? It stops working until `agent setup` runs again \
                         with the new secret (use --keep-old to revoke later).",
                        a.display_name
                    ),
                )?;
            }
            let c = manage::rotate_agent_secret(&s, &a.id, !keep_old, |c| {
                write_secret_file(&secret_out, &c.client_secret)
            })
            .await?;
            crate::out!(
                "new client id: {}\nclient secret written to {}",
                c.client_id,
                secret_out.display()
            );
            if !keep_old {
                crate::out!(
                    "old credentials revoked; re-run `agent setup` on the agent's machine with the new id and file"
                );
            }
            Ok(())
        }
        AgentCmd::Delete { agent, yes } => {
            let s = ctx.session()?;
            let a = resolve_agent(&s, &agent).await?;
            confirm_or_flag(
                yes,
                &format!("Delete agent '{}' and all its access?", a.display_name),
            )?;
            manage::delete_agent(&s, &a.id).await?;
            crate::out!(
                "deleted '{}'. The keys of the vaults and notes it could read are renewed the next time you sync them (`notekpr sync`).",
                a.display_name
            );
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- gateway

fn gateway_token(store: &ProfileStore, rotate: bool) -> Result<(String, bool)> {
    store.update(|p| match (&p.gateway.token, rotate) {
        (Some(t), false) => (t.clone(), false),
        _ => {
            let t = gateway::new_token();
            p.gateway.token = Some(t.clone());
            (t, true)
        }
    })
}

async fn gateway_cmd(ctx: &Ctx, a: GatewayArgs) -> Result<()> {
    let store = ctx.store()?;
    if let Some(GatewayCmd::Token { rotate }) = a.cmd {
        let (t, new) = gateway_token(&store, rotate)?;
        if new && rotate {
            ui::status("new gateway token (restart a running gateway to apply it):");
        }
        crate::out!("{t}");
        return Ok(());
    }
    if !a.listen.ip().is_loopback() && !a.allow_non_loopback {
        bail!(
            "refusing to serve decrypted notes on {} (not a loopback address). Use --allow-non-loopback only behind \
             a TLS proxy you control",
            a.listen
        );
    }
    let (token, created) = gateway_token(&store, false)?;
    if created {
        ui::status("created a gateway token; show it with `notekpr gateway token`");
    }
    let session = ctx.session()?;
    let audit = Arc::new(AuditLog::open(&session.data_dir()));
    let hub = Hub::start(
        session,
        HubOptions {
            vaults: a.vaults.clone(),
            read_only: a.read_only,
            offline_ok: true,
            catch_up: true,
            detached: false,
            quick_notes: false,
        },
    )
    .await?;
    let _control = serve_control(ctx, &hub, Some(audit.clone())).await;
    let (stop_tx, stop_rx) = watch::channel(false);
    // If live sync ends on its own (session revoked), the gateway stops too
    // instead of silently serving data that is no longer kept up to date.
    let (ended_tx, mut ended_rx) = watch::channel(false);
    let live = tokio::spawn({
        let hub = hub.clone();
        async move {
            let r = livesync::run(hub, stop_rx).await;
            let _ = ended_tx.send(true);
            r
        }
    });
    let listener = tokio::net::TcpListener::bind(a.listen)
        .await
        .with_context(|| format!("binding {}", a.listen))?;
    ui::status(&format!(
        "gateway listening on http://{} — {} vault(s){}; audit log: {}",
        listener.local_addr()?,
        hub.vault_ids().len(),
        if a.read_only { ", read-only" } else { "" },
        audit.path().display()
    ));
    let gw = Arc::new(Gateway {
        hub: hub.clone(),
        token,
        audit,
    });
    let stopped = async move {
        tokio::select! {
            _ = shutdown_signal() => {}
            _ = ended_rx.changed() => {}
        }
    };
    gateway::serve(listener, gw, stopped).await?;
    let _ = stop_tx.send(true);
    let live_result = match tokio::time::timeout(Duration::from_secs(5), live).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => Err(anyhow!("the sync task failed: {e}")),
        Err(_) => Ok(()),
    };
    if let Err(e) = &live_result {
        ui::warn(&format!(
            "the gateway stopped because live sync ended: {e:#}"
        ));
    }
    for id in hub.vault_ids() {
        let _ = tokio::time::timeout(Duration::from_secs(10), hub.push(&id)).await;
    }
    // The editing session ends with the process: keep a revision of what changed.
    hub.snapshot_idle(Duration::ZERO, livesync::SESSION_MIN_UPDATES)
        .await;
    hub.save_all().await;
    live_result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forever() -> Task {
        tokio::spawn(async {
            std::future::pending::<()>().await;
            Ok(())
        })
    }

    #[tokio::test]
    async fn a_failing_mirror_task_stops_the_daemon_with_its_reason() {
        let mirror: Task = tokio::spawn(async { Err(anyhow!("inotify watch limit reached")) });
        let (r, live, mirror) = supervise(std::future::pending(), forever(), Some(mirror)).await;
        let e = format!("{:#}", r.unwrap_err());
        assert!(e.contains("mirror") && e.contains("inotify"), "{e}");
        assert!(mirror.is_none(), "a finished task is not awaited again");
        live.unwrap().abort();

        // A mirror that just ends is an error too; a stop signal is not.
        let mirror: Task = tokio::spawn(async { Ok(()) });
        let (r, live, _) = supervise(std::future::pending(), forever(), Some(mirror)).await;
        assert!(r.is_err());
        live.unwrap().abort();
        let (r, live, mirror) = supervise(async {}, forever(), Some(forever())).await;
        assert!(r.is_ok());
        live.unwrap().abort();
        mirror.unwrap().abort();
        // The live loop failing (session revoked) is reported as such.
        let live: Task = tokio::spawn(async { Err(anyhow!("session revoked")) });
        let (r, live, _) = supervise(std::future::pending(), live, None).await;
        assert!(format!("{:#}", r.unwrap_err()).contains("revoked"));
        assert!(live.is_none());
    }
}
