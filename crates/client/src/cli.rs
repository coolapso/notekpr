//! Command-line interface definition (clap).

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

#[derive(Parser, Clone, Debug, Serialize, Deserialize)]
#[command(
    name = "notekpr",
    version = notekpr_core::VERSION,
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
        env = "NOTEKPR_PROFILE",
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

#[derive(Subcommand, Clone, Debug, Serialize, Deserialize)]
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
    /// Delete your account on the server for good — the vaults you own go,
    /// also for the people you share them with; you leave the others — and
    /// this profile with its local data.
    DeleteAccount {
        #[arg(long)]
        yes: bool,
        /// Also delete this profile's decrypted Markdown mirror folders
        /// (asked interactively otherwise).
        #[arg(long)]
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
    /// Create or update a note from stdin (diff-merged, never a blind overwrite); a path
    /// ending in `.drawing` takes a drawing file (ours, Excalidraw's or Obsidian's) instead.
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
    /// Capture a quick note (TEXT, or stdin); --list shows them, --move takes
    /// one to a vault, --delete deletes one for good.
    Quick(QuickArgs),
    /// Your saved shapes: pieces of drawings to drop into any drawing.
    #[command(subcommand)]
    Shapes(ShapesCmd),
    /// Create a folder.
    Mkdir { vault: String, path: String },
    /// Move or rename a document or folder; with --to-vault, move a note or
    /// folder to another vault (TO is its folder there, ending in `/`, or its
    /// new path).
    Mv {
        vault: String,
        from: String,
        to: String,
        /// Rewrite [[wiki links]] in other notes that pointed at the moved note.
        #[arg(long, conflicts_with = "to_vault")]
        update_links: bool,
        /// Move the note or folder to this vault (id or name), with the
        /// attachments in it and those its notes link to; the originals go to
        /// the trash once the server has the copies (their history, comments
        /// and shares stay with them).
        #[arg(long, value_name = "VAULT")]
        to_vault: Option<String>,
    },
    /// Give a note or folder an icon (an emoji), or take it away with --remove.
    Icon {
        vault: String,
        path: String,
        #[arg(required_unless_present = "remove")]
        icon: Option<String>,
        #[arg(long, conflicts_with = "icon")]
        remove: bool,
    },
    /// Put a document or folder just before or after another in its folder
    /// (the order the sidebar and `ls` show; folders stay above notes), or a
    /// folder's contents back to A–Z with --reset.
    Order {
        vault: String,
        /// The document or folder; with --reset, the folder ("/" = the top).
        path: String,
        #[arg(long, value_name = "OTHER", required_unless_present_any = ["after", "reset"], conflicts_with_all = ["after", "reset"])]
        before: Option<String>,
        #[arg(long, value_name = "OTHER", conflicts_with = "reset")]
        after: Option<String>,
        #[arg(long)]
        reset: bool,
    },
    /// Comments on a note: list them, comment on some of its text, reply,
    /// edit, delete, resolve or reopen (docs/comments.md).
    #[command(subcommand)]
    Comments(CommentsCmd),
    /// Comments that mention you (@Name), newest first, in all your vaults.
    Mentions(MentionsArgs),
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
    /// Export a vault as plain Markdown files and attachments (directory or
    /// .zip) — without one, every vault and the quick notes, to import on
    /// another server (`import <file>`).
    Export {
        vault: Option<String>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        force: bool,
        /// Without a vault: leave this vault out (repeatable).
        #[arg(long, value_name = "VAULT", conflicts_with = "vault")]
        skip: Vec<String>,
        /// Without a vault: leave the quick notes out.
        #[arg(long, conflicts_with = "vault")]
        no_quick_notes: bool,
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
    /// Import Markdown files and attachments (directory or .zip) into a vault,
    /// keeping folders — or, with only a source, an export of vaults and quick
    /// notes (`export --out`): its vaults are created again.
    Import {
        /// The vault to import into (or, alone, an export of vaults).
        vault: String,
        source: Option<PathBuf>,
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
        /// Keep the comments written so far to the vault's members: they see
        /// only those written from now on. (By default the note goes with
        /// its comments — to everyone it is shared with.)
        #[arg(long)]
        without_comments: bool,
    },
    /// Stop sharing a note with someone (rotates the note's key).
    UnshareNote {
        vault: String,
        path: String,
        user: String,
    },
    /// Who a note is shared with individually.
    NoteShares { vault: String, path: String },
    /// Give an individually shared note a new key for its current recipients
    /// (finishes an interrupted `unshare-note`).
    RotateNoteKey { vault: String, path: String },
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

#[derive(Args, Clone, Debug, Serialize, Deserialize)]
pub struct QuickArgs {
    /// The note's text (several words are joined with spaces). Read from
    /// stdin when omitted.
    pub text: Vec<String>,
    /// List the quick notes, newest first (name and first line), instead of
    /// adding one.
    #[arg(long, conflicts_with_all = ["text", "delete", "move_note"])]
    pub list: bool,
    /// With --list: how many to show.
    #[arg(long, default_value_t = 20, requires = "list")]
    pub limit: usize,
    /// Delete this quick note (its name, e.g. "2026-10-25 09.15") for good.
    #[arg(long, value_name = "NOTE", conflicts_with_all = ["text", "move_note"])]
    pub delete: Option<String>,
    /// Move this quick note (its name) to the vault given with --to.
    #[arg(
        long = "move",
        value_name = "NOTE",
        requires = "to",
        conflicts_with = "text"
    )]
    pub move_note: Option<String>,
    /// With --move: the vault (id or name).
    #[arg(long, value_name = "VAULT", requires = "move_note")]
    pub to: Option<String>,
    /// With --move: the folder in that vault (default: its top level).
    #[arg(long, requires = "move_note")]
    pub folder: Option<String>,
    /// With --move: the note's name there (default: its first line).
    #[arg(long, requires = "move_note")]
    pub name: Option<String>,
}

#[derive(Subcommand, Clone, Debug, Serialize, Deserialize)]
pub enum ShapesCmd {
    /// List the saved shapes, newest first.
    Ls,
    /// Save the shapes of an Excalidraw library file (.excalidrawlib); those
    /// saved already are left out.
    Import { file: PathBuf },
    /// Write the saved shapes as an Excalidraw library file
    /// (.excalidrawlib), to stdout without --out.
    Export {
        #[arg(long)]
        out: Option<PathBuf>,
        /// Replace an existing file.
        #[arg(long, requires = "out")]
        force: bool,
    },
}

#[derive(Args, Clone, Debug, Serialize, Deserialize)]
pub struct MentionsArgs {
    /// Only this vault (id or name).
    #[arg(long)]
    pub vault: Option<String>,
    /// Only those not read yet (on any of your devices).
    #[arg(long)]
    pub unread: bool,
    /// Mark them read, everywhere (with everything before them).
    #[arg(long)]
    pub mark_read: bool,
    /// How many to show.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
}

#[derive(Args, Clone, Debug, Serialize, Deserialize)]
pub struct RecoverArgs {
    /// Encrypted export to decrypt (from `export-encrypted`, `GET /api/v1/me/export`
    /// or `notekpr-server export-user`). Without it, this device downloads
    /// one, or uses its local copies (see --local).
    #[arg(long)]
    pub from: Option<PathBuf>,
    /// New (or empty) folder for the recovered files.
    #[arg(long)]
    pub out: PathBuf,
    /// Read the recovery key from this file (otherwise from
    /// NOTEKPR_RECOVERY_KEY, or asked for).
    #[arg(long)]
    pub recovery_key_file: Option<PathBuf>,
    /// Use this device's local copies, without contacting the server (done
    /// anyway when the server can't be reached). They hold what this device
    /// synced and the files it downloaded; not notes shared with you one by one.
    #[arg(long, conflicts_with = "from")]
    pub local: bool,
}

#[derive(Args, Clone, Debug, Serialize, Deserialize)]
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
    /// Unlock this device with your recovery key (read from this file) instead of
    /// approving it from another device. Also NOTEKPR_RECOVERY_KEY, or type
    /// it while waiting for approval. Never on the command line: shells keep history.
    #[arg(long)]
    pub recovery_key_file: Option<PathBuf>,
    /// Allow plain http:// to a non-local server (insecure).
    #[arg(long)]
    pub insecure_http: bool,
}

#[derive(Subcommand, Clone, Debug, Serialize, Deserialize)]
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

/// IDs: a comment's id, or the start of it that only it has (`list` shows
/// the first eight characters). TEXT: words joined with spaces, or stdin;
/// `@Name` in it mentions a member of the vault.
#[derive(Subcommand, Clone, Debug, Serialize, Deserialize)]
pub enum CommentsCmd {
    /// The note's comment threads (resolved ones too with --all).
    List {
        vault: String,
        note: String,
        #[arg(long)]
        all: bool,
    },
    /// Comment on the text given with --on (it must appear once in the note).
    Add {
        vault: String,
        note: String,
        #[arg(long, value_name = "QUOTE")]
        on: String,
        text: Vec<String>,
    },
    /// Reply to a comment's thread.
    Reply {
        vault: String,
        note: String,
        id: String,
        text: Vec<String>,
    },
    /// Change one of your own comments.
    Edit {
        vault: String,
        note: String,
        id: String,
        text: Vec<String>,
    },
    /// Delete one of your comments (a vault owner deletes any; a comment
    /// takes its replies with it).
    Delete {
        vault: String,
        note: String,
        id: String,
    },
    /// Mark a comment's thread as resolved.
    Resolve {
        vault: String,
        note: String,
        id: String,
    },
    /// Open a resolved thread again.
    Reopen {
        vault: String,
        note: String,
        id: String,
    },
}

#[derive(Subcommand, Clone, Debug, Serialize, Deserialize)]
pub enum RecoveryCmd {
    /// Replace the recovery key (the old one stops working).
    Rotate {
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand, Clone, Debug, Serialize, Deserialize)]
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
    /// Comments on a shared note: list, add, reply, edit, delete, resolve, reopen.
    #[command(subcommand)]
    Comments(SharedCommentsCmd),
}

/// NOTE: a shared note's title or id (`shared list`). IDs and TEXT as for
/// `notekpr comments`; `@Name` mentions the note's owner or someone who
/// commented on it.
#[derive(Subcommand, Clone, Debug, Serialize, Deserialize)]
pub enum SharedCommentsCmd {
    /// The note's comment threads (resolved ones too with --all).
    List {
        note: String,
        #[arg(long)]
        all: bool,
    },
    /// Comment on the text given with --on (it must appear once in the note).
    Add {
        note: String,
        #[arg(long, value_name = "QUOTE")]
        on: String,
        text: Vec<String>,
    },
    /// Reply to a comment's thread.
    Reply {
        note: String,
        id: String,
        text: Vec<String>,
    },
    /// Change one of your own comments.
    Edit {
        note: String,
        id: String,
        text: Vec<String>,
    },
    /// Delete one of your comments.
    Delete { note: String, id: String },
    /// Mark a comment's thread as resolved.
    Resolve { note: String, id: String },
    /// Open a resolved thread again.
    Reopen { note: String, id: String },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
pub enum ShareRole {
    Editor,
    Viewer,
}

/// A vault member's role.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
pub enum MemberRole {
    /// Can edit, empty the trash and see the activity log (people only).
    Admin,
    Editor,
    Viewer,
}

#[derive(Subcommand, Clone, Debug, Serialize, Deserialize)]
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
    /// Let viewers (and people its notes are shared with read-only)
    /// comment on notes, or not (owner; on by default).
    ViewersComment {
        vault: String,
        #[arg(value_parser = ["on", "off"])]
        setting: String,
    },
    /// Delete a vault for everyone (owner). Cannot be undone.
    Delete {
        vault: String,
        #[arg(long)]
        yes: bool,
    },
    /// List members with their key fingerprints.
    Members { vault: String },
    /// Share with a person or one of your agents (verifies and pins their key),
    /// or change a member's role.
    Share {
        vault: String,
        /// User id, exact e-mail or display name.
        user: String,
        #[arg(long, value_enum, default_value_t = MemberRole::Editor)]
        role: MemberRole,
        /// Accept a changed account key after verifying it out of band.
        #[arg(long)]
        accept_new_key: bool,
    },
    /// Give the vault an icon (an emoji, replacing a picture), or take it away with --remove.
    Icon {
        vault: String,
        #[arg(required_unless_present = "remove")]
        icon: Option<String>,
        #[arg(long, conflicts_with = "icon")]
        remove: bool,
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

#[derive(Subcommand, Clone, Debug, Serialize, Deserialize)]
pub enum TrashCmd {
    /// List trashed entries, and (owner and admins) what was deleted for
    /// good and can still be undone.
    List { vault: String },
    /// Restore an entry by path or id.
    Restore { vault: String, entry: String },
    /// Undo deleting an entry for good (by path or id) while the server
    /// still keeps it: it goes back to the trash (owner and admins).
    Undo { vault: String, entry: String },
    /// Delete everything in the trash for good (owner and admins); it can
    /// be undone for as long as the server keeps it (`trash list`).
    Empty {
        vault: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Args, Clone, Debug, Serialize, Deserialize)]
#[command(args_conflicts_with_subcommands = true)]
pub struct HistoryArgs {
    #[command(subcommand)]
    pub cmd: Option<HistoryCmd>,
    pub vault: Option<String>,
    pub path: Option<String>,
}

#[derive(Subcommand, Clone, Debug, Serialize, Deserialize)]
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

#[derive(Args, Clone, Debug, Serialize, Deserialize)]
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

#[derive(Subcommand, Clone, Debug, Serialize, Deserialize)]
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
        /// Keep the current credentials working (revoke them later).
        #[arg(long)]
        keep_old: bool,
        /// Revoke the current credentials without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Delete an agent, its memberships and sessions.
    Delete {
        agent: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Args, Clone, Debug, Serialize, Deserialize)]
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

#[derive(Subcommand, Clone, Debug, Serialize, Deserialize)]
pub enum GatewayCmd {
    /// Print the gateway's bearer token (created on first use).
    Token {
        /// Replace it with a new one (the old one stops working).
        #[arg(long)]
        rotate: bool,
    },
}
