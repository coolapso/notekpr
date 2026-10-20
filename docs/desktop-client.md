# Desktop and command-line client

`coolpsonotes` is the native client (Linux first; it also builds for macOS and
Windows). It keeps an **encrypted local replica** of your vaults for offline
use, syncs live over the WebSocket, can **mirror vaults as ordinary Markdown
files** you edit with Neovim, VS Code or Obsidian, and imports/exports plain
Markdown. The same binary is how [AI agents](agents.md) work with notes (CLI, Markdown folder, local HTTP gateway).

All cryptography happens in the client, with the same core crate the server
and the browser app use (`crates/core`). The server only ever receives
ciphertext.

## Contents

1. [Install / build](#1-install--build)
2. [Signing in](#2-signing-in)
3. [Everyday commands](#3-everyday-commands)
4. [Sync and the Markdown mirror](#4-sync-and-the-markdown-mirror)
5. [Offline behaviour](#5-offline-behaviour)
6. [How external edits are merged](#6-how-external-edits-are-merged)
7. [Conflicts, duplicates, deletes and renames](#7-conflicts-duplicates-deletes-and-renames)
8. [Sharing, members and keys](#8-sharing-members-and-keys)
9. [Export and import](#9-export-and-import)
10. [Where data is stored and how it is protected](#10-where-data-is-stored-and-how-it-is-protected)
11. [Troubleshooting](#11-troubleshooting)

## 1. Install / build

Requires a Rust toolchain (1.94+). No Node.js or other tooling.

```sh
cargo build --release -p coolpsonotes-client
install -m 0755 target/release/coolpsonotes ~/.local/bin/
coolpsonotes --help
```

## 2. Signing in

### With your identity provider (normal use)

```sh
coolpsonotes login --server https://notes.example.com
```

The client starts an OAuth 2.0 device authorization (RFC 8628) and prints a
link and a short code. Open the link in a browser where you are signed in to
the coolpsonotes web app (via your OIDC provider), check that the code matches
and approve. The client polls the server (honouring `interval`/`slow_down`)
and receives a session token. Nothing is typed into the terminal, so this also
works over SSH.

### Development mode (no OIDC) and several users side by side

A server started with `--dev-mode` lets anyone sign in as any user name —
useful for testing sharing between several people on one machine. Each
**profile** is an independent identity with its own keys and data:

```sh
cargo run -p coolpsonotes-server -- --dev-mode          # http://127.0.0.1:8080

coolpsonotes --profile alice login --server http://127.0.0.1:8080 --dev-user alice
coolpsonotes --profile bob   login --server http://127.0.0.1:8080 --dev-user bob

coolpsonotes -p alice vault create "Family"
coolpsonotes -p alice vault share Family bob --role editor
echo "# Groceries" | coolpsonotes -p alice write Family Groceries.md
coolpsonotes -p bob cat Family Groceries
```

`--profile` can also be set with `COOLPSONOTES_PROFILE`; the default profile is
`default`. `coolpsonotes profiles` lists them. `--dev-user` is refused unless
the server reports development mode in `/api/v1/info`.

Plain `http://` is only accepted for `localhost`, `127.0.0.1` and `::1`; any
other host needs `https://` (or the explicit, insecure `--insecure-http`).

### First device: account keys and the recovery key

The first time an account signs in anywhere, the client generates the account
key pair, a device key and a **recovery key**, uploads only public keys and
ciphertexts, and prints the recovery key once:

```
========================================================================
  YOUR RECOVERY KEY (shown only this once):

      FRGN4-EFBCD-G4SA6-WVJB7-EVQOU-KKODS-4LRDN-I6HVW-MGYVI-IFNDP-TIQIQ
  ...
```

Store it in a password manager or on paper. It is the only way back in if you
lose every device; nobody, including the server administrator, can recover
your notes without it. Replace it any time with `coolpsonotes recovery rotate`.

The keys are saved (sealed) on this device before anything is uploaded. If
the setup is interrupted — the network fails, the process is killed — run
`login` again: it uploads the same keys and shows the recovery key again, as
long as it was never displayed.

### Additional devices

If your account already has keys (you use the web app or another computer),
`login` registers this computer as a **pending device** and shows a
verification code:

```
This device must be approved by one of your other devices.
On a signed-in device run `coolpsonotes devices approve 6225…` (or open Devices in the web app)
and approve only if it shows this verification code:

    HORY-OK2F-B62Y
```

Approve it from another device — `coolpsonotes devices approve <id>` shows the
same code and asks for confirmation (`--yes` skips the prompt) — or type your
recovery key at the prompt (or pass `--recovery-key-file FILE`, or set
`COOLPSONOTES_RECOVERY_KEY`) to unlock the account and approve the device
yourself. The key is never accepted as a command-line argument, which shells
keep in their history. If you interrupt the wait, run `login` again: it resumes with the
same pending device.

Comparing the codes defeats a server that substitutes its own device key: the
code is derived from the user id, the device id and the device's public key.

If this computer was removed from the account (`devices revoke`, or in the
web app), the next `login` notices — only after the server's device list
confirms it, never on a bare error — asks before continuing (on a terminal),
moves the old sealed keys aside (`account.sealed.removed-<time>` in the data
directory, never deleted) and enrolls the computer as a new device.

### Logging out

* `coolpsonotes logout` ends the session; local encrypted copies stay (read
  them offline; `login` again to sync).
* `coolpsonotes logout --forget` also removes this device from the account and
  deletes the profile and all of its local data. It uploads queued changes
  first; if some cannot be uploaded (offline, too large) it says how many
  would be lost before asking (`--yes` confirms). The decrypted Markdown
  mirror folders of the profile are deleted only when you agree to it at the
  prompt or pass `--remove-mirror`; otherwise they are left in place (and
  named). `vault leave` works the same way for one vault.
* `coolpsonotes devices list | approve <id> | revoke <id>` manages devices.
  Revoking a device ends its sessions; if it might be compromised, also rotate
  the keys of shared vaults (`vault rotate`).
* `coolpsonotes status` shows profile, server, account fingerprint, devices,
  vaults, pending changes and sync state (`--json` for scripts).

## 3. Everyday commands

Vaults are referred to by id or by (unique, case-insensitive) name; paths are
vault-relative, `.md` is optional for notes.

| Command | |
|---|---|
| `vault list` / `vault create <name>` | list (with role and document count) / create |
| `vault rename <v> <name>` / `vault archive <v> [--undo]` / `vault delete <v>` | owner only; delete asks you to type the name |
| `vault members <v>` | members, roles, key fingerprints and pin status |
| `vault share <v> <user> --role editor\|viewer` | see [§8](#8-sharing-members-and-keys) |
| `vault unshare <v> <user>` / `vault rotate <v>` / `vault leave <v>` | removal always rotates the key |
| `ls <v> [folder] [-r]` | folders, notes and attachments |
| `cat <v> <path>` | note text (or attachment bytes) |
| `write <v> <path>` | create or update a note from stdin — diff-merged, never a blind overwrite; `--append`, `--attachment`, `--if-match <hash>` |
| `quick [text…]` / `quick --list` | capture a quick note (text or stdin) / list the newest; see [Quick notes](#quick-notes) |
| `mkdir <v> <path>`, `mv <v> <from> <to> [--update-links]`, `rm <v> <path>` | `rm` moves to the trash; `mv` into `dir/` keeps the name; `--update-links` rewrites `[[links]]` |
| `trash list\|restore\|empty <v>` | restore by path or id; `empty` deletes permanently |
| `search <query> [--vault v]` | terms, `"phrases"`, `tag:x`, `folder:x`, `title:x`, `vault:x` |
| `backlinks <v> <path>` | notes linking to a note |
| `history <v> <path>`, `history show\|restore <v> <path> <rev>` | encrypted revisions stored on the server |
| `export <v> --out <dir\|file.zip>`, `import <v> <dir\|file.zip>` | see [§9](#9-export-and-import) |

Every command first catches up with the server (REST) and uploads queued
changes; read commands fall back to the local copy when the server is
unreachable.

**One process per profile.** Only one coolpsonotes process at a time may own
a profile's local copies (two would overwrite each other's files and lose
queued or received changes). While `sync` or `gateway` runs, read commands
(`ls`, `cat`, `search`, `backlinks`, `status`, `export`, `history`, `trash
list`, `vault list`, `quick --list`) show that process's local copies read-only — it keeps
them current — and commands that change something (`write`, `quick`, `mv`,
`rm`, `mkdir`, `import`, `vault …`, `share-note`, …) are handed to it: it
runs them on its own copies and sends back what they print and whether they
failed, so they work as usual (through a loopback connection whose port and
token are in `control.json` in the profile's data folder, readable by you
only). It runs them only for vaults it keeps (`sync Family` keeps one), and a
read-only gateway refuses changes. `sync --once` and `logout` are never
handed over: they wait up to 3 seconds for another one-shot command and then
stop with "profile … is in use by another coolpsonotes process", as do all
changing commands while an older version of the daemon runs. A second `sync`
or `gateway` on the same profile refuses to start. (Use separate profiles for a mirror and a gateway at the same time.)
The lock is released automatically when a process ends, also after a crash. `--json` gives machine-readable output for listings, `-q`
silences status lines. Status and warnings go to stderr, results to stdout.

Revisions: a note gets a revision when an editing session ends (after a CLI
write, when `sync`/the gateway stop, or after two idle minutes, once at least
five edits accumulated) and when it is compacted (every ~200 updates).

### Quick notes

Quick notes are for thoughts that don't have a home yet. They are ordinary
notes in the `Quick notes/` folder of one vault, so they sync, mirror to
Markdown files and open in any editor like every other note. The web app
lists them on its *Quick notes* page (the sidebar's *Quick notes* entry;
`Ctrl+Alt+Q` makes one), where they can be promoted to a regular note or
archived (moved to `Quick notes/Archive/`).

```sh
coolpsonotes quick Call the dentist about Tuesday   # words are joined with spaces
echo "Gift ideas: a kite" | coolpsonotes quick      # or the text from stdin
coolpsonotes quick --vault Work "Ask about the release date"
coolpsonotes quick --list                           # newest first: name and first line
coolpsonotes quick --list --archived --limit 50 --json
```

A new quick note is named by the local time, e.g.
`Quick notes/2026-10-19 14.32.md` (a second one in the same minute gets
` 2`, ` 3`, …; there is no `:` because Windows doesn't allow it in file
names). The vault is the one given with `--vault`, otherwise the one chosen
in the web app (Settings → Editor → *Vault for quick notes*, stored in your
encrypted, synced preferences), otherwise your own first vault (the oldest
one you own), otherwise the first vault you can edit. Without a connection
to the server the setting can't be read: the default among the local copies
is used, with a warning. `quick` changes data, so it waits for the profile
lock like `write`; `quick --list` only reads.

## 4. Sync and the Markdown mirror

For a step-by-step guide to working in Obsidian, Neovim or any other editor
with an always-running mirror, see [own-editor.md](own-editor.md).

```sh
coolpsonotes sync                       # encrypted replicas, live (+ the remembered mirror, see below)
coolpsonotes sync --dir ~/Notes         # + plaintext Markdown mirror, live; remembered for next time
coolpsonotes sync --dir ~/Notes --once  # catch up, ingest disk changes, push, exit
coolpsonotes sync --vault Family        # restrict to some vaults (repeatable)
coolpsonotes sync --no-dir              # forget the remembered mirror directory
coolpsonotes sync --once --allow-mass-delete   # confirm that many files were deleted on purpose (§7)
coolpsonotes sync --once --restore-missing     # write vanished files (or a missing mirror) back
```

The directory given with `--dir` is remembered in the profile: a later plain
`coolpsonotes sync` mirrors into it again and says so when it starts
("mirroring decrypted files into … (remembered …)"). `sync --no-dir` forgets
it, so `sync` keeps only the encrypted replicas. `coolpsonotes status` shows
which directory is mirrored.

`sync` subscribes to every vault over the WebSocket (`since` = the local
cursor), applies catch-up pages and live updates as they arrive, uploads local
changes, reconnects with exponential backoff and jitter, and reacts to server
events: rotated keys are fetched, vaults shared with you appear, revoked or
deleted vaults become read-only local copies, and as an owner your client hands
the vault key to members who joined before they had keys. Stop it with Ctrl-C
or SIGTERM; it flushes and saves before exiting. To run it permanently, use a
systemd user unit:

```ini
# ~/.config/systemd/user/coolpsonotes-sync.service
[Unit]
Description=coolpsonotes sync
[Service]
ExecStart=%h/.local/bin/coolpsonotes sync
Restart=on-failure
[Install]
WantedBy=default.target
```

(Plain `sync` also mirrors into a directory remembered from an earlier
`sync --dir`; run `sync --no-dir` once for encrypted replicas only.) If the
live connection ends for good (session revoked) or the Markdown mirror stops
working, `sync` exits with an error instead of carrying on half-dead, so
`Restart=on-failure` brings it back.

### Modes and their security implications

| Mode | What is on disk | Protects against |
|---|---|---|
| `sync` (default) | encrypted replica: CRDT state encrypted with the vault keys and sealed with a per-profile local key | other users on the machine (file permissions); copies of individual replica files. **Not** a copy of the whole profile directory, which includes the local key (see §10) |
| `sync --dir DIR` (**plaintext mirror**) | additionally ordinary decrypted `.md` files and attachments in `DIR` | only file permissions (files `0600`, directories `0700`) and your disk encryption |

The plaintext mirror is the point of "real Markdown files", but anything that
can read `DIR` can read those notes: other programs running as you, backup or
cloud-sync tools that pick the folder up, anyone with the unlocked laptop.
It therefore has to be **confirmed explicitly** for each directory the first
time it is used — interactively, or with `--plaintext-ok` — and the
confirmation is remembered in the profile for that directory only (pointing
the mirror somewhere else asks again). The server and the encrypted copies
are unaffected.

### Layout

```
~/Notes/
  Family/                 <- vault name (sanitised: / \ : * ? " < > | -> _)
    Groceries.md
    Recipes/Pancakes.md
    assets/photo.jpg      <- attachments are plain files
    Empty folder/         <- explicit (empty) folders are directories
  Work (3f2a1c)/          <- a second vault with the same name gets its id suffix
```

The directory name is chosen once per vault and kept, even if the vault is
renamed later. A small `.coolpsonotes-vault` file in it records which vault it
mirrors: a folder that already holds other files — left behind by a deleted or
left vault with the same name, or not created by coolpsonotes at all — is never
adopted (its files would be imported into the vault); the vault gets a
suffixed folder (`Diary (3f2a1c)`) instead. Entries whose path has a hidden
segment (`.git/…`, `.obsidian/…`) are never written to disk: a collaborator
must not be able to plant files such as git hooks in your mirror. On Windows,
paths that are not valid Win32 names (`C:` prefixes, `:` `<>"|?*`, device names
such as `CON`, trailing dots or spaces) are not mirrored either.

On case-insensitive file systems (the macOS and Windows defaults, detected at
start), paths are compared ignoring case: vaults named `Notes` and `notes` get
separate folders, and notes whose paths differ only in case (`Idea.md`,
`idea.md`) get distinct file names (`Idea.md`, `idea 2.md`) instead of
overwriting each other. Renaming a file by changing only its case on disk is
not synced.

Accented letters can be written two ways — "é" as one character, or as "e"
plus a separate accent mark, which is how macOS lists file names (and how zip
files made there store them). Note names always use the first form, so
`Café.md` from a Mac is the same note as `Café.md` typed anywhere else; the
file keeps the name it has on disk. Where the file system ignores the
difference (macOS, detected at start), the mirror compares names that way.

### Ignored files

Dotfiles and dot-directories (`.git`, `.obsidian`, `.trash`, `.DS_Store`, our
own temporary files), editor swap/backup/lock files (`*.swp`, `*.swo`, `*~`,
`.#*`, `#*#`, `4913`, `~$*`, `*.tmp`, `*.part`, `*.bak`) and symlinks are never
synced. Markdown files must be UTF-8.

### How changes are detected

A `notify` watcher triggers a pass ~300 ms after file activity settles; a full
scan runs at startup (catching edits made while the client was not running)
and every 30 s as a safety net. If the directory cannot be watched (the
inotify watch limit, `fs.inotify.max_user_watches`, or a file system without
change events) the client warns and scans every 5 s instead; it re-attaches
the watch when that becomes possible (e.g. after a remount). One file that
cannot be read or written (a name the file system rejects, missing
permissions) is reported once and skipped; it never stops the other files. Each file's size, mtime and content hash are
recorded when written or read, so the client recognises its own writes and
never re-uploads unchanged files; files touched within the timestamp
granularity are compared by content. Remote changes are written ~150 ms after
they arrive, atomically (temporary file + rename), and only if the file still
matches what the client last saw.

## 5. Offline behaviour

"Offline" includes a server that answers with a server error (HTTP 5xx, e.g.
a reverse proxy's `502` while the server restarts): changes stay queued and
reads use the local copies.

Everything works offline against the encrypted replica: `ls`, `cat`, `search`,
`backlinks`, `write`, `mv`, `rm`, imports and mirror edits. Changes are queued
as CRDT updates with a random `client_update_id` and persisted before the
command returns. When the server is reachable again (`sync`, or any command),
the queue is uploaded; a retry after a crash reuses the same ids, so the server
recognises duplicates and nothing is applied twice. Concurrent edits made
elsewhere meanwhile merge automatically (CRDT); see the next section for files
edited outside the client.

The replica is crash-safe: pending updates are journaled before document
states, and the sync cursor is written last, so an interrupted save at worst
re-downloads updates (applying them again is harmless).

If the server's history ever goes **backwards** (restored from an older backup,
or a malicious rollback), the client warns, resets its cursor to the server's
head and re-uploads the full state of every document it holds; CRDT merging
restores whatever the server lost.

## 6. How external edits are merged

A synced note is a CRDT document; the `.md` file is a projection of it. When an
external program changes the file, the client never copies the file over the
document — that would silently discard edits that arrived from others since
the editor loaded the file. Instead:

1. For every note the client keeps its last few **base versions** (encrypted,
   in the replica): the CRDT state behind each text it wrote to the file, and
   the state matching each version it read back from the file.
2. When the file changes, the base the editor most likely started from is
   chosen: the one needing the fewest changed lines (then bytes) to become the
   file; ties go to the newest. Only the version the editor saved last and the
   versions the client wrote after it are candidates — so going back to an
   older text (undo after saving, `git checkout`) is synced as the change it
   is, not mistaken for an unchanged file.
3. That base is forked into a fresh document with a **new client id** (an id is
   never reused with a stale clock), the textual diff `base → file` is applied
   there as fine-grained insertions/deletions, and the resulting CRDT update
   is merged into the live document and queued for upload.
4. The merged text is written back to the file if it differs.

Example: the file contained `alpha / beta`, your editor has it open, a
collaborator changes `alpha` to `ALPHA` (the client writes that to disk), and
your editor — which did not reload — saves `alpha / beta / delta`. The base
closest to that save is the old version, so the change applied is "add
`delta`", and the result is `ALPHA / beta / delta`: both edits survive. The
same happens for edits made while the client was not running ("edit offline
with Neovim, sync later"), and when a remote edit arrives before a local save
has been picked up.

Editors that keep a buffer open will notice the file changing underneath them
(Neovim: `:e` / `autoread`); either way, saving the old buffer does not lose
the remote edit.

## 7. Conflicts, duplicates, deletes and renames

* **Concurrent edits** of the same note merge character by character. Two
  people inserting at the same position both keep their text (deterministic
  order); deleting a passage someone else is editing removes the text that
  existed when the delete was made.
* **Duplicate paths**: if two devices create `Notes/Idea.md` offline, both
  survive; the older keeps the name and the other is shown as `Idea 2.md`
  (everywhere, deterministically — nothing is renamed in the shared state).
* **Deleting a file** moves the note to the vault's **trash** (never a
  permanent delete). `coolpsonotes trash list|restore` or the web app bring it
  back, and the mirror writes it again.
* **Many files vanishing at once is not a deletion.** If a vault's whole
  folder disappears, or more than 10 of its mirrored files and more than half
  of them are missing at the same time (a moved, renamed or unmounted folder,
  a sync tool gone wrong), nothing is moved to the trash — not even by
  `--once` — and the folder is not re-created; the client warns and waits for
  the files to come back. If you really deleted them, stop the daemon and run
  `coolpsonotes sync --once --allow-mass-delete` (asked interactively with
  `--once` on a terminal); `--restore-missing` writes them back from the local
  copies instead.
* **A missing mirror directory** is never re-created silently: `sync` refuses
  to start when the remembered (or given) directory is gone although notes
  were mirrored into it, and a running daemon pauses while it is gone (an
  unmounted drive). Put it back, or start with `--restore-missing` for a fresh
  copy at that place.
* **A file replaced by a symlink, folder or special file** is not a deletion
  either: the note is kept and that path is no longer synced (the link is
  never written through).
* **Renaming or moving a file** (`mv`, or delete + create of identical content
  within 1.5 s) moves the entry, keeping its identity and history. A directory
  left empty becomes an explicit folder.
* **Remote moves and trashes** move or remove the local file — unless you
  changed it locally in the meantime: then your edit is merged into the note
  and your file is kept.
* **An unexpected local file** where a not-yet-mirrored note will be written
  (for example after re-pointing the mirror at an old directory) is never
  overwritten: it is renamed to `Name (conflict 2026-10-19 101500).md` and
  synced as its own note.
* **Attachments** are immutable blobs; changing one on disk uploads a new
  version and moves the previous one to the trash.
* **Viewer access**: local edits are not uploaded (you are warned) and the file
  is left as is.

## 8. Sharing, members and keys

```sh
coolpsonotes vault share Family "Bob" --role editor
```

The client fetches Bob's public account key, verifies its self-signature,
prints its **fingerprint** and pins it (trust on first use, stored in the
profile). Compare the fingerprint with Bob over a trusted channel (in person,
a call): Bob sees his own with `coolpsonotes status`. If the server ever
presents a different key for a pinned user, sharing refuses to continue with
a loud warning until you verify the new key and pass `--accept-new-key`; key
rotations go ahead without that person (and say so), who gets the new key
once you share with them again with `--accept-new-key`.

* People who have not set up encryption yet are added without a key; your
  client wraps the vault key for them automatically once they have (during
  `sync`, `vault members`, or any sync of that vault).
* `vault unshare` removes the member **and rotates the vault key**: they keep
  what they already downloaded, but cannot decrypt anything written later.
* `vault rotate` rotates manually (e.g. after revoking a lost device).
* Members holding the new key can still read old content through the key chain.

### Sharing a single note

Share one note with someone without giving them the vault:

```sh
coolpsonotes share-note Family "Plans/Trip.md" "Bob" --role viewer
coolpsonotes note-shares Family "Plans/Trip.md"      # who has it
coolpsonotes unshare-note Family "Plans/Trip.md" "Bob"   # also renews the note's key
```

The first share gives the note its own key; Bob receives only that key and
can't see the vault's other notes, file names, attachments or members. Key
fingerprints are checked and pinned exactly as for vault sharing. Vault members
keep working on the note as usual (their clients pick up the note's key
automatically). Rotating the vault key also renews the keys of shared notes.

A new key is never wrapped for someone whose account key changed since it was
pinned: the rotation stops and says who. Verify the fingerprint with them and
re-share with `--accept-new-key` (or stop sharing with them), then finish an
interrupted removal with `coolpsonotes rotate-note-key Family "Plans/Trip.md"`
(or `coolpsonotes vault rotate Family` for a vault).

Notes shared *with you*:

```sh
coolpsonotes shared list
coolpsonotes shared cat "Trip"
echo "- sunscreen" | coolpsonotes shared write "Trip" --append
```

`shared write` merges your text as a diff into the current version, so
concurrent edits by others are kept. Shared notes are not part of `sync`'s
Markdown mirror.

## 9. Export and import

```sh
coolpsonotes export Family --out ~/backup/family           # directory
coolpsonotes export Family --out ~/backup/family.zip       # zip archive
coolpsonotes import Family ~/old-obsidian-vault [--into Imported] [--skip-existing] [--dry-run]
coolpsonotes import Family ~/backup/family.zip
```

Exports are a plain tree of `.md` files, attachments and (empty) folders — no
proprietary metadata — readable by any Markdown tool. Trashed documents are not
exported. Without the server, `export` uses the local copy: attachments this
device never downloaded are left out and listed. An existing non-empty target is refused unless `--force`. Exports
are decrypted, so they are written for the owner only (files `0600`, folders
`0700`, also inside the zip); a failed zip export leaves no `.partial` file.

Import walks a directory or zip, keeping folders: `.md` files become notes
(existing notes at the same path are diff-merged; `--skip-existing` leaves
them alone — "the same path" ignoring case, exactly as `write` matches notes),
other files become attachments, empty directories become folders. Hidden
files and editor leftovers are skipped, as are files over the server's
attachment limit (reported). `--dry-run` reports what an import would create,
update or leave unchanged without changing anything.

### Encrypted export and offline recovery

```sh
coolpsonotes export-encrypted --out ~/backup/coolpsonotes-$(date +%F).json
coolpsonotes recover --from ~/backup/coolpsonotes-2026-10-19.json --out ~/Recovered
coolpsonotes recover --out ~/Recovered          # with this signed-in device
coolpsonotes recover --local --out ~/Recovered  # from this device's copies, no server
```

`export-encrypted` saves everything the account can read, still encrypted, as
one file that is safe to keep in backups. `recover` turns such a file into
plain Markdown files without a server, unlocking it with the recovery key
(asked for; or `--recovery-key-file`, or `COOLPSONOTES_RECOVERY_KEY`). Without
`--from`, a signed-in device downloads the export, or, when the server can't be
reached (or with `--local`), recovers from its local copies: what it synced and
downloaded, without notes shared with you one by one. See
[recovery.md](recovery.md).

## 10. Where data is stored and how it is protected

| Path | Content |
|---|---|
| `$XDG_CONFIG_HOME/coolpsonotes/profiles/<profile>.json` (mode `0600`) | server URL, session token, identity, device id, TOFU pins, sync settings, gateway token |
| `$XDG_DATA_HOME/coolpsonotes/<profile>/local.key` (`0600`) | random 256-bit local key |
| `…/<profile>/account.sealed`, `device.sealed` | account and device secrets, sealed with the local key |
| `…/<profile>/vaults/<id>/` | sealed replica: state, pending journal, document states, mirror bookkeeping; attachment blobs (vault-key ciphertext) |
| `…/<profile>/audit.jsonl` | gateway write log (no content) |

`COOLPSONOTES_CONFIG_DIR` and `COOLPSONOTES_DATA_DIR` override the two roots.

Honest limits: `local.key` lives in the same profile directory as the sealed
data, so **a copy of that directory (a backup, a synced folder) can be
decrypted** — keep it out of cloud-sync and unencrypted backups. Moving the
local key into the OS keyring is planned ([TODO.md](../TODO.md)). The key does
not protect against malware or anyone able to run programs as your OS user
either — they can read `local.key` too.
Use full-disk encryption (LUKS, FileVault, BitLocker) and a screen lock for the
lost-or-stolen-laptop case. Session tokens in the profile grant server access
(not decryption) until they expire or the device is revoked.

## 11. Troubleshooting

* **"refusing to use plain http://"** — use `https://`, or `--insecure-http`
  for a trusted test network.
* **"this device is still waiting for approval"** — run `login` again to keep
  waiting, approve it from another device (`devices approve`), or use the
  recovery key.
* **"the session expired or was revoked"** — run `coolpsonotes login` again;
  local data and queued changes are kept and uploaded afterwards.
* **"the account key … CHANGED"** — someone's key differs from the pinned one.
  Verify the new fingerprint with them before using `--accept-new-key`.
* **"vault … was shared with you but its key has not arrived yet"** — the
  owner's client grants it next time it syncs.
* **"local.key is missing, but … contains encrypted local data"** — the key
  that seals this profile's files was deleted (or not restored with a
  backup). The client refuses to create a new one, which would make every
  local copy unreadable: restore `local.key`, or start over with
  `logout --forget` and `login` (changes never uploaded are lost).
* **"read-only on this device"** — access was revoked or the vault deleted; the
  local copy stays readable (`cat`, `export`) but is not synced.
* **A file does not sync** — hidden names and editor temporary files are
  ignored by design; Markdown must be UTF-8; very large attachments are
  rejected by the server limit (warning printed).
* **"changes are larger than the server accepts"** — large edits (a big paste
  or import) are split into several updates below the server's per-update
  limit (`/info`, remembered for offline use). A change that cannot be split
  is kept on this device (still visible locally, counted as "TOO LARGE" in
  `status`) and set aside, so everything else keeps uploading; it goes out by
  itself once the administrator raises `COOLPSONOTES_MAX_UPDATE_KB`.
* **Environment overlap** — the server also reads `COOLPSONOTES_DATA_DIR`; do
  not export it in a shell that also starts a server, or use separate shells.
* **More detail** — `COOLPSONOTES_LOG=debug coolpsonotes sync` logs to stderr.
  Logs never contain tokens, keys or note content; document paths appear only
  at debug level (and in the direct output of commands you ran, such as
  `export`). Default status lines show counts, not names.
