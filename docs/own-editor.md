# Use your own editor (Obsidian, Neovim, VS Code, …)

Your notes don't have to live in the notekpr app. The CLI (`notekpr`) can
keep your vaults as an ordinary folder of Markdown files, always up to date
and in plain text on your machine, while the server only ever stores them
encrypted. Edit them with whatever you like — Obsidian, Neovim, VS Code, grep
and sed, an AI coding agent — and changes sync both ways, live, with everyone
who uses the vault in the web app or their own editor.

In other words: notekpr can be an end-to-end encrypted, self-hosted sync
server (a "relay") for an Obsidian vault, with real-time collaboration and a
web app on top.

## How it works

```
 your editor ⇄ ~/Notes/*.md ⇄ notekpr sync (daemon) ⇄ server (ciphertext only) ⇄ other devices / the web app
```

The `notekpr sync --dir` daemon watches the folder. When a file changes it
computes what you changed, merges it into the note's CRDT (so concurrent edits
by others are kept, not overwritten), encrypts it and uploads it. Changes from
others are decrypted and written into the files within a fraction of a second.
Details: [cli.md §4–7](cli.md#4-sync-and-the-markdown-mirror).

The daemon still keeps a small encrypted copy of each note's history in the
profile's data directory (only a `.notekpr-vault` marker file sits in
each vault's folder). It needs it to merge concurrent edits, and it is what
the server accepts — the server never takes plaintext.

## Set it up

Install the client ([cli.md §1](cli.md#1-install--build)), then:

```sh
notekpr setup
```

It signs you in, asks for the folder (`~/Notes` unless you choose another)
and keeps it in sync in the background. Notes in that folder already? See
[Coming from Obsidian](#coming-from-obsidian).

Or step by step, which is what `setup` does:

1. Sign in, once per device ([cli.md §2](cli.md#2-signing-in)):

   ```sh
   notekpr login --server https://notes.example.com
   ```

2. Start mirroring. The first time you are asked to confirm that decrypted
   files will be written to disk (or pass `--plaintext-ok`):

   ```sh
   notekpr sync --dir ~/Notes
   ```

   Every vault you can access appears as a subfolder (`~/Notes/Family/…`).
   Use `--vault NAME` (repeatable) to mirror only some.

3. Keep it running in the background with the systemd user service the
   packages install. It syncs the folder `sync --dir` remembered; the part
   after `@` is the profile:

   ```sh
   systemctl --user enable --now notekpr-sync@default
   journalctl --user -u notekpr-sync@default -f     # see what it does
   ```

   Without a package, copy
   [`packaging/systemd/notekpr-sync@.service`](../packaging/systemd/notekpr-sync@.service)
   to `~/.config/systemd/user/`, point its `ExecStart` at your `notekpr`
   and run `systemctl --user daemon-reload` first.

   It works offline too: edits are kept and uploaded when the connection is
   back.

## Coming from Obsidian

An Obsidian vault is a folder, and its folders are just folders. A notekpr
vault is what you share and encrypt as one: each has its own key and its
own members, with folders inside it as in Obsidian. So **one Obsidian vault
is one notekpr vault**. `Daily/`, `Projects/` and the rest stay folders in
it, and wiki links between them keep working (links work within a vault).

The Markdown folder is a level above: it holds one folder per vault
(`~/Notes/My notes/`, `~/Notes/Family/`) and your quick notes. What you open
in Obsidian is a vault's folder.

If your Obsidian vault is where you want your notes, `~/Notes` say, run
`notekpr setup` and choose that folder. It offers to move everything in it,
as it is, into `~/Notes/My notes/` (or the vault name you give), which then
syncs. `.obsidian` moves along, so open `~/Notes/My notes` in Obsidian
(**Open folder as vault**) and your settings, themes and plugins are there.
Or let it copy your notes into another folder and keep the old one as it is.
Details: [cli.md §2](cli.md#all-of-it-at-once-notekpr-setup).

A part you later want to share on its own can become a vault of its own:
`notekpr mv <vault> <folder> <to> --to-vault <other>` moves a folder into
another vault.

## Obsidian

* Open `~/Notes/<vault>` (or `~/Notes` for all vaults at once) with **Open
  folder as vault**. Don't also enable Obsidian Sync or another sync tool on
  that folder.
* Wiki links (`[[Note]]`, `[[Note|alias]]`, `[[Note#Heading]]`), embeds
  (`![[image.png]]`), tags and task lists work the same in Obsidian and in the
  notekpr web app.
* The `.obsidian/` folder (settings, themes, plugins) is **not synced** — on
  purpose: otherwise anyone who can edit a shared vault could install plugins
  (code) on your machine. Configure Obsidian on each device.
* Attachments are ordinary files in the folder and sync like notes.

## Neovim, VS Code and others

Any editor works. Swap, backup and lock files (`*.swp`, `*~`, `.#*`, …) and
dotfiles are ignored, and files are written atomically, so editors that
reload on change pick up remote edits cleanly.

## Good to know

* **The folder is plaintext.** Anything that can read it can read your notes:
  programs running as you, backup or cloud-sync tools that pick it up, anyone
  with your unlocked computer. Use full-disk encryption. The server and other
  devices are unaffected.
* **Collaboration is live.** If someone edits the same note in the web app
  while you edit the file, both changes are merged. Conflicting renames,
  duplicates and deletes are handled as described in
  [cli.md §7](cli.md#7-conflicts-duplicates-deletes-and-renames).
* **Moving, renaming or unmounting the folder** doesn't delete anything: when
  a vault folder (or most of its files) vanishes at once, the daemon warns and
  waits instead of trashing the notes, and it never re-creates a missing mirror
  folder by itself. See
  [cli.md §7](cli.md#7-conflicts-duplicates-deletes-and-renames)
  for confirming a deliberate mass deletion.
* **Quick notes are the folder `Quick notes/`.** Drop a `.md` file there to
  capture a thought (while there's room: 20 unless your server says
  otherwise); move it into a vault's folder to keep it there. It is flat —
  files in folders inside it aren't synced — and deleting a file there
  deletes the quick note for good (they have no trash). See
  [cli.md §4](cli.md#layout).
* **CLI commands while the daemon runs:** reading ones (`cat`, `ls`,
  `search`, …) work and show the daemon's copies; changing ones (`write`,
  `mv`, …) are handed to the daemon, which runs them on its copies (of the
  vaults it keeps) and updates the files. Details:
  [cli.md §3](cli.md#3-everyday-commands).
* **Comments aren't in the files** (they would stop being plain Markdown).
  They stay on the text they are about while you edit it here — your
  changes are merged as small edits, not as a new file — and if you delete
  that text, the comment keeps a quote of it. Read and answer them in the
  web app, or with `notekpr comments …`
  ([cli.md §3](cli.md#3-everyday-commands)).
* **Phones and tablets:** there is no mobile client yet; use the web app.
* **AI agents** (Claude Code, Codex, …) work in the folder like any editor.
  [agents.md §9](agents.md#9-telling-an-agent-how-your-notes-work) has an
  `AGENTS.md` and a skill you can give them, so they know its rules.
