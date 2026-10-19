//! Command-line interface definition (clap).

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "coolpsonotes",
    version,
    about = "End-to-end encrypted, local-first Markdown notes: desktop sync, file mirror and agent gateway",
    after_help = "Use several profiles side by side, e.g. `--profile alice` and `--profile bob`.\n\
                  Documentation: docs/desktop-client.md and docs/agents.md"
)]
pub struct Cli {
    /// Profile to use (one signed-in identity on one server).
    #[arg(
        long,
        short = 'p',
        global = true,
        env = "COOLPSONOTES_PROFILE",
        default_value = "default"
    )]
    pub profile: String,
    /// Only print results and warnings.
    #[arg(long, short = 'q', global = true)]
    pub quiet: bool,
    /// Machine-readable JSON output (for listing commands).
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Sign in and set up this device (OIDC device login, or --dev-user in development mode).
    Login(LoginArgs),
    /// End the session (with --forget: also remove this device and all local data).
    Logout {
        #[arg(long)]
        forget: bool,
        #[arg(long)]
        yes: bool,
        /// With --forget: also delete this profile's decrypted Markdown mirror
        /// folders (asked interactively otherwise).
        #[arg(long, requires = "forget")]
        remove_mirror: bool,
    },
    /// Show profile, account, devices, vaults and sync state.
    Status,
    /// List local profiles.
    Profiles,
    /// Manage this account's devices.
    #[command(subcommand)]
    Devices(DevicesCmd),
    /// Recovery key management.
    #[command(subcommand)]
    Recovery(RecoveryCmd),
    /// Create, share and manage vaults.
    #[command(subcommand)]
    Vault(VaultCmd),
    /// List documents and folders.
    Ls {
        vault: String,
        folder: Option<String>,
        #[arg(long, short = 'r')]
        recursive: bool,
    },
    /// Print a note (or an attachment's bytes).
    Cat { vault: String, path: String },
    /// Create or update a note from stdin (diff-merged, never a blind overwrite).
    Write {
        vault: String,
        path: String,
        /// Store stdin as an attachment instead of a note.
        #[arg(long)]
        attachment: bool,
        /// Append to the note instead of replacing its content.
        #[arg(long, conflicts_with = "attachment")]
        append: bool,
        /// Only write if the note still has this content hash (from `cat --json`/the gateway).
        #[arg(long)]
        if_match: Option<String>,
    },
    /// Create a folder.
    Mkdir { vault: String, path: String },
    /// Move or rename a document or folder.
    Mv {
        vault: String,
        from: String,
        to: String,
        /// Rewrite [[wiki links]] in other notes that pointed at the moved note.
        #[arg(long)]
        update_links: bool,
    },
    /// Move a document or folder to the trash.
    Rm { vault: String, path: String },
    /// Inspect, restore or empty the trash.
    #[command(subcommand)]
    Trash(TrashCmd),
    /// Full-text search (tag:x, folder:x, title:x, vault:x, "phrases").
    Search {
        query: Vec<String>,
        #[arg(long)]
        vault: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Notes linking to a document.
    Backlinks { vault: String, path: String },
    /// Revision history of a note.
    History(HistoryArgs),
    /// Export a vault as plain Markdown files and attachments (directory or .zip).
    Export {
        vault: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// Save an encrypted export of everything this account can read (a backup
    /// that `recover` can turn into Markdown files without any server).
    ExportEncrypted {
        #[arg(long)]
        out: PathBuf,
        /// Replace an existing file.
        #[arg(long)]
        force: bool,
    },
    /// Decrypt an encrypted export into plain Markdown files, offline: with
    /// --from and your recovery key, or (without --from) with this signed-in device.
    Recover(RecoverArgs),
    /// Import Markdown files and attachments (directory or .zip), keeping folders.
    Import {
        vault: String,
        source: PathBuf,
        /// Put everything below this folder of the vault.
        #[arg(long)]
        into: Option<String>,
        /// Leave notes that already exist untouched (default: merge the file's content).
        #[arg(long)]
        skip_existing: bool,
        /// Only report what would be imported; change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Share a single note (not its vault) with someone.
    ShareNote {
        vault: String,
        path: String,
        /// User id, exact e-mail or display name.
        user: String,
        #[arg(long, value_enum, default_value_t = ShareRole::Editor)]
        role: ShareRole,
        /// Accept a changed account key after verifying it out of band.
        #[arg(long)]
        accept_new_key: bool,
    },
    /// Stop sharing a note with someone (rotates the note's key).
    UnshareNote {
        vault: String,
        path: String,
        user: String,
    },
    /// Who a note is shared with individually.
    NoteShares { vault: String, path: String },
    /// Notes other people shared with you individually.
    #[command(subcommand)]
    Shared(SharedCmd),
    /// Keep the local replicas in sync; with --dir also mirror decrypted Markdown files.
    Sync(SyncArgs),
    /// Agent identities (setup on the agent's machine, or management by the owner).
    #[command(subcommand)]
    Agent(AgentCmd),
    /// Local plaintext HTTP API for agents and scripts.
    Gateway(GatewayArgs),
}

#[derive(Args, Debug)]
pub struct RecoverArgs {
    /// Encrypted export to decrypt (from `export-encrypted`, `GET /api/v1/me/export`
    /// or `coolpsonotes-server export-user`). Without it, this device downloads one.
    #[arg(long)]
    pub from: Option<PathBuf>,
    /// New (or empty) folder for the recovered files.
    #[arg(long)]
    pub out: PathBuf,
    /// Read the recovery key from this file (otherwise from
    /// COOLPSONOTES_RECOVERY_KEY, or asked for).
    #[arg(long)]
    pub recovery_key_file: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct LoginArgs {
    /// Server URL, e.g. https://notes.example.com
    #[arg(long)]
    pub server: Option<String>,
    /// DEVELOPMENT servers only: sign in as this user name without OIDC.
    #[arg(long)]
    pub dev_user: Option<String>,
    /// Name shown in your device list.
    #[arg(long)]
    pub device_name: Option<String>,
    /// Unlock this device with your recovery key instead of approving it from another device.
    #[arg(long, env = "COOLPSONOTES_RECOVERY_KEY", hide_env_values = true)]
    pub recovery_key: Option<String>,
    /// Allow plain http:// to a non-local server (insecure).
    #[arg(long)]
    pub insecure_http: bool,
}

#[derive(Subcommand, Debug)]
pub enum DevicesCmd {
    /// List devices.
    List,
    /// Approve a pending device after comparing its verification code.
    Approve {
        id: String,
        #[arg(long)]
        yes: bool,
    },
    /// Remove a device (ends its sessions). Rotate shared vault keys if it was compromised.
    Revoke {
        id: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum RecoveryCmd {
    /// Replace the recovery key (the old one stops working).
    Rotate {
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum SharedCmd {
    /// List notes shared with you.
    List,
    /// Print a shared note (by title or id).
    Cat { note: String },
    /// Replace a shared note's text with stdin (merged, never a blind overwrite).
    Write {
        note: String,
        /// Append stdin instead of replacing the text.
        #[arg(long)]
        append: bool,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum ShareRole {
    Editor,
    Viewer,
}

#[derive(Subcommand, Debug)]
pub enum VaultCmd {
    /// List vaults.
    List,
    /// Create a vault.
    Create { name: String },
    /// Rename a vault (owner).
    Rename { vault: String, name: String },
    /// Archive (or with --undo, unarchive) a vault (owner).
    Archive {
        vault: String,
        #[arg(long)]
        undo: bool,
    },
    /// Delete a vault for everyone (owner). Cannot be undone.
    Delete {
        vault: String,
        #[arg(long)]
        yes: bool,
    },
    /// List members with their key fingerprints.
    Members { vault: String },
    /// Share with a person or one of your agents (verifies and pins their key).
    Share {
        vault: String,
        /// User id, exact e-mail or display name.
        user: String,
        #[arg(long, value_enum, default_value_t = ShareRole::Editor)]
        role: ShareRole,
        /// Accept a changed account key after verifying it out of band.
        #[arg(long)]
        accept_new_key: bool,
    },
    /// Remove a member and rotate the vault key.
    Unshare { vault: String, user: String },
    /// Rotate the vault key (owner).
    Rotate { vault: String },
    /// Leave a vault shared with you (removes the local copy).
    Leave {
        vault: String,
        #[arg(long)]
        yes: bool,
        /// Also delete the vault's decrypted Markdown mirror folder (asked
        /// interactively otherwise).
        #[arg(long)]
        remove_mirror: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum TrashCmd {
    /// List trashed entries.
    List { vault: String },
    /// Restore an entry by path or id.
    Restore { vault: String, entry: String },
    /// Permanently delete everything in the trash.
    Empty {
        vault: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub struct HistoryArgs {
    #[command(subcommand)]
    pub cmd: Option<HistoryCmd>,
    pub vault: Option<String>,
    pub path: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum HistoryCmd {
    /// Print one revision.
    Show {
        vault: String,
        path: String,
        revision: String,
    },
    /// Make a revision the current content (merged as an edit).
    Restore {
        vault: String,
        path: String,
        revision: String,
    },
}

#[derive(Args, Debug)]
pub struct SyncArgs {
    /// Mirror decrypted Markdown files into this directory (plaintext mode).
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// Only these vaults (id or name; repeatable).
    #[arg(long = "vault")]
    pub vaults: Vec<String>,
    /// Catch up, ingest disk changes, push, then exit.
    #[arg(long)]
    pub once: bool,
    /// Confirm that writing decrypted files to --dir is intended.
    #[arg(long)]
    pub plaintext_ok: bool,
    /// Stop mirroring to the remembered directory (keeps the files).
    #[arg(long, conflicts_with = "dir")]
    pub no_dir: bool,
    /// Move mirrored files that disappeared to the trash even when many (or a
    /// whole vault folder) vanished at once; normally that is held for safety.
    #[arg(long, conflicts_with = "restore_missing")]
    pub allow_mass_delete: bool,
    /// Write mirrored files that disappeared in bulk (or a missing mirror
    /// directory) again from the local copies instead of holding.
    #[arg(long)]
    pub restore_missing: bool,
}

#[derive(Subcommand, Debug)]
pub enum AgentCmd {
    /// Configure this profile as an agent identity and create its key pair.
    Setup {
        #[arg(long)]
        server: String,
        #[arg(long)]
        client_id: String,
        /// File containing the client secret (never pass secrets on the command line).
        #[arg(long)]
        client_secret_file: PathBuf,
        #[arg(long)]
        insecure_http: bool,
    },
    /// Create an agent you own; its secret is written to --secret-out (mode 0600).
    Create {
        name: String,
        #[arg(long)]
        secret_out: PathBuf,
    },
    /// List your agents.
    List,
    /// Show an agent's key fingerprint (compare with `agent setup` output before sharing).
    Fingerprint { agent: String },
    /// Issue a new client secret (and revoke the old ones unless --keep-old).
    RotateSecret {
        agent: String,
        #[arg(long)]
        secret_out: PathBuf,
        #[arg(long)]
        keep_old: bool,
    },
    /// Delete an agent, its memberships and sessions.
    Delete {
        agent: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub struct GatewayArgs {
    #[command(subcommand)]
    pub cmd: Option<GatewayCmd>,
    /// Address to listen on (loopback by default).
    #[arg(long, default_value = "127.0.0.1:7878")]
    pub listen: SocketAddr,
    /// Refuse all writes.
    #[arg(long)]
    pub read_only: bool,
    /// Only expose these vaults (id or name; repeatable).
    #[arg(long = "vault")]
    pub vaults: Vec<String>,
    /// Allow listening on a non-loopback address (plaintext over the network!).
    #[arg(long)]
    pub allow_non_loopback: bool,
}

#[derive(Subcommand, Debug)]
pub enum GatewayCmd {
    /// Print the gateway's bearer token (created on first use).
    Token {
        /// Replace it with a new one (the old one stops working).
        #[arg(long)]
        rotate: bool,
    },
}
