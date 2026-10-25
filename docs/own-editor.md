# Use your own editor (Obsidian, Neovim, VS Code, …)

Your notes don't have to live in the notekpr app. The desktop client can
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
Details: [desktop-client.md §4–7](desktop-client.md#4-sync-and-the-markdown-mirror).

The daemon still keeps a small encrypted copy of each note's history in the
profile's data directory (only a `.notekpr-vault` marker file sits in
each vault's folder). It needs it to merge concurrent edits, and it is what
the server accepts — the server never takes plaintext.

## Set it up

1. Install the client and sign in (once per device, see
   [desktop-client.md §1–2](desktop-client.md#1-install--build)):

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

3. Keep it running in the background with a systemd user service:

   ```ini
   # ~/.config/systemd/user/notekpr-sync.service
   [Unit]
   Description=notekpr sync (Markdown folder)
   After=network-online.target

   [Service]
   ExecStart=%h/.local/bin/notekpr sync --dir %h/Notes --plaintext-ok
   Restart=on-failure

   [Install]
   WantedBy=default.target
   ```

   ```sh
   systemctl --user daemon-reload
   systemctl --user enable --now notekpr-sync
   journalctl --user -u notekpr-sync -f     # see what it does
   ```

   It works offline too: edits are kept and uploaded when the connection is
   back.

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
  [desktop-client.md §7](desktop-client.md#7-conflicts-duplicates-deletes-and-renames).
* **Moving, renaming or unmounting the folder** doesn't delete anything: when
  a vault folder (or most of its files) vanishes at once, the daemon warns and
  waits instead of trashing the notes, and it never re-creates a missing mirror
  folder by itself. See
  [desktop-client.md §7](desktop-client.md#7-conflicts-duplicates-deletes-and-renames)
  for confirming a deliberate mass deletion.
* **Quick notes are the folder `Quick notes/`.** Drop a `.md` file there to
  capture a thought (while there's room: 20 unless your server says
  otherwise); move it into a vault's folder to keep it there. It is flat —
  files in folders inside it aren't synced — and deleting a file there
  deletes the quick note for good (they have no trash). See
  [desktop-client.md §4](desktop-client.md#layout).
* **CLI commands while the daemon runs:** reading ones (`cat`, `ls`,
  `search`, …) work and show the daemon's copies; changing ones (`write`,
  `mv`, …) are handed to the daemon, which runs them on its copies (of the
  vaults it keeps) and updates the files. Details:
  [desktop-client.md §3](desktop-client.md#3-everyday-commands).
* **Comments aren't in the files** (they would stop being plain Markdown).
  They stay on the text they are about while you edit it here — your
  changes are merged as small edits, not as a new file — and if you delete
  that text, the comment keeps a quote of it. Read and answer them in the
  web app, or with `notekpr comments …`
  ([desktop-client.md §3](desktop-client.md#3-everyday-commands)).
* **Phones and tablets:** there is no mobile client yet; use the web app.
