---
name: notekpr
description: The user's notes in notekpr (end-to-end encrypted Markdown notes, kept on this computer as a folder of Markdown files). Use when the user asks about their notes, or to find, read, write or change one.
---

# The user's notes (notekpr)

The notes are Markdown files in a folder notekpr keeps in sync:
`notekpr status` names it (the `Mirror:` line; `--json`: `mirror_dir`). What
you change there is encrypted and uploaded within moments, and other
people's changes appear there too. Read, search and edit the files with
your usual tools, following these rules.

- Each folder at the folder's top is a vault, with a `.notekpr-vault` file
  in it: leave that file alone. A new folder at the top is not synced: new
  notes go inside a vault's folder.
- Notes are Markdown (`.md`, UTF-8). `[[Wiki links]]`, `![[embeds]]` and
  `#tags` work as in Obsidian; a link only finds notes in its own vault.
- Deleting a file moves the note to its vault's trash. In `Quick notes/` it
  deletes the note for good: never delete files there. `Quick notes/` has no
  folders: files in folders inside it aren't synced.
- `.drawing` files are drawings (JSON): don't edit them by hand.
- Renaming or moving a file keeps the note and its history. Deleting many
  files at once (more than 10, and more than half of a vault's) is held as a
  likely mistake and not synced: delete fewer, or ask first.
- `… (conflict <date>).md` and `… (recovered).md` are copies kept on
  purpose: ask before deleting them.
- Hidden files and folders and editor backups are never synced.

## What the files don't hold: the `notekpr` command

Vaults are named by their name or id, notes by their path in the vault
(`.md` optional). `notekpr <command> --help` has the details.

- Search every vault: `notekpr search <words>` (`"phrases"`, `tag:x`,
  `folder:x`, `title:x`, `vault:x`); `notekpr backlinks <vault> <note>`.
- Comments: `notekpr comments list <vault> <note>`,
  `notekpr comments add <vault> <note> --on "<text it is about>" <comment>`,
  `notekpr comments reply <vault> <note> <id> <reply>`.
- History: `notekpr history <vault> <note>`, then
  `notekpr history show <vault> <note> <rev>`.
- The trash: `notekpr trash list <vault>`, `notekpr trash restore <vault> <note>`.
- Quick notes: `notekpr quick <text>` captures one.
- Several accounts are profiles: `notekpr profiles` lists them; add
  `--profile <name>` to use one.

Ask before sharing or unsharing (`notekpr vault share`, `share-note`),
emptying the trash, moving notes to another vault, or deleting notes.
