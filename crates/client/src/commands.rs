//! Implementations of the CLI commands (thin wrappers over the library).

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use coolpsonotes_core::doc::EntryKind;
use coolpsonotes_core::engine::PinStatus;
use coolpsonotes_core::protocol::{DeviceStatus, PrincipalKind, Role};
use serde::Serialize;
use serde_json::json;
use tokio::sync::watch;

use crate::account::{self, AgentSetupOptions, LoginOptions};
use crate::audit::AuditLog;
use crate::cli::*;
use crate::gateway::{self, Gateway};
use crate::hub::{Hub, HubOptions};
use crate::manage;
use crate::mirror::Mirror;
use crate::note_shares;
use crate::ops;
use crate::profile::{Dirs, ProfileStore};
use crate::session::Session;
use crate::vault::ListKind;
use crate::{livesync, transfer, ui};

pub struct Ctx {
    pub dirs: Dirs,
    pub profile: String,
    pub json: bool,
}

impl Ctx {
    fn store(&self) -> Result<ProfileStore> {
        ProfileStore::new(&self.dirs, &self.profile)
    }

    fn session(&self) -> Result<Arc<Session>> {
        Ok(Arc::new(Session::open(&self.dirs, &self.profile)?))
    }

    /// A hub for one-shot commands: catches up and pushes the given vaults.
    async fn hub(&self, vaults: &[&str]) -> Result<Arc<Hub>> {
        let opts = HubOptions {
            vaults: vaults.iter().map(|s| s.to_string()).collect(),
            read_only: false,
            offline_ok: true,
            catch_up: true,
        };
        Hub::start(self.session()?, opts).await
    }

    fn print_json(&self, v: &impl Serialize) {
        println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
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
    ui::set_quiet(cli.quiet);
    let ctx = Ctx {
        dirs: Dirs::from_env()?,
        profile: cli.profile.clone(),
        json: cli.json,
    };
    match cli.command {
        Command::Login(a) => login(&ctx, a).await,
        Command::Logout { forget, yes } => {
            if forget {
                confirm_or_flag(
                    yes,
                    "Remove this device from your account and delete all local data of this profile?",
                )?;
            }
            account::logout(&ctx.dirs, &ctx.profile, forget).await?;
            println!(
                "{}",
                if forget {
                    "Signed out; this device and its local data were removed."
                } else {
                    "Signed out."
                }
            );
            Ok(())
        }
        Command::Status => status(&ctx).await,
        Command::Profiles => {
            for p in ctx.dirs.profiles() {
                let store = ProfileStore::new(&ctx.dirs, &p)?;
                match store.load() {
                    Ok(Some(pr)) => println!(
                        "{p:<16} {:<6} {:<24} {}",
                        pr.kind.as_str(),
                        pr.display_name.unwrap_or_default(),
                        pr.server
                    ),
                    _ => println!("{p:<16} (unreadable)"),
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
        } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let role = match role {
                ShareRole::Editor => Role::Editor,
                ShareRole::Viewer => Role::Viewer,
            };
            let out = note_shares::share_note(&hub.session, &v, &path, &user, role, accept_new_key)
                .await?;
            if out.updated_role {
                println!(
                    "'{path}' is now shared with {} as {}",
                    out.user.display_name,
                    role.as_str()
                );
            } else {
                println!(
                    "shared '{path}' with {} as {} (they cannot see anything else in the vault)",
                    out.user.display_name,
                    role.as_str()
                );
            }
            match (&out.fingerprint, out.pin) {
                (Some(fp), Some(PinStatus::New)) => println!(
                    "Their key fingerprint: {fp} (now pinned; compare it with them over a trusted channel)"
                ),
                (Some(fp), _) => println!("Their key fingerprint: {fp} (matches the pinned one)"),
                _ => {}
            }
            Ok(())
        }
        Command::UnshareNote { vault, path, user } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let u = note_shares::unshare_note(&hub.session, &v, &path, &user).await?;
            println!(
                "stopped sharing '{path}' with {}; the note's key was rotated",
                u.display_name
            );
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
                println!("'{path}' is not shared individually");
                return Ok(());
            }
            let list = note_shares::note_shares(&hub.session, &vid, &nid).await?;
            if ctx.json {
                ctx.print_json(&list.iter().map(|s| json!({ "user_id": s.user.id, "name": s.user.display_name, "role": s.role })).collect::<Vec<_>>());
            } else {
                for s in list {
                    println!(
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
                        ctx.print_json(&list.iter().map(|n| json!({ "id": n.info.object_id, "vault_id": n.info.vault_id, "title": n.title, "role": n.info.role, "owner": n.info.owner.display_name })).collect::<Vec<_>>());
                    } else if list.is_empty() {
                        println!("no notes are shared with you individually");
                    } else {
                        for n in list {
                            println!(
                                "{:<34} {:<6} {:<24} {}",
                                n.info.object_id,
                                n.info.role.as_str(),
                                n.info.owner.display_name,
                                n.title
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
            update_links,
        } => {
            let hub = ctx.hub(&[&vault]).await?;
            let out = op(ops::move_document(&hub, &vault, &from, &to, update_links).await)?;
            println!("moved to {}", out.to);
            Ok(())
        }
        Command::Rm { vault, path } => {
            let hub = ctx.hub(&[&vault]).await?;
            let (_, ids) = op(ops::delete_document(&hub, &vault, &path).await)?;
            println!(
                "moved {} item(s) to the trash (restore with `coolpsonotes trash restore`)",
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
                println!("no matches");
            } else {
                for h in hits {
                    println!(
                        "{}:{}  ({:.1})\n    {}",
                        h.vault_name, h.path, h.score, h.snippet
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
                    println!("{}", d.path);
                }
            }
            Ok(())
        }
        Command::History(h) => history(&ctx, h).await,
        Command::Export { vault, out, force } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let r = transfer::export(&hub, &v, &out, force).await?;
            print_transfer("exported", &r);
            Ok(())
        }
        Command::Import {
            vault,
            source,
            into,
            skip_existing,
        } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let r = transfer::import(&hub, &v, &source, into.as_deref(), skip_existing).await?;
            print_transfer("imported", &r);
            Ok(())
        }
        Command::Sync(a) => sync(&ctx, a).await,
        Command::Agent(c) => agent(&ctx, c).await,
        Command::Gateway(a) => gateway_cmd(&ctx, a).await,
    }
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
    if o.offline || o.remaining > 0 {
        ui::status(&format!(
            "saved locally; {} change(s) will be uploaded when the server is reachable",
            o.remaining
        ));
    }
}

fn print_transfer(verb: &str, r: &transfer::TransferReport) {
    println!(
        "{verb} {} note(s), {} attachment(s), {} folder(s){}",
        r.notes,
        r.attachments,
        r.folders,
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
            recovery_key: a.recovery_key,
            interactive: true,
            approval_timeout: None,
        },
    )
    .await?;
    if let Some(rk) = &out.recovery_key {
        show_recovery_key(rk);
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
    println!(
        "Signed in as {} (profile '{}'): {how}.",
        out.user.display_name, ctx.profile
    );
    println!("Account key fingerprint: {}", out.fingerprint);
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
    println!("Profile:   {} ({})", ctx.profile, p.kind.as_str());
    println!("Server:    {}", p.server);
    println!(
        "User:      {} ({})",
        p.display_name.clone().unwrap_or_default(),
        p.user_id.clone().unwrap_or_default()
    );
    if p.device_pending {
        println!("Device:    waiting for approval — run `coolpsonotes login` again to continue");
        return Ok(());
    }
    let session = ctx.session()?;
    println!("Key:       {}", session.fingerprint());
    match (&p.session_token, p.kind) {
        (None, PrincipalKind::Human) => {
            println!("Session:   signed out (local copies only) — run `coolpsonotes login`")
        }
        (_, PrincipalKind::Agent) => println!(
            "Session:   agent client credentials ({})",
            p.agent.as_ref().map(|a| a.client_id.as_str()).unwrap_or("")
        ),
        (Some(_), _) => {
            if let Some(exp) = p.session_expires_at {
                println!("Session:   valid until {}", ui::fmt_time(exp));
            }
        }
    }
    let hub = Hub::start(
        session.clone(),
        HubOptions {
            offline_ok: true,
            ..Default::default()
        },
    )
    .await?;
    match session.server_info().await {
        Ok(i) => println!("Online:    yes ({} {})", i.name, i.version),
        Err(e) => println!("Online:    no ({e})"),
    }
    if p.kind == PrincipalKind::Human
        && hub.is_online()
        && let Ok(devs) = manage::devices(&session).await
    {
        println!("Devices:");
        for d in devs {
            let cur = if Some(&d.id) == p.device_id.as_ref() {
                " (this device)"
            } else {
                ""
            };
            println!("  {:<10} {}{cur}  [{}]", d.status.as_str(), d.name, d.id);
        }
    }
    println!("Vaults:");
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
        println!(
            "  {:<24} {:<7} {:>4} docs  {:>3} pending  synced {last}{}{ro}  [{}]",
            s.name,
            s.role.as_str(),
            s.documents,
            s.pending_changes,
            if s.archived { " (archived)" } else { "" },
            s.id
        );
    }
    for id in hub.waiting_for_keys() {
        println!("  (shared with you, waiting for its key)  [{id}]");
    }
    match &p.sync.mirror_dir {
        Some(d) => println!("Mirror:    {} (plaintext files)", d.display()),
        None => println!("Mirror:    off (encrypted local copies only)"),
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
                println!(
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
                println!(
                    "\n{} device(s) waiting: `coolpsonotes devices approve <id>`",
                    pending.len()
                );
            }
            Ok(())
        }
        DevicesCmd::Approve { id, yes } => {
            let (d, code) = manage::pending_device(&s, &id).await?;
            println!("Device:            {}", d.name);
            println!("Verification code: {code}");
            confirm_or_flag(yes, "Does the new device show exactly this code?")?;
            manage::approve_device(&s, &d).await?;
            println!("Approved '{}'.", d.name);
            Ok(())
        }
        DevicesCmd::Revoke { id, yes } => {
            if Some(&id) == s.profile.device_id.as_ref() {
                ui::warn("this is the device you are using; it will be signed out");
            }
            confirm_or_flag(yes, &format!("Remove device {id} from your account?"))?;
            manage::revoke_device(&s, &id).await?;
            println!(
                "Device removed. If it may be compromised, rotate the keys of your shared vaults (`vault rotate`)."
            );
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- vaults

async fn vault(ctx: &Ctx, c: VaultCmd) -> Result<()> {
    match c {
        VaultCmd::List => {
            let hub = ctx.hub(&[]).await?;
            let list = hub.summaries().await;
            if ctx.json {
                ctx.print_json(&list);
                return Ok(());
            }
            if list.is_empty() {
                println!("no vaults yet — create one with `coolpsonotes vault create <name>`");
            }
            for s in list {
                println!(
                    "{:<28} {:<7} {:>4} docs{}{}  [{}]",
                    s.name,
                    s.role.as_str(),
                    s.documents,
                    if s.archived { "  (archived)" } else { "" },
                    s.read_only
                        .map(|r| format!("  READ-ONLY: {r}"))
                        .unwrap_or_default(),
                    s.id
                );
            }
            for id in hub.waiting_for_keys() {
                println!("{:<28} (waiting for its key from the owner)  [{id}]", "?");
            }
            Ok(())
        }
        VaultCmd::Create { name } => {
            let s = ctx.session()?;
            let v = manage::create_vault(&s, &name).await?;
            println!("created vault '{}' [{}]", v.name(), v.id());
            Ok(())
        }
        VaultCmd::Rename { vault, name } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            manage::rename_vault(&hub.session, &v, &name).await?;
            println!("renamed to '{name}'");
            Ok(())
        }
        VaultCmd::Archive { vault, undo } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            manage::set_archived(&hub.session, &v, !undo).await?;
            println!("{}", if undo { "unarchived" } else { "archived" });
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
            println!("deleted '{name}'");
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
                let key = if m.member.has_current_key {
                    ""
                } else {
                    "  [waiting for vault key]"
                };
                println!(
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
                ShareRole::Editor => Role::Editor,
                ShareRole::Viewer => Role::Viewer,
            };
            let out = manage::share(&hub.session, &v, &user, role, accept_new_key).await?;
            let who = format!("{} ({})", out.user.display_name, out.user.kind.as_str());
            if out.updated_role {
                println!("{who} is now {}", role.as_str());
            } else {
                println!("shared with {who} as {}", role.as_str());
            }
            match (&out.fingerprint, out.pin) {
                (Some(fp), Some(PinStatus::New)) => println!(
                    "Their key fingerprint: {fp}\nCompare it with them (or with `agent setup` output for an agent) over a \
                     trusted channel. It is now pinned; you will be warned if it ever changes."
                ),
                (Some(fp), _) => println!("Their key fingerprint: {fp} (matches the pinned one)"),
                (None, _) => println!(
                    "{} has not set up encryption yet; the vault key is shared automatically by your client \
                     (run `coolpsonotes sync` or `vault members`) once they have.",
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
            println!(
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
            println!("vault key rotated (epoch {epoch})");
            Ok(())
        }
        VaultCmd::Leave { vault, yes } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let name = v.lock().await.name().to_string();
            confirm_or_flag(yes, &format!("Leave '{name}' and delete its local copy?"))?;
            manage::leave_vault(&hub.session, &v).await?;
            println!("left '{name}'");
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
    for l in list {
        match (l.kind, &l.entry) {
            (ListKind::Folder, _) => println!("{:>10}  {:<20}  {}/", "", "", l.path),
            (_, Some(e)) => {
                let size = if e.kind == EntryKind::Note {
                    g.note_text(&e.id).len() as u64
                } else {
                    e.size.unwrap_or(0)
                };
                println!(
                    "{:>10}  {:<20}  {}",
                    ui::fmt_size(size),
                    ui::fmt_time(e.modified),
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
    let mut out = std::io::stdout().lock();
    match e.kind {
        EntryKind::Note => {
            let doc = op(ops::read_document(&hub, vault, path).await)?;
            if ctx.json {
                ctx.print_json(&doc);
            } else {
                out.write_all(doc.content.as_bytes())?;
            }
        }
        EntryKind::Attachment => {
            let data = hub.session.attachment(&v, &e.id).await?;
            out.write_all(&data)?;
        }
        EntryKind::Folder => bail!("'{path}' is a folder; use `ls`"),
    }
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
    let mut data = Vec::new();
    std::io::stdin()
        .read_to_end(&mut data)
        .context("reading stdin")?;
    let hub = ctx.hub(&[vault]).await?;
    let out = if attachment {
        op(ops::put_attachment(&hub, vault, path, &data, None).await)?
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
    println!("{what} {}", out.path);
    if out.pending_changes > 0 {
        ui::status(&format!(
            "saved locally; {} change(s) will be uploaded when the server is reachable",
            out.pending_changes
        ));
    }
    Ok(())
}

async fn trash(ctx: &Ctx, c: TrashCmd) -> Result<()> {
    match c {
        TrashCmd::List { vault } => {
            let hub = ctx.hub(&[&vault]).await?;
            let v = hub.resolve(&vault).await?;
            let list = v.lock().await.trashed();
            if ctx.json {
                ctx.print_json(&list);
                return Ok(());
            }
            if list.is_empty() {
                println!("the trash is empty");
            }
            for e in list {
                println!(
                    "{}  {:<10}  {}  [{}]",
                    ui::fmt_time(e.trashed.unwrap_or(0)),
                    e.kind.as_str(),
                    e.path,
                    e.id
                );
            }
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
            println!("restored {}", e.path);
            Ok(())
        }
        TrashCmd::Empty { vault, yes } => {
            let hub = ctx.hub(&[&vault]).await?;
            confirm_or_flag(yes, "Permanently delete everything in the trash?")?;
            let n = op(ops::empty_trash(&hub, &vault).await)?;
            println!("permanently deleted {n} item(s)");
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
            print!("{text}");
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
            println!("{} restored to revision {revision}", out.path);
            Ok(())
        }
        None => {
            let (Some(vault), Some(path)) = (h.vault, h.path) else {
                bail!(
                    "usage: coolpsonotes history <vault> <path>  |  history show <vault> <path> <revision>"
                );
            };
            let hub = ctx.hub(&[&vault]).await?;
            let revs = op(ops::history(&hub, &vault, &path).await)?;
            if ctx.json {
                ctx.print_json(&revs);
                return Ok(());
            }
            if revs.is_empty() {
                println!(
                    "no stored revisions yet (they are created as a document accumulates edits)"
                );
            }
            for r in revs {
                println!(
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

async fn sync(ctx: &Ctx, a: SyncArgs) -> Result<()> {
    let store = ctx.store()?;
    let profile = store.require()?;
    if a.no_dir {
        store.update(|p| p.sync.mirror_dir = None)?;
        println!("mirroring disabled (existing files are left in place)");
        return Ok(());
    }
    let dir = match a.dir.clone().or(profile.sync.mirror_dir.clone()) {
        Some(d) => {
            Some(std::path::absolute(&d).with_context(|| format!("resolving {}", d.display()))?)
        }
        None => None,
    };
    if let Some(d) = &dir {
        if !profile.sync.plaintext_ok && !a.plaintext_ok {
            eprintln!("{}\n", plaintext_note(d));
            if !ui::stdin_is_tty() {
                bail!("writing decrypted files must be confirmed once: re-run with --plaintext-ok");
            }
            let answer = ui::prompt_line("Type 'yes' to enable the plaintext mirror: ")?;
            if answer != "yes" {
                bail!(
                    "cancelled; run `coolpsonotes sync` without --dir to keep only encrypted local copies"
                );
            }
        } else if a.dir.is_some() || !profile.sync.plaintext_ok {
            ui::status(&plaintext_note(d));
        }
        store.update(|p| {
            p.sync.plaintext_ok = true;
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
        },
    )
    .await?;
    if a.once {
        if let Some(d) = &dir {
            let mut m = Mirror::new(hub.clone(), d).await?;
            let r = m.reconcile_all(true).await?;
            ui::status(&format!(
                "mirror: {} edit(s) and {} new file(s) uploaded, {} moved, {} trashed, {} file(s) written, {} conflict copies",
                r.ingested, r.created, r.renamed, r.trashed, r.written, r.conflicts
            ));
        }
        let mut pending = 0;
        for id in hub.vault_ids() {
            match hub.sync_vault(&id).await {
                Ok(o) => pending += o.remaining,
                Err(e) => ui::warn(&format!("vault {id}: {e:#}")),
            }
        }
        hub.snapshot_idle(Duration::ZERO, livesync::SESSION_MIN_UPDATES)
            .await;
        hub.save_all().await;
        let n = hub.vault_ids().len();
        if hub.is_online() {
            println!(
                "synced {n} vault(s){}",
                if pending > 0 {
                    format!("; {pending} change(s) still queued")
                } else {
                    String::new()
                }
            );
        } else {
            println!(
                "offline: {n} local vault(s) up to date with your edits; {pending} change(s) queued for upload"
            );
        }
        return Ok(());
    }

    let (stop_tx, stop_rx) = watch::channel(false);
    let live = tokio::spawn(livesync::run(hub.clone(), stop_rx.clone()));
    let mirror_task = match &dir {
        Some(d) => {
            let mut m = Mirror::new(hub.clone(), d).await?;
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
    let mut live = live;
    let result = tokio::select! {
        _ = shutdown_signal() => Ok(()),
        r = &mut live => match r {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(anyhow!("sync task failed: {e}")),
        },
    };
    let _ = stop_tx.send(true);
    if let Some(t) = mirror_task {
        let _ = tokio::time::timeout(Duration::from_secs(5), t).await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(5), live).await;
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

async fn resolve_agent(
    s: &Session,
    reference: &str,
) -> Result<coolpsonotes_core::protocol::AgentInfo> {
    let list = manage::agents(s).await?;
    let mut found: Vec<_> = list
        .into_iter()
        .filter(|a| a.id == reference || a.display_name.eq_ignore_ascii_case(reference))
        .collect();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => bail!("no agent '{reference}' (see `coolpsonotes agent list`)"),
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
            println!(
                "Agent '{}' is set up in profile '{}'{}.",
                out.display_name,
                ctx.profile,
                if out.created_keys {
                    " (new key pair created)"
                } else {
                    ""
                }
            );
            println!("Agent key fingerprint: {}", out.fingerprint);
            println!(
                "Ask the owner to check this fingerprint before sharing a vault with the agent\n\
                 (`coolpsonotes vault share <vault> \"{}\"` shows the same value).",
                out.display_name
            );
            Ok(())
        }
        AgentCmd::Create { name, secret_out } => {
            let s = ctx.session()?;
            let created = manage::create_agent(&s, &name).await?;
            write_secret_file(&secret_out, &created.credential.client_secret)?;
            println!(
                "created agent '{}' [{}]",
                created.agent.display_name, created.agent.id
            );
            println!("client id:     {}", created.credential.client_id);
            println!(
                "client secret: written to {} (mode 0600)",
                secret_out.display()
            );
            println!(
                "On the agent's machine run:\n  coolpsonotes --profile <name> agent setup --server {} --client-id {} \
                 --client-secret-file <file>",
                s.profile.server, created.credential.client_id
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
                println!(
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
                Some(fp) => println!("{fp}"),
                None => println!("'{}' has not run `agent setup` yet", a.display_name),
            }
            Ok(())
        }
        AgentCmd::RotateSecret {
            agent,
            secret_out,
            keep_old,
        } => {
            let s = ctx.session()?;
            let a = resolve_agent(&s, &agent).await?;
            if secret_out.exists() {
                bail!(
                    "{} already exists; choose another --secret-out",
                    secret_out.display()
                );
            }
            let c = manage::rotate_agent_secret(&s, &a.id, !keep_old).await?;
            write_secret_file(&secret_out, &c.client_secret)?;
            println!(
                "new client id: {}\nclient secret written to {}",
                c.client_id,
                secret_out.display()
            );
            if !keep_old {
                println!(
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
            println!(
                "deleted '{}'. Rotate the keys of vaults it could read (`vault rotate`) if its machine may be compromised.",
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
        println!("{t}");
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
        ui::status("created a gateway token; show it with `coolpsonotes gateway token`");
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
        },
    )
    .await?;
    let (stop_tx, stop_rx) = watch::channel(false);
    let live = tokio::spawn(livesync::run(hub.clone(), stop_rx));
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
    gateway::serve(listener, gw, shutdown_signal()).await?;
    let _ = stop_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), live).await;
    for id in hub.vault_ids() {
        let _ = tokio::time::timeout(Duration::from_secs(10), hub.push(&id)).await;
    }
    // The editing session ends with the process: keep a revision of what changed.
    hub.snapshot_idle(Duration::ZERO, livesync::SESSION_MIN_UPDATES)
        .await;
    hub.save_all().await;
    Ok(())
}
