<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/logo-dark.svg">
    <img src="docs/assets/logo.svg" alt="notekpr" height="80">
  </picture>
</h1>

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
* **Use your own editor** — the CLI keeps your vaults as a plain
  folder of Markdown files, synced live and both ways: edit with Obsidian,
  Neovim, VS Code or anything else while the server only stores ciphertext
  ([how](docs/own-editor.md)).
* **Easy for everyone** — sign in with your existing identity provider (OIDC),
  one click to set up encryption, a clean editor with a formatting toolbar.
* **For AI agents too** — give an agent its own identity and keys, share only
  the vaults it needs; it works with notes through the CLI, a folder of
  Markdown files, or a local HTTP gateway.
* **Rust everywhere** — the server, the CLI and the browser client
  (compiled to WebAssembly) share one core crate for all cryptography and
  syncing. No npm, no third-party JavaScript.

## Features

The short version; [every feature](docs/features.md), with its shortcuts,
is in the docs.

* **[Writing](docs/features.md#writing)** — a live-preview Markdown editor
  with a toolbar, tables, task lists, code blocks, callouts, colors,
  images, video and audio; optional Vim keys.
* **[Drawings](docs/features.md#drawings)** — a whiteboard among your
  notes, drawn together live and shown in any note; compatible with
  Excalidraw: copy and paste, its files and Obsidian's.
* **[Organizing and finding](docs/features.md#organizing-and-finding)** —
  folders with drag and drop, icons, a quick switcher, full-text search,
  backlinks, a graph, quick notes, a trash and every note's history.
* **[Working together](docs/features.md#working-together)** — share a vault
  or a single note, edit together with live cursors, comment and @mention.
* **[Privacy and security](docs/features.md#privacy-and-security)** — a key
  for every note, signed membership and edits, new devices approved from one
  you already use.
* **[Offline, and your own tools](docs/features.md#offline-and-your-own-tools)**
  — works offline; the CLI does everything and keeps your vaults as a
  folder of Markdown files; import and export.
* **[For AI agents](docs/features.md#for-ai-agents)** — their own identity,
  only the vaults you share, through the CLI, files or an HTTP gateway.
* **[Running it](docs/features.md#running-it)** — one Rust server with
  SQLite, attachments in S3 or a folder, an admin page, limits and metrics.

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
cargo build -p notekpr-server
./scripts/build-web.sh
./target/debug/notekpr-server --dev-mode --web-dir dist/web
```

Open <http://localhost:8080>, sign in as `alice`, then open another tab and
sign in as `bob` — every tab keeps its own session, so you can test sharing and
live collaboration between several users in one browser.

To look around with data already there, `task demo` starts a separate demo
server on <http://localhost:8099>: five users with their own and shared
vaults, notes shared on their own, and a vault of linked notes to see the
graph ([details](docs/development.md#a-demo-with-data)).

Or with Docker:

```sh
docker compose -f docker-compose.dev.yml up --build
```

> Development mode lets anyone sign in as anyone. Never expose it.

## Run it for real

The server is published as a container image for amd64 and arm64,
`ghcr.io/coolapso/notekpr`, tagged with each release's version and `latest`.
With Docker Compose:

```sh
git clone https://github.com/coolapso/notekpr && cd notekpr   # or download just docker-compose.yml
export NOTEKPR_PUBLIC_URL=https://notes.example.com
export NOTEKPR_OIDC_ISSUER=https://auth.example.com   # Authelia, Keycloak, Authentik, …
export NOTEKPR_OIDC_CLIENT_ID=notekpr
export NOTEKPR_OIDC_CLIENT_SECRET=…                   # from wherever you keep secrets
docker compose up -d                                  # notekpr on 127.0.0.1:8080
```

Put the reverse proxy you use in front of it for HTTPS: notekpr serves
plain HTTP and leaves TLS to the proxy, by design.

| Setup | Files | How |
|---|---|---|
| Docker Compose | [`docker-compose.yml`](docker-compose.yml) | [operations](docs/operations.md#docker-compose) |
| Kubernetes | [`deploy/kubernetes/`](deploy/kubernetes/): the server, a nightly backup, a public demo | [operations](docs/operations.md#kubernetes) |
| From source | — | [operations](docs/operations.md#from-source) |
| A reverse proxy, if you have none (optional) | [`deploy/examples/`](deploy/examples/): Caddy with automatic HTTPS, Traefik on Kubernetes; nginx in the docs | [operations](docs/operations.md#examples) |

### How big a server?

A small one: ¼ core and 128 MB serve 100 people online at once, and a
1 vCPU / 1 GB VPS is plenty for 1,000. Memory is what runs out first:
about a quarter of a megabyte per person online. Measured with the
[load test](docs/load-test.md); the numbers, and how to measure your own
server, are in [operations](docs/operations.md#how-big-a-machine).

See [docs/operations.md](docs/operations.md) for configuration, identity
provider setup, reverse proxies, Kubernetes, backups, upgrades and monitoring.

## Clients

| Client | |
|---|---|
| Browser | served by the server; works on desktop and phones |
| CLI (`notekpr`) | offline replica, live sync, optional Markdown mirror, export/import — [docs/cli.md](docs/cli.md) |
| Agents | CLI, Markdown folder or local HTTP gateway — [docs/agents.md](docs/agents.md) |

## Documentation

* [Every feature](docs/features.md)
* [Who can do what in a vault (owners, admins, editors, viewers)](docs/roles.md)
* [Use your own editor (Obsidian, Neovim, …)](docs/own-editor.md)
* [Getting your notes out, whatever happens (encrypted export, offline recovery)](docs/recovery.md)
* [Architecture and technology decisions](docs/architecture.md)
* [Security model and threat analysis](docs/security.md)
* [Sync protocol and HTTP API](docs/protocol.md)
* [Operations](docs/operations.md) · [Development](docs/development.md)
* [Acceptance criteria and how they are tested](docs/acceptance.md) · [Load test with a live view](docs/load-test.md)
* [Working on the code (rules for humans and AI agents)](AGENTS.md)

## License

[AGPL-3.0-or-later](LICENSE).

If you like this project and want to support / contribute in a different way you can always [:heart: Sponsor Me](https://github.com/sponsors/coolapso) or

<a href="https://www.buymeacoffee.com/coolapso" target="_blank">
  <img src="https://cdn.buymeacoffee.com/buttons/default-yellow.png" alt="Buy Me A Coffee" style="height: 51px !important;width: 217px !important;" />
</a>
