# Desktop and command-line client

`notekpr` is the native client (Linux first; it also builds for macOS and
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
cargo build --release -p notekpr-client
install -m 0755 target/release/notekpr ~/.local/bin/
notekpr --help
```

`notekpr --version` says which release it is; one built from source says
`dev` unless `NOTEKPR_VERSION` names a version while building
(`NOTEKPR_VERSION=0.2.0 cargo build …`). It sends that with every request
to the server (`Notekpr-App: cli/<version>`, protocol.md).

## 2. Signing in

### With your identity provider (normal use)

```sh
notekpr login --server https://notes.example.com
```

The client starts an OAuth 2.0 device authorization (RFC 8628) and prints a
link and a short code. Open the link in a browser where you are signed in to
the notekpr web app (via your OIDC provider), check that the code matches
and approve. The client polls the server (honouring `interval`/`slow_down`)
and receives a session token. Nothing is typed into the terminal, so this also
works over SSH.

`--server` is only needed for a profile's first login; later ones use the
server it remembers. `--device-name NAME` sets the name shown in your device
list (default: `notekpr CLI on <hostname>`).

### Development mode (no OIDC) and several users side by side

A server started with `--dev-mode` lets anyone sign in as any user name —
useful for testing sharing between several people on one machine. Each
**profile** is an independent identity with its own keys and data:

```sh
cargo run -p notekpr-server -- --dev-mode          # http://127.0.0.1:8080

notekpr --profile alice login --server http://127.0.0.1:8080 --dev-user alice
notekpr --profile bob   login --server http://127.0.0.1:8080 --dev-user bob

notekpr -p alice vault create "Family"
notekpr -p alice vault share Family bob --role editor
echo "# Groceries" | notekpr -p alice write Family Groceries.md
notekpr -p bob cat Family Groceries
```

`--profile` can also be set with `NOTEKPR_PROFILE`; the default profile is
`default`. `notekpr profiles` lists them. `--dev-user` is refused unless
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
your notes without it. Replace it any time with `notekpr recovery rotate`
(it asks first; `--yes` skips that).

The keys are saved (sealed) on this device before anything is uploaded. If
the setup is interrupted — the network fails, the process is killed — run
`login` again: it uploads the same keys and shows the recovery key again, as
long as it was never displayed.

The command line makes no vault for you (the web app makes *My notes* when it
sets an account up): while the account has none — its quick notes don't count
— `login` says how to start, with `vault create`, `sync --dir` and `import`.

### Additional devices

If your account already has keys (you use the web app or another computer),
`login` registers this computer as a **pending device** and shows a
verification code:

```
This device must be approved by one of your other devices.
On a signed-in device run `notekpr devices approve 6225…` (or open Devices in the web app)
and approve only if it shows this verification code:

    HORY-OK2F-B62Y
```

Approve it from another device — `notekpr devices approve <id>` shows the
same code and asks for confirmation (`--yes` skips the prompt) — or type your
recovery key at the prompt (or pass `--recovery-key-file FILE`, or set
`NOTEKPR_RECOVERY_KEY`) to unlock the account and approve the device
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

* `notekpr logout` ends the session; local encrypted copies stay (read
  them offline; `login` again to sync).
* `notekpr logout --forget` also removes this device from the account and
  deletes the profile and all of its local data. It uploads queued changes
  first; if some cannot be uploaded (offline, too large) it says how many
  would be lost before asking (`--yes` confirms). The decrypted Markdown
  mirror folders of the profile are deleted only when you agree to it at the
  prompt or pass `--remove-mirror`; otherwise they are left in place (and
  named). `vault leave` works the same way for one vault.
* `notekpr devices list | approve <id> | revoke <id>` manages devices
  (`approve` and `revoke` ask first; `--yes` skips that). Revoking a device
  ends its sessions; if it might be compromised, also rotate the keys of
  shared vaults (`vault rotate`).
* `notekpr status` shows profile, server, account fingerprint, devices,
  vaults (with their ids, and what they take on the server of how much they
  may), pending changes and sync state (`--json` for scripts).

## 3. Everyday commands

Vaults are referred to by id or by (unique, case-insensitive) name; paths are
vault-relative, `.md` is optional for notes (`.drawing` for drawings).
Your quick notes answer to the name `Quick notes`, which no vault may have
(in any case): `vault create` and `vault rename` refuse it, as the web app
does. A vault named so anyway (by a
client that doesn't check: the server can't, names are encrypted) is reached
by its id.

| Command | |
|---|---|
| `vault list` / `vault create <name>` | list (with role, document count, how many drawings each holds of how many it may — see [Drawings](#drawings) —, what each takes on the server of its storage limit — `FULL` at it —, its id, and how many quick notes there are) / create |
| `vault rename <v> <name>` / `vault archive <v> [--undo]` / `vault delete <v>` | owner only; delete asks you to type the name (`--yes` skips that; without a terminal it is required) |
| `vault icon <v> <emoji>` / `vault icon <v> --remove` | owner only; the vault's icon (replaces a picture set in the browser) |
| `vault viewers-comment <v> on\|off` | owner only; whether viewers (and people its notes are shared with read-only) may comment (on by default) |
| `vault members <v>` | members, roles, key fingerprints and pin status |
| `vault share <v> <user> --role admin\|editor\|viewer` | owner only; for an existing member it changes their role; admins (people only) also empty the trash; see [§8](#8-sharing-members-and-keys) |
| `vault unshare <v> <user>` / `vault rotate <v>` / `vault leave <v>` | unshare and rotate: owner only; removal always rotates the key; the owner can't leave (delete instead) |
| `ls <v> [folder] [-r]` | folders, notes and attachments, in the sidebar's order (`-r`: each folder followed by what's in it) |
| `cat <v> <path>` | note text (or attachment bytes; a drawing as a `.drawing` file: Excalidraw's JSON under notekpr's `type`) |
| `write <v> <path>` | create or update a note from stdin — diff-merged, never a blind overwrite; `--append`, `--attachment`, `--if-match <hash>` (ignored with the other two). A path ending in `.drawing` takes a drawing file on stdin — a `.drawing`, Excalidraw's `.excalidraw`, Obsidian's `.excalidraw.md`, what Excalidraw copies, or a PNG or SVG with the drawing inside (Excalidraw's "Embed scene", notekpr's exports) — and makes or changes the drawing there, element by element. `@today`, `@tomorrow` and `@yesterday` in the text become those days' dates (`@2026-10-23`, in the local time zone), as in the web app; so do they with `quick`, `shared write` and the gateway, but not in files of the Markdown folder |
| `quick [text…]` / `quick --list` / `quick --move <note> --to <v>` / `quick --delete <note>` | capture a quick note (text or stdin) / list them / move one to a vault / delete one for good; see [Quick notes](#quick-notes) |
| `shapes ls` / `shapes import <file.excalidrawlib>` / `shapes export [<id>…] [--out <file>] [--force]` / `shapes rename <id> <name>` / `shapes rm <id>…` | your saved shapes (Excalidraw's library): list them, newest first (id, when, name, what they are made of; `--json` for the full ids) / save the shapes of an Excalidraw library file, leaving out those saved already / write them (all, or those named) as one (to stdout without `--out`) / name one (`""` takes its name away) / remove some. A shape is named by its id or the start of it, as `ls` shows; see [Saved shapes](#saved-shapes) |
| `mkdir <v> <path>`, `mv <v> <from> <to> [--update-links]`, `rm <v> <path>` | `rm` moves to the trash (from the quick notes: deletes for good); `mv` into `dir/` keeps the name; `--update-links` rewrites `[[links]]` |
| `mv <v> <note, drawing or folder> <to> --to-vault <other>` | move a note or drawing, or a folder with everything in it, to another vault: `to` is its folder there (ending in `/`, `/` alone for the top level) or its new path; a name taken there gets a number (`Projects 2`). Inside a folder everything keeps its place; attachments elsewhere its notes link to come along (beside the note, or at the folder's top level), and `[[links]]` that wouldn't find a copy there are rewritten. The originals go to the trash once the server has the copies, except attachments a note staying behind links to; their history, comments and shares stay with them. Drawings need room in the other vault (each vault holds so many): without it nothing is copied. Files move only with notes. `--json` prints what moved |
| `order <v> <path> --before\|--after <other>` / `order <v> <folder> --reset` | put a note, attachment or folder just before or after another of the same folder (the order the sidebar and `ls` show; folders stay above notes; a folder only made by its notes gets an entry), or a folder's contents (`/` = the top level) back to A–Z; moving something to another folder drops its place |
| `comments list <v> <note> [--all]` | a note's comment threads: the text each is about, the comments and replies with their authors, ids (first 8 characters) and whether unverified, not sent yet or edited; `--all` adds resolved ones; `--json` for the full ids |
| `comments add <v> <note> --on <quote> [text…]` | comment on the quoted text (it must appear once in the note); the comment is the words or stdin. Viewers can comment too, unless the vault's owner turned it off |
| `comments reply\|edit\|delete\|resolve\|reopen <v> <note> <id> [text…]` | `id`: a comment's id or the start of it that only it has. Only its author edits a comment; its author or the vault's owner deletes it (a comment takes its replies with it) |
| `@Name` in a comment's text | mentions that member of the vault (any case; longer names win: `@Ann Lee` over `@Ann`). Only the vault's members can be mentioned: other `@words` stay text, with a warning. The output says whom it mentioned |
| `mentions [--vault v] [--unread] [--mark-read] [--limit n]` | comments that mention you, newest first (`*`: not read on any device yet), with the note, who and when; `--mark-read` marks them read everywhere (the web app's inbox too), with everything before them |
| `icon <v> <path> <emoji>` / `icon <v> <path> --remove` | a note's, attachment's or folder's icon; `ls` and `vault list` show icons, and people a note is shared with see its icon and new name |
| `trash list\|restore\|undo\|empty <v>` | restore by path or id; `empty` deletes for good (the vault's owner and admins; asks first; `--yes`), which `undo <v> <entry>` takes back while the server keeps it (`--purge-undo-days`, 30 by default): the entry goes back to the trash. `list` shows those too, for the owner and admins, with how long they are kept (`kept_until` with `--json`) |
| `search <query> [--vault v] [--limit n]` | terms, `"phrases"`, `tag:x`, `folder:x`, `title:x`, `vault:x`; 20 results unless `--limit` |
| `backlinks <v> <path>` | notes linking to a note |
| `history <v> <path>`, `history show\|restore <v> <path> <rev>` | encrypted revisions stored on the server |
| `export <v> --out <dir\|file.zip>`, `import <v> <dir\|file.zip>` | see [§9](#9-export-and-import) |
| `export --out <dir\|file.zip> [--skip <v>]… [--no-quick-notes]`, `import <dir\|file.zip>` | every vault and your Personal storage (quick notes, saved shapes) at once, to move to another server: see [§9](#moving-to-another-server) |
| `delete-account [--yes] [--remove-mirror]` | delete your account on the server for good — the vaults you own go, also for the people you share them with; you leave the others — and this profile with its local data (asks first; `--remove-mirror` deletes its Markdown folders too, asked otherwise) |
| `agent …`, `gateway …` | AI agents and the local HTTP API: see [agents.md §2](agents.md#2-setting-up-an-agent) and [§4](agents.md#4-the-gateway-local-http-api) |

Every command first catches up with the server (REST) and uploads queued
changes; read commands fall back to the local copy when the server is
unreachable. Scripts sending many commands quickly may hit the server's rate
limit: commands then wait as long as the server says and retry (for up to
30 s) instead of failing.

**One process per profile.** Only one notekpr process at a time may own
a profile's local copies (two would overwrite each other's files and lose
queued or received changes). While `sync` or `gateway` runs, read commands
(`ls`, `cat`, `search`, `backlinks`, `status`, `export`, `history`, `trash
list`, `vault list`, `quick --list`, `shapes ls`, `shapes export`,
`note-shares`, `recover`) show that
process's local copies read-only — it keeps them current — and commands that
change something (`write`, `quick`, `shapes import`, `shapes rename`, `shapes rm`, `mv`, `rm`, `mkdir`, `import`,
`vault …`, `share-note`, …) are handed to it right away: it runs them on its
own copies and sends back what
they print and whether they failed, so they work as usual (through a loopback
connection whose port and token are in `control.json` in the profile's data
folder, readable by you only). It runs them only for vaults it keeps
(`sync --vault Family` keeps one), and a gateway started with `--read-only`
refuses every command handed to it ("the running gateway is read-only: it
makes no changes"). `sync --once` and `logout --forget` are never handed
over: they wait up to 3 seconds for another one-shot command and then stop
with "profile … is in use by another notekpr process", as do all
changing commands while an older version of the daemon runs. Commands that
don't use the local copies run right away, whatever else is running:
`login`, `logout` (without `--forget`), `profiles`, `devices …`,
`recovery …`, `shared …`, `agent …`, `export-encrypted`, `gateway token` and
`sync --no-dir`. A second
`sync` or `gateway` on the same profile refuses to start. (Use separate
profiles for a mirror and a gateway at the same time.) The lock is released
automatically when a process ends, also after a crash. `--json` gives
machine-readable output for listings, `-q` silences status lines. Status and
warnings go to stderr, results to stdout.

Revisions: a note gets a revision when an editing session ends (after a CLI
write, when `sync`/the gateway stop, or after two idle minutes, once at least
five edits accumulated) and when it is compacted (every ~200 updates).

### Quick notes

Quick notes are for thoughts that don't have a home yet: a place of your
own, in no vault, the same from every app and device. They are never shared
(no members, no single-note shares, no agents, no comments) and stay flat
(no folders). There are at most 20 of them unless the server's
administrators say otherwise (`quick_notes_per_user`, or a limit of your
own): they are a place to look through, not to keep things. A quick note
leaves by moving to a vault, or by being deleted — for good: quick notes
have no trash. The web app lists them on its *Quick notes* page (the
sidebar's *Quick notes* entry; `Alt+Q` makes one).

```sh
notekpr quick Call the dentist about Tuesday   # words are joined with spaces
echo "Gift ideas: a kite" | notekpr quick      # or the text from stdin
notekpr quick --list                           # newest first: name and first line
notekpr quick --list --limit 50 --json
notekpr quick --move "2026-10-19 14.32" --to Family --folder Plans   # named after its first line
notekpr quick --move "2026-10-19 14.32" --to Family --name "Dentist"
notekpr quick --delete "2026-10-19 14.32"     # for good
```

A new quick note is named by the local time, e.g. `2026-10-19 14.32` (a
second one in the same minute gets ` 2`, ` 3`, …; there is no `:` because
Windows doesn't allow it in file names). Behind the scenes they live in your
**Personal storage**: a vault of your own of a special kind, made the first
time an app is online with your account, which also keeps your saved shapes
([Saved shapes](#saved-shapes)) and whatever else is yours alone rather than
a vault's; `vault list` shows how many quick notes there are, not the vault.

When they are full, a new one is refused ("Quick notes are full (20 of 20):
move some to a vault or delete them first"). One made on a device that
didn't know yet (offline, say, while another device filled them) is kept on
that device — `quick --list` marks it — and uploaded once there's room.
Your Personal storage also has a storage limit of its own, the server's
or one an administrator gave you (`vault list` shows how much it takes):
when that is reached, changes to quick notes and saved shapes stay queued
("your personal storage is full") until something there is deleted or an
administrator gives it more room.

Moving one takes the attachments it links to along (renamed if the name is
taken there, with its links following) and needs the server: the quick note
goes only once the vault has the copy. Its history stays behind. Deleting one
deletes the attachments only it uses too. `quick` changes data, so it waits
for the profile lock like `write`; `quick --list` only reads.

Quick notes keep files only for the notes that link to them (`![](scan.png)`,
`![[scan.png]]`, `[[plan.pdf]]`): `write --attachment`, `import` and the
gateway refuse a file no quick note links to yet, and the web app only takes
files into a quick note's editor. A file whose link is edited out goes for
good the next time an app catches up with the server, once it has been in the
quick notes for an hour (time for a link made on another device to arrive).
Only apps can tell: the server sees no links.

### Drawings

Drawings (`.drawing` files in the Markdown folder, `cat` prints one as
such, `write` takes one: `notekpr write Home Garden/Beds.drawing <
beds.excalidraw`) live in vaults, among the notes, and everyone in the
vault draws on them together.

A drawing file is Excalidraw's JSON under notekpr's own `type`
(`notekpr-drawing`); `write` and imports also take Excalidraw's
`.excalidraw` files, what Excalidraw copies and Obsidian's
`.excalidraw.md`. `cat --json` prints a drawing's details, its file and
its words (texts, labels, sticky notes) — what agents read too
([agents.md](agents.md)).
Pictures in a drawing are files of its vault: a drawing file that comes in
has its pictures made files in `attachments/` (or shown from a file with
the same bytes already there), and every drawing file written out — `cat`,
exports, the Markdown folder, `recover` — holds them inside, as
Excalidraw's files do. Moving a drawing to another vault takes its pictures
along (unless a drawing staying behind shows them too).

The server may limit how many drawings a vault holds (`drawings_per_vault`,
or the vault's own limit set by an administrator; by default there is
none), whoever draws in it. Drawings in the trash count until it is
emptied. `vault list` shows how many each vault holds of how many it may
("Drawings are full (1 of 1)"). A new drawing beyond the limit is refused —
"This vault may hold 1 drawing on this server: delete it for good to make
another" — and stays on your device, uploaded once there's room (one
deleted for good, or a larger limit); in the Markdown folder a new
`.drawing` file waits on disk. Moving a drawing to another vault needs room
there: `mv --to-vault` says so before copying anything.

### Saved shapes

Your **saved shapes** — groups of elements to drop into any drawing, what
Excalidraw calls its library — are yours alone and the same on all your
devices: in the web app you save what is selected and put shapes in from
their panel. They are kept, encrypted, in your Personal storage (the quick
notes' vault: its index, so they take no drawing's place; see
[protocol.md §1](protocol.md#1-data-model-the-server-sees)), and count
towards its storage limit.

```sh
notekpr shapes ls                               # newest first
notekpr shapes import shapes.excalidrawlib      # Excalidraw's library files
notekpr shapes export --out mine.excalidrawlib  # Excalidraw imports it
notekpr shapes export > mine.excalidrawlib      # the same, on stdout
notekpr shapes export 3f9a2c1e b71d --out two.excalidrawlib   # just these two
notekpr shapes rename 3f9a2c1e "Raised bed"
notekpr shapes rm 3f9a2c1e b71d
```

A shape is named by its id, or the start of it — the first 8 characters `ls`
shows are enough, fewer if no other shape's id starts the same. A shape
saved in the web app takes its name from the first text in it (cut to 40
characters), if it has one; `rename` changes it (`""` takes it away), and
the web app's panel shows the names under the pictures.

`import` adds the shapes of an `.excalidrawlib` file (version 1 or 2) and
leaves out those saved already (the same elements), so importing a file
twice adds nothing. A shape is stored as one piece, so one larger than an
update may carry on the server (half its `max_update_bytes`) is left out
with a warning instead of being queued for a server that would refuse it.
Shapes hold elements only: no pictures (Excalidraw's rule). Account exports
(`export` of everything, with the quick notes) carry them as a
`Saved shapes.excalidrawlib` file, and `import` brings them back; `recover`
writes that file in the quick notes' folder.

## 4. Sync and the Markdown mirror

For a step-by-step guide to working in Obsidian, Neovim or any other editor
with an always-running mirror, see [own-editor.md](own-editor.md).

```sh
notekpr sync                       # encrypted replicas, live (+ the remembered mirror, see below)
notekpr sync --dir ~/Notes         # + plaintext Markdown mirror, live; remembered for next time
notekpr sync --dir ~/Notes --once  # catch up, ingest disk changes, push, exit
notekpr sync --vault Family        # restrict to some vaults (repeatable)
notekpr sync --no-dir              # forget the remembered mirror directory
notekpr sync --once --allow-mass-delete   # confirm that many files were deleted on purpose (§7)
notekpr sync --once --restore-missing     # write vanished files (or a missing mirror) back
```

The directory given with `--dir` is remembered in the profile: a later plain
`notekpr sync` mirrors into it again and says so when it starts
("mirroring decrypted files into … (remembered …)"). `sync --no-dir` forgets
it, so `sync` keeps only the encrypted replicas. `notekpr status` shows
which directory is mirrored.

`sync` subscribes to every vault over the WebSocket (`since` = the local
cursor), applies catch-up pages and live updates as they arrive, uploads local
changes, reconnects with exponential backoff (1 s, doubling up to 60 s, ±25 %
jitter; when the server announces a restart, about every 0.25–1.25 s for up
to a minute), and reacts to server events: rotated keys are fetched, vaults
shared with you appear, revoked or deleted vaults become read-only local
copies, and as an owner your client hands the vault key to members who joined
before they had keys. Stop it with Ctrl-C or SIGTERM; it flushes and saves
before exiting. To run it permanently, use a systemd user unit:

```ini
# ~/.config/systemd/user/notekpr-sync.service
[Unit]
Description=notekpr sync
[Service]
ExecStart=%h/.local/bin/notekpr sync
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
  Quick notes/            <- your quick notes: flat, no folders
    2026-10-19 14.32.md
  Saved shapes.excalidrawlib   <- your saved shapes, Excalidraw's library file
```

**Quick notes** ([§3](#quick-notes)) are the folder `Quick notes/`, mirrored
first so the name is theirs (a vault named so anyway gets the id suffix). It is
flat: what is put into folders inside it is not synced (you are told once).
Any `.md` file dropped there becomes a quick note, whatever its name, while
there is room; beyond the limit a new file stays on disk, not synced, and
goes up once there's room (moved or deleted ones). Any other file waits on
disk the same way until a quick note links to it. Moving a file from there
into a vault's folder moves the note there. Deleting a file deletes the note
**for good** — quick notes have no trash — so more than 3 of them vanishing at
once is held like a mass deletion (below).

**Drawings** ([Drawings](#drawings)) are `.drawing` files in their vault's
folder. A new one becomes a drawing while the vault has room for it; beyond
its drawings limit it stays on disk, not synced (you are told once), and
goes up once there's room.

**Saved shapes** ([Saved shapes](#saved-shapes)) are one file at the top,
`Saved shapes.excalidrawlib` — Excalidraw's library file, which Excalidraw
and its Obsidian plugin open — written once you have some and whenever they
change (no vault folder takes that name). Edit it, or make it, and the
change is merged in shape by shape against what the file held when it was
last written or read: shapes added there are saved (those saved already,
the same elements, are left out, as with `shapes import`), shapes changed
there are replaced, shapes taken out are removed — and changes made on your
other devices meanwhile stay, unless they touched the same shape (then the
file's change wins). Emptying its `libraryItems` removes them all. A file
that isn't an Excalidraw library (half-saved, say) is left alone with a
warning and changes nothing until it is one again; deleting the file removes
no shapes — it is written again. It comes with your Personal storage: not
there for an agent's profile, nor with `sync --vault` unless it names `"Quick notes"`.

Besides the characters shown, control characters become `_`, surrounding
spaces, leading dots and trailing dots are dropped, Windows device names get
a `_` prefix (`con` → `_con`, on every system) and names are cut to 200
bytes. If the suffixed name is taken too, ` 2`, ` 3`, … follows
(`Work (3f2a1c) 2`).

The directory name is chosen once per vault and kept, even if the vault is
renamed later. A small `.notekpr-vault` file in it records which vault it
mirrors: a folder that already holds other files — left behind by a deleted or
left vault with the same name, or not created by notekpr at all — is never
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
own temporary files), editor swap/backup/lock files (`*.swp`, `*.swo`,
`*.swx`, `*.swn`, `*.kate-swp`, `*~`, `.#*`, `#*#`, `4913`, `~$*`, `*.tmp`,
`*.part`, `*.crdownload`, `*.bak`), `Thumbs.db`, `desktop.ini` and symlinks
are never synced. Markdown files must be UTF-8.

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

A server can also be **read-only** for a while (its administrators switched
it, or its storage is full): it serves everything but refuses changes.
Changes made meanwhile stay queued like offline ones and commands say so
("the server is read-only right now: N change(s) will be uploaded when it
accepts changes again"). A running `notekpr sync` hears it over the live
connection, uploads nothing while it lasts, and uploads the queue the moment
the server accepts changes again.

A **full vault** (at its storage limit) works the same way for that vault
alone: its changes stay queued ("the vault is full: N change(s) will be
uploaded once it has room"), other vaults keep uploading, and `notekpr sync`
uploads them the moment the server says it has room — after something in it
was deleted for good (`trash empty`), or an administrator raised its limit.
To ask for more room, give the administrator the vault's id (`vault list`
or `status` show it); they see ids, owners and sizes, never names.

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
  permanent delete). `notekpr trash list|restore` or the web app bring it
  back, and the mirror writes it again. In `Quick notes/` it deletes the
  quick note for good (offline: once online). **Deleting a folder** moves it
  to the trash once what was in it is there; meanwhile it isn't made again on
  disk.
* **Many files vanishing at once is not a deletion.** If a vault's whole
  folder disappears, or more than 10 of its mirrored files and more than half
  of them are missing at the same time (a moved, renamed or unmounted folder,
  a sync tool gone wrong), nothing is moved to the trash — not even by
  `--once` — and the folder is not re-created; the client warns and waits for
  the files to come back. In `Quick notes/`, where deleting is for good, more
  than 3 missing at once are held. If you really deleted them, stop the daemon and run
  `notekpr sync --once --allow-mass-delete` (asked interactively with
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
* **A note deleted for good elsewhere** (its trash emptied) while you have
  changes to it not uploaded yet (you were offline, say): your version is
  kept as a new note, `<title> (recovered)` at the top of the vault, with a
  warning — as in the web app.
* **An unexpected local file** where a not-yet-mirrored note will be written
  (for example after re-pointing the mirror at an old directory) is never
  overwritten: it is renamed to `Name (conflict 2026-10-19 101500).md` and
  synced as its own note.
* **Attachments** are immutable blobs; changing one on disk uploads a new
  version and moves the previous one to the trash.
* **Viewer access** (or a read-only local copy): local edits are not uploaded
  (you are warned) and the file is left as is; deleted files are written back,
  and new files are ignored.

## 8. Sharing, members and keys

```sh
notekpr vault share Family "Bob" --role editor
```

The client fetches Bob's public account key, verifies its self-signature,
prints its **fingerprint** and pins it (trust on first use, stored in the
profile). Compare the fingerprint with Bob over a trusted channel (in person,
a call): Bob sees his own with `notekpr status`. If the server ever
presents a different key for a pinned user, sharing refuses to continue with
a loud warning until you verify the new key and pass `--accept-new-key`; key
rotations go ahead without that person (and say so), who gets the new key
once you share with them again with `--accept-new-key`.

* Only the owner can share, unshare and rotate. Running `vault share` again
  for someone who is already a member changes their role to the given
  `--role`.
* Roles: `editor` writes and moves things to the trash; `admin` also empties
  the trash (deleting for good, which editors can't, and undoing that for a
  while: `trash undo`) and sees the vault's activity log — people only, never agents; `viewer` reads (and comments,
  unless the owner turns it off).
* People who have not set up encryption yet are added without a key; your
  client wraps the vault key for them automatically once they have (during
  `sync`, `vault members`, or any sync of that vault).
* `vault unshare` removes the member **and rotates the vault key**, in one
  request: they keep what they already downloaded, but cannot decrypt
  anything written later.
* When someone leaves on their own (or you delete an agent), the server marks
  the key they knew, and your client rotates it the next time it syncs that
  vault (`sync`, the sync daemon, `vault members`, `status`). The same goes
  for a note someone removed from their list.
* `vault rotate` rotates manually (e.g. after revoking a lost device).
* Members holding the new key can still read old content through the key chain.

### Sharing a single note

Share one note with someone without giving them the vault:

```sh
notekpr share-note Family "Plans/Trip.md" "Bob" --role viewer
notekpr note-shares Family "Plans/Trip.md"      # who has it
notekpr unshare-note Family "Plans/Trip.md" "Bob"   # also renews the note's key
```

The first share gives the note its own key; Bob receives only that key and
can't see the vault's other notes, file names, attachments or members. The
note goes with its comments: `share-note` sends the comments written so far
again under the note's key, for everyone the note is shared with
(`--without-comments`: they see only comments written from then on). Key
fingerprints are checked and pinned exactly as for vault sharing. Vault members
keep working on the note as usual (their clients pick up the note's key
automatically). Rotating the vault key also renews the keys of shared notes.
Only the vault's owner can share a note, stop sharing it or renew its key;
running `share-note` again for someone who already has the note changes
their role.

A new key is never wrapped for someone whose account key changed since it was
pinned: the rotation goes ahead without them and says who. Verify the
fingerprint with them and share with them again with `--accept-new-key` (or
stop sharing with them). `notekpr rotate-note-key Family "Plans/Trip.md"`
renews a note's key by hand.

Notes shared *with you*:

```sh
notekpr shared list
notekpr shared cat "Trip"
echo "- sunscreen" | notekpr shared write "Trip" --append
```

`shared write` merges your text as a diff into the current version, so
concurrent edits by others are kept. Shared notes are not part of `sync`'s
Markdown mirror.

Their comments work like `comments …`, with the note's title (or id) in
place of vault and path:

```sh
notekpr shared comments list "Trip"
notekpr shared comments add "Trip" --on "sunscreen" "SPF 50? @Alice"
notekpr shared comments reply "Trip" 3f9a2c1d "Done"
```

You see the comments written since the note was shared with you (and older
ones if its owner included them); their authors are checked against the
owner's key and keys pinned on first use. `@Name` mentions the note's owner
or someone who commented on it — you don't see the vault's members, and
nobody can mention you on a note shared with you. Viewers can comment unless
the vault's owner turned it off.

## 9. Export and import

```sh
notekpr export Family --out ~/backup/family           # directory
notekpr export Family --out ~/backup/family.zip       # zip archive
notekpr import Family ~/old-obsidian-vault [--into Imported] [--skip-existing] [--dry-run]
notekpr import Family ~/backup/family.zip
```

Exports are a plain tree of `.md` files, drawings (`.drawing`), attachments and (empty) folders — no
proprietary metadata — readable by any Markdown tool. Trashed documents are not
exported. Comments are: each commented note's threads as a Markdown file at
`_comments/<the note's path>` (who wrote what, when, on which text). A directory export leaves out (and lists) paths the mirror wouldn't
write either — hidden segments such as `.obsidian/…`, names Windows can't
store when run there; a zip keeps hidden ones. Without the server, `export`
uses the local copy: attachments this device never downloaded are left out
and listed. An existing non-empty target is refused unless `--force`. Exports
are decrypted, so they are written for the owner only (files `0600`, folders
`0700`, also inside the zip); a failed zip export leaves no `.partial` file.

Import walks a directory or zip, keeping folders: `.md` files become notes
(existing notes at the same path are diff-merged; `--skip-existing` leaves
them, and existing attachments, alone — "the same path" ignoring case,
exactly as `write` matches notes); drawing files become drawings at their
`.drawing` path — ours, Excalidraw's `X.excalidraw` and Obsidian's
`X.excalidraw.md` (a Markdown file by that name that isn't a drawing stays a
note), and notes showing `![[X.excalidraw]]` show the imported drawing; other
files become attachments, empty
directories become folders. Hidden files and editor leftovers are skipped, as
are empty attachment files and files over the server's attachment limit
(reported), and an export's `_comments/` folder (comments can't be brought
back: they are signed by their authors). `--dry-run` reports what an import would create, update or leave
unchanged without changing anything.

### Moving to another server

```sh
notekpr export --out ~/notes.zip --skip Scratch      # every vault but Scratch, and the quick notes
notekpr --profile new login --server https://notes.example.org
notekpr --profile new import ~/notes.zip [--dry-run]
notekpr delete-account                                # then, if you like, on the old server
```

Without a vault, `export` takes every vault (but those `--skip` names), with
their drawings, and your Personal storage — the quick notes and saved shapes
(unless `--no-quick-notes`) — into one `.zip` or directory: a folder per
vault (named after it; a name some file systems can't store has `_`
instead), one called `Quick notes`, the saved shapes as `Saved
shapes.excalidrawlib`, and `.notekpr-export.json` naming which folder is
which vault — with its name, description, icon and picture, and
its notes', files' and folders' icons. The web app makes the same export
(Settings → Account → Data). `import` with only that `.zip` or directory —
on any server, also the same one — creates the vaults again, named as they
were (a name the account has already gets a number: `Family 2`), adds the
quick notes to the account's, as many as there's room for (the rest are
reported), and the saved shapes to the account's. Nothing else comes along: no history, comments (only the
`_comments/` files, which aren't imported), members or shares — the new
server starts fresh.

### Encrypted export and offline recovery

```sh
notekpr export-encrypted --out ~/backup/notekpr-$(date +%F).json
notekpr recover --from ~/backup/notekpr-2026-10-19.json --out ~/Recovered
notekpr recover --out ~/Recovered          # with this signed-in device
notekpr recover --local --out ~/Recovered  # from this device's copies, no server
```

`export-encrypted` saves everything the account can read, still encrypted, as
one file that is safe to keep in backups (an existing file is replaced only
with `--force`). `recover` turns such a file into plain Markdown files
without a server, unlocking it with the recovery key (asked for; or
`--recovery-key-file`, or `NOTEKPR_RECOVERY_KEY`). Without
`--from`, a signed-in device downloads the export, or, when the server can't be
reached (or with `--local`), recovers from its local copies: what it synced and
downloaded, without notes shared with you one by one. Comments come along
under `_comments/`, as with `export`. `--out` must be a new or
empty folder; `--json` prints `{vaults, files, problems, source}` (`source`:
`export` or `device`). See [recovery.md](recovery.md).

## 10. Where data is stored and how it is protected

| Path | Content |
|---|---|
| `$XDG_CONFIG_HOME/notekpr/profiles/<profile>.json` (mode `0600`) | server URL, session token, identity, device id, TOFU pins, sync settings, gateway token |
| `$XDG_DATA_HOME/notekpr/<profile>/local.key` (`0600`) | random 256-bit local key |
| `…/<profile>/account.sealed`, `device.sealed` | account and device secrets, sealed with the local key |
| `…/<profile>/vaults/<id>/` | sealed replica: state, pending journal, document states, mirror bookkeeping; attachment blobs (vault-key ciphertext) |
| `…/<profile>/audit.jsonl` | gateway write log (no content) |

`NOTEKPR_CONFIG_DIR` and `NOTEKPR_DATA_DIR` override the two roots.

Honest limits: `local.key` lives in the same profile directory as the sealed
data, so **a copy of that directory (a backup, a synced folder) can be
decrypted** — keep it out of cloud-sync and unencrypted backups. Moving the
local key into the OS keyring is planned. The key does
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
* **"the session expired or was revoked"** — run `notekpr login` again;
  local data and queued changes are kept and uploaded afterwards.
* **"this server needs notekpr X or newer"** — the server was updated past
  what this version of the CLI works with: `sync` stops, and nothing else
  works with that server until you install a newer `notekpr`. Local data and
  queued changes are kept and uploaded by the new version.
* **"who is in vault … was changed by a newer version of notekpr"** — the
  vault's membership history has a kind of entry this version can't check,
  so it doesn't use the vault: update `notekpr`. Nothing local is lost.
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
  itself once the administrator raises `NOTEKPR_MAX_UPDATE_KB`.
* **Environment overlap** — the server also reads `NOTEKPR_DATA_DIR`; do
  not export it in a shell that also starts a server, or use separate shells.
* **More detail** — `NOTEKPR_LOG=debug notekpr sync` logs to stderr.
  Logs never contain tokens, keys or note content; document paths appear only
  at debug level (and in the direct output of commands you ran, such as
  `export`). Default status lines show counts, not names.
