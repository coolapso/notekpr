# Features

Everything notekpr does, area by area, with its shortcuts (`Ctrl` is `⌘` on
a Mac). The [README](../README.md) has the short version.

## Writing

* Live-preview Markdown editor: formatting shows as you type and the syntax
  hides once the caret leaves the line; also a plain source mode and a reading
  view (`Ctrl+E`).
* A formatting toolbar and keyboard shortcuts (`Ctrl+/` lists them; `⌘`
  instead of `Ctrl` on a Mac), optional Vim keys, light, dark or system
  theme.
* Notes as narrow or wide as you like: a slider in Settings sets the
  default (for this browser, or the same on all your devices), and a note's
  ⋯ menu gives it a width of its own, only for you.
* Tables you build with Enter and Tab, with aligned columns; one wider than
  the screen scrolls sideways instead of squeezing its columns. Task lists
  you can tick in the editor and the reading view.
* Code blocks with syntax highlighting for some 35 languages, line numbers,
  indentation guides and a copy button. Options after the language: a file
  name (`` ```bash title=deploy.sh ``), highlighted lines (`hl=2,4-6`), no
  line numbers (`ln=false`) or indentation guides (`vl=false`), long lines
  that don't wrap but scroll sideways (`wrap=false`) — they stay plain
  Markdown. A new block from the toolbar or `` Ctrl+Shift+` ``; Enter (or
  pasting) after an opening `` ``` `` closes the block.
* Images and other files: paste or drop them into a note — they are encrypted,
  uploaded and shown inline; videos and audio (`![[clip.mp4]]`) play there,
  in the editor and the reading view.
* Callouts as GitHub and Obsidian write them: `> [!tip]`, `> [!warning] A
  title`, `> [!faq]-` for one that folds (`+` open). Types are
  case-insensitive — note, abstract, info, todo, tip, important, success,
  question, warning, caution, failure, danger, bug, example, quote and
  Obsidian's aliases; others look like a note. Each has its color and icon,
  in the editor and the reading view; callouts can nest. The toolbar starts
  one, or gives the one the caret is in another type.
* Text and background colors from the toolbar, in a palette that reads well
  in the light and the dark theme. They are saved as plain HTML
  (`<span style="color: …">`), so Obsidian, Typora, VS Code and other
  Markdown editors show them too — and colors written there (`<font color>`,
  colored highlights, any CSS color) show here in the nearest palette color.
* HTML inside notes for what Markdown can't do: fold-out sections
  (`<details>`), keys (`<kbd>`), sub- and superscript, centered text,
  sized images, tables with merged cells. Only formatting is kept — no
  scripts, styles, frames, forms or embeds; external images aren't loaded.
  The editor shows HTML rendered, like Markdown: a block until you click
  into it, inline tags (`<kbd>`, `<i>`, …) until the caret is on their line.
* Dates: type `@today`, `@tomorrow` or `@yesterday` (or pick from the menu
  `@` opens) and the note gets that day's date, `@2026-10-23` — plain text
  any editor reads. It shows as Today, Tomorrow or Yesterday while it is
  one, and as the date after that. The CLI and agents get the same
  (`notekpr write`, the gateway).
* Wiki links (`[[Note]]`, `[[Note#Heading]]`, `[[Note|alias]]`) that create
  the note if it's missing; renaming a note updates every link to it.

## Drawings

* A drawing editor with Excalidraw's hand-drawn look, written in Rust: the
  same drawing looks the same as on excalidraw.com, wobbles and all.
  Rectangles, diamonds, ellipses, arrows and lines (dragged or point by
  point), the pen, texts, pictures (`9`, or pasted or dropped), sticky
  notes (`N`), frames (`F`), the eraser and the laser pointer (`K`);
  colours (picks and Excalidraw's whole palette), fills (hachure,
  cross-hatch, solid, zigzag), stroke width and style, sloppiness, rounded
  edges, arrowheads, arrow types (sharp, curved, elbow), every font
  Excalidraw has, opacity; select, move, resize, turn, group, lock, align,
  distribute, layers, duplicate (`Alt`+drag copies); undo and redo of your
  own changes; Excalidraw's keyboard shortcuts (`R`, `O`, `A`, `T`, `Q` to
  keep a tool, …). Two fingers pinch to zoom on touch screens.
* Text inside shapes and on arrows and lines (double-click; Enter in a
  shape), wrapped and centred, the shape growing to hold it; an arrow's or
  line's label slides along it, in a gap of its line (Excalidraw labels
  arrows only: it shows a line's label where it is, the line through it).
  Arrows stay attached to shapes and follow them — dropped near a shape
  they attach to its outline (highlighted while dragged), held still over
  it for a moment they point inside; elbow arrows route themselves at right
  angles around what they connect (drag a segment to hold it, double-click
  to let go).
* A selected line or arrow shows its points: drag them, or the handle
  halfway along to bend it; Ctrl+double-click or Enter edits every point — Shift selects several, Delete removes them;
  ends that meet close a line.
* Sticky notes whose text shrinks to fit, with the day they were made at
  their foot; frames that hold what you draw or drop in them, move and
  clip it, renamed by double-clicking their name. A frame selected on its
  own is exported alone, as in Excalidraw: its PNG or SVG (or a copy) is
  just what is in the frame, cut to it.
* The grid (`Ctrl+'`, part of the drawing) and snapping to other shapes'
  edges, centres and equal gaps (`Alt+S`, your own setting; `Ctrl` while
  dragging turns it off for that drag).
* Pictures are files of the vault (`attachments/`), stored and synced once,
  encrypted; drawing files written out hold them inside, as Excalidraw's do.
  Enter or a double click crops a picture.
* Links on elements (`Ctrl+K`): a web address, or `[[a note]]` of the
  vault, opened from a button by the element.
* Saved shapes, as Excalidraw's library: keep shapes from a drawing (the
  button with the overlapping shapes) and drop them into any other; yours,
  in your Personal storage, synced and encrypted between your devices;
  named after their first text, renamed from the pencil; Shift-click (a
  range) or Ctrl-click picks several, to put in side by side, export or
  remove at once (`Esc` lets them go); `.excalidrawlib` files imported
  (also dropped on a drawing) and exported (`notekpr shapes
  ls|import|export|rename|rm`); in the Markdown folder as
  `Saved shapes.excalidrawlib`, edits merged shape by shape; agents' gateway
  reads and adds them.
* Drawn together, live, with the vault's members: their pointers with
  their names, what they selected, their laser, and who is on the drawing
  in its header; merged when someone drew offline. A drawing's history
  keeps versions, shown as pictures, to restore or download.
* Drawings live in vaults, among the notes, in the file tree, and are
  renamed there or by their name at the top of the drawing; the vault's
  *Drawings* page shows them all as pictures ("3 of 10 drawings" when the
  server limits how many a vault holds). `![[Board.drawing]]` in a note shows the drawing, live, and the link
  while the cursor is on it (the drawing's ⤓ menu copies it); notes from
  Obsidian that show `![[Board.excalidraw]]` find the imported drawing.
* Works with Excalidraw's files: copy and paste between notekpr and
  excalidraw.com or Obsidian; importing a folder or Obsidian vault turns
  `.excalidraw` and Obsidian's `.excalidraw.md` drawings into drawings.
  Those synced as they are from a Markdown folder (so Obsidian keeps
  working with them) show as drawings, view-only, when opened and where a
  note embeds them, pictures of the vault included; "Make it a drawing"
  makes one notekpr's, to edit (asked first: the file goes to the trash); and
  files dropped on a drawing come in; export a drawing as a `.drawing`
  file, for Excalidraw (`.excalidraw`), for Obsidian (`.excalidraw.md`),
  SVG or PNG — the drawing kept inside, so they open again in notekpr and
  Excalidraw, whose own PNG and SVG exports with the scene inside open as
  drawings too (dropped on one, or `notekpr write`) — or copy it (or what
  is selected) as a PNG (`Shift+Alt+C`) or SVG. The Markdown folder keeps drawings as `.drawing` files (Excalidraw's
  JSON inside), merged element by element when edited there.
* The text in drawings is found by search, and agents read it (and draw:
  Excalidraw's JSON sent to a `.drawing` path makes a drawing).
* Each note's menu downloads it as Markdown or copies a `[[link]]` to it.

## Organizing and finding

* Vaults, folders and a note tree with drag and drop: drop onto a folder to
  move something into it, or between two entries to put it there (a line
  shows where; folders stay above notes, and *Sort A–Z* undoes it). Everyone
  in the vault sees the same order. A new note with `Ctrl+Alt+N`. Each vault opens on a home page with its recently edited
  notes and tags; its owner can archive it while it isn't needed, or delete
  it.
* Icons: an emoji for each note and folder, an emoji or a picture for each
  vault (emojis also from the command line, pictures in the browser).
* Favorites and recently opened notes in the sidebar.
* Quick switcher (`Ctrl+P`) that creates the note when nothing matches;
  full-text search (`Ctrl+Shift+F`) with `tag:` (or `#tag`), `folder:` (or
  `path:`), `title:` and `vault:` filters and `"phrases"`.
* A side panel beside each note (`Ctrl+\`): backlinks, outgoing links,
  unresolved links you click to create, the outline and tags.
* A graph of how your notes link, like Obsidian's (`Ctrl+G`): the whole vault
  with a filter, or the open note and its neighbours beside it. Drawings are
  in it too (squares), linked with the notes that show or link them and the
  notes their elements link to. Drag, zoom, click a note to open it.
* Quick notes: capture a thought in a second (`Alt+Q`, a capture page
  that works on phones, `notekpr quick …`, or a file dropped into the
  Markdown folder's `Quick notes/`). They are yours alone, in no vault, the
  same from every vault and device: a short list (20 unless the
  administrator says otherwise) to sort out by moving each to a vault, or
  deleting it for good.
* Move a note, or a folder with everything in it, to another vault, with
  the attachments its notes link to (`Move to vault…` in the file tree's
  menu or the note's ⋯ menu, `notekpr mv … --to-vault`).
* A trash with restore — even emptying it can be undone for a while (30
  days unless the server is set otherwise), from the trash or
  `notekpr trash undo` — and the version history of every note with
  preview and restore.

## Working together

* Share a vault as owner, admin, editor or viewer, or a single note with
  someone outside the vault. A vault can have several owners, all equal, and
  always keeps one: the last owner can't leave, nor delete their account
  while others use the vault, so it never goes with one person — and a vault
  others use suggests a second owner (a hint you can hide). Editors move
  notes to the trash; only owners and the vault's admins delete for good,
  and see the vault's activity log ([who can do what](roles.md)).
  Leave a vault shared with you, or remove a shared note from your list.
* Edit the same note at the same time with live cursors, and see who else is
  on the note you're on. Edits merge without conflicts (CRDTs), also after
  working offline.
* Comments on the text, like in Notion: select some text and comment
  (`Alt+M`), in the editor or the reading view; the text gets a highlight,
  and clicking it opens the thread to reply, resolve, edit or delete.
  Viewers can comment too (the vault's owners can turn that off), comments
  follow the text through edits — also edits from your own editor — and a
  note can be shared with its comments. Also from the command line
  (`notekpr comments`, `notekpr shared comments`) and for AI agents, and in
  exports (`_comments/`).
* @mentions: type `@` in a comment to mention a member of the vault. Their
  devices find it — the server never learns who is mentioned — and show it
  in the sidebar's mentions inbox, with a message when it arrives and, if
  they like, a system notification. Read on one device, read on all;
  `notekpr mentions` lists them.
* You're told when a note is shared with you; owners and admins have an
  activity log per vault.

## Privacy and security

* End-to-end encrypted: note text, titles, file and folder names,
  attachments, vault names and icons, presence and your settings. The server
  only stores ciphertext and can't read any of it.
* Every note has its own key; removing someone renews the keys in the same
  step, and when someone leaves on their own the owners' apps renew them
  next time they're online. Sharing a note never gives away its earlier
  history. Owners can also renew a vault's key by hand.
* The vault's owners sign who belongs to it, and every change is signed by
  its author, so even a compromised server can't add people or forge edits.
* Key fingerprints to check who you share with, and a loud warning when
  someone's key changes.
* New devices are approved from one you already use (with a verification
  code) or unlocked with your recovery key; any browser can be forgotten.
  Approving the command line's sign-in shows when, and from which address,
  it was asked for.
  See where you're signed in and sign out remotely, and make a new recovery
  key whenever you need one.
* Sign in with your own identity provider (OIDC: Authelia, Keycloak,
  Authentik, …).

## Offline, and your own tools

* The browser keeps an encrypted copy of your vaults: they open at once, and
  what you write offline syncs later. Optionally the app starts without the
  server too.
* A command-line client (CLI) with a local encrypted copy, live sync, and a
  two-way folder of plain Markdown files for Obsidian, Neovim, VS Code or
  anything else ([how](own-editor.md)).
* The command line does everything: list, read, write, search, move, icons,
  trash, history, quick notes, sharing, archiving, key rotation, devices and
  recovery keys, import and export — also while its sync daemon runs. Shell
  completions for bash, zsh, fish, PowerShell and elvish.
* `notekpr setup` sets a computer up in one go: signs in, takes the folder
  for your Markdown files (an Obsidian vault already there comes along) and
  keeps it in sync in the background.
* Import folders of Markdown and zip files (an Obsidian vault, say) with their
  attachments; export a vault as plain `.md` files any time.
* Moving to another server: export the vaults you choose and your quick
  notes as one `.zip` (Settings → Account → Data), and import it there — the
  vaults come back with their folders, files and icons; the new server
  starts fresh, without history, comments or shares. Then delete your
  account on the old one, if you like (also from the command line).
* Never locked in: an encrypted export plus your recovery key turn back into
  plain files without the server or the apps
  ([recovery](recovery.md)).

## For AI agents

* Agents get their own identity and keys and see only the vaults you share
  with them. They work through the CLI, a Markdown folder or a local HTTP
  gateway with an audit log ([agents](agents.md)).
* Create agents, give them new secrets, revoke or delete them in Settings or
  from the CLI.

## Running it

* One Rust server with SQLite; Docker Compose or Kubernetes, behind the
  reverse proxy you choose for TLS (optional examples for Caddy, Traefik and
  nginx).
* Attachments in a folder, in S3 or anything compatible (several servers can
  share a bucket, each under its own prefix), or inside the database for
  one-file backups; existing ones move there by themselves.
* Updates without fuss: the server tells connected apps it's restarting;
  they keep working and are back about a second after it is.
* Backups, upgrades, Prometheus metrics, and an admin page for accounts
  (make admins, disable accounts), server statistics and the server's
  version — never content;
  size limits for changes, snapshots and attachments.
* Instance settings that change without a restart: limits on accounts,
  agents and vaults, vaults, agents and quick notes per person (with each
  person's own allowance), drawings per vault (with a vault's own), a
  read-only mode the apps ride out — edits are kept and sent
  when it ends —, a size for the whole server, database and files (at it,
  every vault takes no new content until something is deleted for good or
  the size is raised: the room a family or a team shares on its own
  server), and the
  oldest app version it accepts: older apps are told to update and keep
  their unsent edits.
* Storage per vault: a size each vault may take, by default and for one
  vault. A full vault takes no new content — its members' edits wait in
  their apps and go out once it has room — while every other vault goes
  on; emptying its trash makes room at once. Members see a vault's id and
  how much of its room it uses in its settings, and give the id to ask for
  more; the admin page lists vaults by size with their ids and owners, never
  their names. Each person's Personal storage (quick notes, saved shapes)
  has a size of its own, by default and for one person.
  From the admin page, or from scripts (a billing system, say) as admin
  clients: each with its own scopes and tokens, rotated and revoked without
  a restart, and never reaching a vault. An instance can leave everything to
  them and have no administrators among its people
  ([operations](operations.md#admin-clients-scripts-and-services)).
* A load test you can watch (`task load`): a crowd of simulated people —
  real clients with real encryption — growing level by level to a thousand,
  against the server image with CPU and memory limits, an outage in every
  level, a GAME OVER when it can't keep up, and a check that nothing was
  lost; choose the run on its start screen, with people busier than anyone
  or as busy as real ones, against a server here or one elsewhere
  ([load test](load-test.md)).
* A development mode where anyone can sign in as anyone (one session per
  browser tab) to try sharing and collaboration in one browser, and a demo
  with users, shared vaults and notes ready to explore (`task demo`).
* No npm and no third-party JavaScript: the browser client is Rust compiled to
  WebAssembly.
