# coolpsonotes

A self-hosted, open-source, local-first Markdown knowledge base: Obsidian's
plain-Markdown philosophy and offline access, Notion's friendly interface and
real-time collaboration — with **end-to-end encryption** so the server can't
read anything.

* **Private by design** — notes, file names, folders, attachments and history
  are encrypted on your devices. The server stores ciphertext and coordinates.
* **Collaborative** — share vaults with family or colleagues as editor or
  viewer; edit the same note simultaneously with live cursors. Conflict-free
  merging (CRDTs), also after working offline.
* **Plain Markdown** — wiki links, backlinks, tags, embeds, export to a
  folder of `.md` files at any time.
* **Use your own editor** — the desktop client keeps your vaults as a plain
  folder of Markdown files, synced live and both ways: edit with Obsidian,
  Neovim, VS Code or anything else while the server only stores ciphertext
  ([how](docs/own-editor.md)).
* **Easy for everyone** — sign in with your existing identity provider (OIDC),
  one click to set up encryption, a clean editor with a formatting toolbar.
* **For AI agents too** — give an agent its own identity and keys, share only
  the vaults it needs; it works with notes through the CLI, a folder of
  Markdown files, or a local HTTP gateway.
* **Rust everywhere** — the server, the desktop client and the browser client
  (compiled to WebAssembly) share one core crate for all cryptography and
  syncing. No npm, no third-party JavaScript.

## Features

**Writing**

* Live-preview Markdown editor: formatting shows as you type and the syntax
  hides once the caret leaves the line; also a plain source mode and a reading
  view (`Ctrl+E`).
* A formatting toolbar and keyboard shortcuts (`Ctrl+/` lists them), optional
  Vim keys, light and dark themes.
* Tables you build with Enter and Tab, with aligned columns; task lists you
  can tick in the reading view.
* Images and other files: paste or drop them into a note — they are encrypted,
  uploaded and shown inline.
* Wiki links (`[[Note]]`, `[[Note#Heading]]`) that create the note if it's
  missing; renaming a note updates every link to it.

**Organizing and finding**

* Vaults, folders and a note tree with drag and drop.
* Icons: an emoji for each note and folder, an emoji or a picture for each
  vault.
* Favorites and recently opened notes in the sidebar.
* Quick switcher (`Ctrl+P`), full-text search (`Ctrl+Shift+F`) with tag
  filters, tags, and backlinks and outgoing links beside each note.
* A graph of how your notes link, like Obsidian's (`Ctrl+G`): the whole vault
  with a filter, or the open note and its neighbours beside it. Drag, zoom,
  click a note to open it.
* Quick notes: capture a thought in a second (`Ctrl+Alt+Q`, a capture page
  that works on phones, or `coolpsonotes quick …`), then promote it to a
  regular note or archive it.
* A trash with restore, and the version history of every note with preview
  and restore.

**Working together**

* Share a vault as editor or viewer, or a single note with someone outside
  the vault.
* Edit the same note at the same time and see each other's cursors and who is
  where. Edits merge without conflicts (CRDTs), also after working offline.
* You're told when something is shared with you; owners have an activity log
  per vault.

**Privacy and security**

* End-to-end encrypted: note text, titles, file and folder names,
  attachments, vault names and icons, presence and your settings. The server
  only stores ciphertext and can't read any of it.
* Every note has its own key; removing someone renews the keys, and sharing a
  note never gives away its earlier history.
* The vault's owner signs who belongs to it, and every change is signed by
  its author, so even a compromised server can't add people or forge edits.
* Key fingerprints to check who you share with, and a loud warning when
  someone's key changes.
* New devices are approved from one you already use (with a verification
  code) or unlocked with your recovery key; any browser can be forgotten.
* Sign in with your own identity provider (OIDC: Authelia, Keycloak,
  Authentik, …).

**Offline, and your own tools**

* The browser keeps an encrypted copy of your vaults: they open at once, and
  what you write offline syncs later. Optionally the app starts without the
  server too.
* Desktop/CLI client with a local encrypted copy, live sync, and a two-way
  folder of plain Markdown files for Obsidian, Neovim, VS Code or anything
  else ([how](docs/own-editor.md)).
* The command line does everything: list, read, write, search, move, trash,
  history, quick notes, sharing, import and export — also while its sync
  daemon runs.
* Import folders of Markdown and zip files (an Obsidian vault, say) with their
  attachments; export a vault as plain `.md` files any time.
* Never locked in: an encrypted export plus your recovery key turn back into
  plain files without the server or the apps
  ([recovery](docs/recovery.md)).

**For AI agents**

* Agents get their own identity and keys and see only the vaults you share
  with them. They work through the CLI, a Markdown folder or a local HTTP
  gateway with an audit log ([agents](docs/agents.md)).

**Running it**

* One Rust server with SQLite; Docker Compose with Caddy and automatic HTTPS,
  or Kubernetes.
* Backups, upgrades, Prometheus metrics and an admin page; size limits for
  changes, snapshots and attachments.
* A development mode where anyone can sign in as anyone (one session per
  browser tab) to try sharing and collaboration in one browser.
* No npm and no third-party JavaScript: the browser client is Rust compiled to
  WebAssembly.

## Try it (development mode, no identity provider needed)

You need Rust ≥ 1.94 with the `wasm32-unknown-unknown` target and the
`wasm-bindgen` CLI 0.2.128. On Arch: `pacman -S rust rust-wasm wasm-bindgen go-task`.
See [development dependencies](docs/development.md#development-dependencies)
for everything else (browser tests, Docker, …). No Node.js or npm.

```sh
task setup     # checks the toolchain
task dev       # builds everything and starts a dev server on http://localhost:8080
```

or without go-task:

```sh
cargo build -p coolpsonotes-server
./scripts/build-web.sh
./target/debug/coolpsonotes-server --dev-mode --web-dir dist/web
```

Open <http://localhost:8080>, sign in as `alice`, then open another tab and
sign in as `bob` — every tab keeps its own session, so you can test sharing and
live collaboration between several users in one browser. Or with Docker:

```sh
docker compose -f docker-compose.dev.yml up --build
```

> Development mode lets anyone sign in as anyone. Never expose it.

## Run it for real

```sh
cp .env.example .env         # your domain and OIDC client (Authelia, Keycloak, Authentik, …)
docker compose up -d --build # coolpsonotes + Caddy with automatic HTTPS
```

On a home network without public DNS, or behind a proxy you already run, see
the setups in [docs/operations.md](docs/operations.md#1-choose-a-setup).

See [docs/operations.md](docs/operations.md) for configuration, identity
provider setup, reverse proxies, Kubernetes, backups, upgrades and monitoring.

## Clients

| Client | |
|---|---|
| Browser | served by the server; works on desktop and phones |
| Desktop / CLI (`coolpsonotes`) | offline replica, live sync, optional Markdown mirror, export/import — [docs/desktop-client.md](docs/desktop-client.md) |
| Agents | CLI, Markdown folder or local HTTP gateway — [docs/agents.md](docs/agents.md) |

## Documentation

* [Use your own editor (Obsidian, Neovim, …)](docs/own-editor.md)
* [Getting your notes out, whatever happens (encrypted export, offline recovery)](docs/recovery.md)
* [Architecture and technology decisions](docs/architecture.md)
* [Security model and threat analysis](docs/security.md)
* [Sync protocol and HTTP API](docs/protocol.md)
* [Operations](docs/operations.md) · [Development](docs/development.md)
* [Acceptance criteria and how they are tested](docs/acceptance.md)
* [Working on the code (rules for humans and AI agents)](AGENTS.md) · [TODO and roadmap](TODO.md)

## Status

First release (0.1): everything listed under [Features](#features) is
implemented and covered by tests (unit, server API, CLI and browser
end-to-end). Planned: a native desktop app (around the web UI) and mobile
apps; what's still open is in [TODO.md](TODO.md).

## License

[AGPL-3.0-or-later](LICENSE).
