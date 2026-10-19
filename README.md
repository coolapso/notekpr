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
  folder of `.md` files at any time. The desktop client can mirror vaults as
  real files you edit with Neovim or Obsidian.
* **Easy for everyone** — sign in with your existing identity provider (OIDC),
  one click to set up encryption, a clean editor with a formatting toolbar.
* **For AI agents too** — give an agent its own identity and keys, share only
  the vaults it needs, and connect it through a local HTTP gateway or MCP.
* **Rust everywhere** — the server, the desktop client and the browser client
  (compiled to WebAssembly) share one core crate for all cryptography and
  syncing. No npm, no third-party JavaScript.

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
cp .env.example .env     # your domain and OIDC client (Authelia, Keycloak, Authentik, …)
docker compose up -d     # coolpsonotes + Caddy with automatic HTTPS
```

See [docs/operations.md](docs/operations.md) for configuration, identity
provider setup, reverse proxies, Kubernetes, backups, upgrades and monitoring.

## Clients

| Client | |
|---|---|
| Browser | served by the server; works on desktop and phones |
| Desktop / CLI (`coolpsonotes`) | offline replica, live sync, optional Markdown mirror, export/import — [docs/desktop-client.md](docs/desktop-client.md) |
| Agents | gateway HTTP API and MCP server — [docs/agents.md](docs/agents.md) |

## Documentation

* [Architecture and technology decisions](docs/architecture.md)
* [Security model and threat analysis](docs/security.md)
* [Sync protocol and HTTP API](docs/protocol.md)
* [Operations](docs/operations.md) · [Development](docs/development.md)
* [Acceptance criteria and how they are tested](docs/acceptance.md)
* [Working on the code (rules for humans and AI agents)](AGENTS.md)

## Status

First release (0.1). Implemented: server, browser client, end-to-end
encryption with device enrollment, recovery and key rotation, vault sharing
with roles, sharing single notes with people outside a vault, live
collaboration, offline sync, revision history, trash,
search/backlinks/tags, import/export, agent identities, desktop/CLI client with
gateway and MCP, Docker/Kubernetes packaging. Planned: native desktop GUI shell,
mobile apps, sharing folders, signed updates
([roadmap](docs/architecture.md#9-milestones)).

## License

[AGPL-3.0-or-later](LICENSE).
