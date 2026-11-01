# My notes

notekpr keeps this folder in sync: what you change here is encrypted and
uploaded within moments, and other people's changes appear here too. Work
with the files as usual, following these rules.

- Each folder at the top is a vault, with a `.notekpr-vault` file in it:
  leave that file alone. A new folder at the top is not synced: new notes go
  inside a vault's folder.
- Notes are Markdown files (`.md`, UTF-8). `[[Wiki links]]`, `![[embeds]]`
  and `#tags` work as in Obsidian; a link only finds notes in its own vault.
- Deleting a file moves the note to its vault's trash (`notekpr trash`
  brings it back). In `Quick notes/` it deletes the note for good: never
  delete files there.
- `Quick notes/` has no folders: files in folders inside it aren't synced.
- `.drawing` files are drawings (JSON): don't edit them by hand.
- Renaming or moving a file keeps the note and its history. Deleting many
  files at once (more than 10, and more than half of a vault's) is held as a
  likely mistake and not synced: delete fewer, or ask first.
- `… (conflict <date>).md` and `… (recovered).md` are copies kept on
  purpose, not clutter: ask before deleting them.
- Hidden files and folders (`.obsidian`, `.git`, …) and editor backups are
  never synced.
- What the files don't hold, the `notekpr` command does (`notekpr --help`,
  `notekpr <command> --help`): comments (`notekpr comments list <vault>
  <note>`), a note's history (`notekpr history <vault> <note>`), the trash
  (`notekpr trash list <vault>`), searching every vault (`notekpr search
  <words>`), sharing. Ask before sharing, unsharing or emptying the trash.
