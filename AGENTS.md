# AGENTS.md

Guide for anyone — AI coding agents and humans — changing this repository.
Read this first; it is short on purpose and links to the detailed docs.

> Not to be confused with [docs/agents.md](docs/agents.md), which documents
> the *product feature* that lets AI agents use notekpr as a knowledge base
> (CLI, Markdown folder, local HTTP gateway).

## What this is

notekpr is a self-hosted, local-first, **end-to-end encrypted**
Markdown notes platform with real-time collaboration, written entirely in
Rust: a zero-knowledge sync server, a browser client compiled to WebAssembly,
and a command-line client, the CLI (sync daemon, Markdown mirror, agent
gateway). A desktop app is planned; it is a separate client, not the CLI.
The original requirements are in [spec.md](spec.md).

| Doc | Read it when you… |
|---|---|
| [docs/architecture.md](docs/architecture.md) | need the big picture and why things are the way they are |
| [docs/protocol.md](docs/protocol.md) | touch the HTTP API, WebSocket messages or ciphertext formats |
| [docs/security.md](docs/security.md) | touch keys, crypto, authorization, logging or anything a server could learn |
| [docs/development.md](docs/development.md) | set up a machine, run things locally, run tests |
| [docs/acceptance.md](docs/acceptance.md) | want to know which test proves which requirement |
| [docs/cli.md](docs/cli.md), [docs/operations.md](docs/operations.md) | change the CLI or deployment/ops behaviour |
| [docs/load-test.md](docs/load-test.md) | run or change the load test and its live view |

Known work, review findings and decisions are tracked in a TODO kept outside
the repository, in the maintainer's own notes. Its path is in
`AGENTS.local.md` (git-ignored, one per machine) as `TODO: <path>`. If that
file or the line is missing, ask the person you work for where the TODO is
and write it there. Check the TODO before starting something, and keep it
current.

## Hard rules

Breaking one of these is a bug, even if everything compiles and tests pass.

1. **The server never sees plaintext.** Note text, titles, paths, file names,
   attachments, vault names and presence are encrypted on clients. If a
   feature seems to need plaintext on the server, it belongs in a client.
2. **All cryptography lives in `crates/core`** and has unit tests. Clients
   call it; they never hand-roll crypto or invent ciphertext formats.
3. **Wire formats are append-only.** Never renumber or reuse `Purpose`
   values (`crates/core/src/crypto.rs`) or change the envelope layout, AAD,
   HKDF labels or HPKE info strings — existing data would become unreadable.
   Protocol changes are additive under `/api/v1` (new optional fields use
   `#[serde(default)]`).
4. **No npm, Node.js or JavaScript packages. Ever.** The web UI is Leptos
   compiled to wasm32 and built with cargo + the `wasm-bindgen` CLI only. The
   few static files in `crates/web/static` are hand-written.
5. **Never log secrets or content:** no tokens, query strings, note content,
   titles, names or paths — on the server, in clients or in audit logs.
6. **Don't leak existence.** Vaults or notes the caller can't access return
   `404`, never `403`. People a single note is shared with get only that
   note's content under its own key — never the vault index, other notes,
   attachments, members, or the note's history from before it was shared.
7. **Keep development mode working** (`--dev-mode`: sign in as any user name,
   one session per browser tab, CLI `--profile` + `--dev-user`). It is how
   multi-user features get tested. It must stay impossible to enable by
   accident in production.
8. **Migrations: data survives.** Since the data freeze (0.6.0),
   `crates/server/migrations/0001_init.sql` is the starting schema. Never
   edit an applied migration: every schema change is a new numbered file
   that upgrades the database in place (where SQLite can't change something
   in place, copy the table). From 1.0.0 on, every migration also works with
   the previous release (add, start using, drop a release later).

## Layout

```
crates/core     crypto.rs (envelopes, HPKE, signatures) · comments.rs (signed comment
                records, folding into threads, anchors) · keys.rs (account/device/
                recovery/vault/document keys) · engine.rs (VaultReplica: apply,
                encrypt, defer, snapshot; sharing helpers) · doc.rs (yrs CRDT index
                + notes) · protocol.rs (every API/WS type) · memberlog.rs (owner-signed
                membership log) · recover.rs (offline export decryption) · order.rs
                (the file tree's order: folders first, places set by drag and
                drop, A–Z) · moving.rs (a note or folder to another vault: what
                goes, where, what stays) · version.rs (release versions, for apps
                too old for a server) · account_export.rs (vaults and quick notes
                as one Markdown .zip, to move servers) · markdown, search, paths,
                textdiff
crates/sync     the client sync engine both clients use (sans-IO): outbox (merge, hold,
                batch pushes, acks and refusals), rollback recovery, edit splitting ·
                members.rs (membership log checks, whom to trust) · snapshot.rs (when
                a snapshot may be stored)
crates/server   axum + SQLite. routes/ (REST) · ws.rs (WebSocket, subscriptions) ·
                sync.rs (encrypted log, snapshots, key checks) · access.rs (who may
                do what) · repo/ (all other SQL) · hub.rs (EventBus: live events) ·
                blobs.rs (attachments: a folder, S3 or the database) · instance.rs (runtime settings: limits,
                read-only mode) · apps.rs (apps too old for the server:
                OLDEST_APP_VERSION) · admin_clients.rs (scripts administering the
                instance: scopes, tokens) · auth.rs (sessions; Admin: admins and
                admin clients), oidc.rs · migrations/
crates/editor   the live-preview Markdown editor. Pure model (native tests): text.rs
                (source + line index) · change.rs (ChangeSet: compose, transform,
                map) · state.rs (EditorState, Transaction) · decorations.rs
                (incremental block classification) · syntax.rs · code.rs (code
                block options, syntax highlighting) · html.rs (HTML in notes:
                the allow list, HTML blocks) · commands.rs · vim.rs.
                DOM view: view/ (contenteditable, line patches, IME, native edits)
crates/web      Leptos CSR → wasm32. state/ (Core + Ui in mod.rs; session, vaults,
                members, shares, sync, presence, prefs, nav, offline, comments) · cache.rs
                (IndexedDB copy) · static/sw.js (service worker, only with offline
                use on) · actions.rs (user operations; NoteHost = the editor's Host) ·
                editor.rs (re-exports crates/editor) · views/ · render.rs (reading view)
crates/cli      `notekpr` CLI: session.rs + livesync.rs (sync), mirror.rs
                (Markdown files), gateway.rs, ops.rs, note_shares.rs, setup.rs
                (`notekpr setup`: sign-in, the folder, existing notes moved in)
crates/load     load test (`task load`): simulated people = real clients (the CLI's
                library) against the server image with CPU/memory limits; crash,
                check, report. load-events: what it streams; load-ui: its live
                dashboard (wasm32, canvas; scripts/build-load-ui.sh → dist/load)
tests/e2e       browser tests: run.sh (entry point) → suite.py, cross_client.py, drawings.py;
                perf.py (typing latency) · wd.py (stdlib WebDriver)
scripts/        build-web.sh, build-load-ui.sh (wasm builds) · demo.py (task demo),
                demo-files/ (what it can't make, e.g. a video) · logo.py (the logo
                and icon in docs/assets, by hand) · test-s3.sh (task test:s3: the
                S3 file store against Garage in Docker)
packaging/      the CLI's release builds (GoReleaser, .goreleaser.yaml): Dockerfile (the
                image it runs in) · run.sh (a command in that image) · completions.sh ·
                systemd/ (the sync user unit); Linux only for now: .tar.gz, .deb, .rpm, AUR
fuzz/           fuzz targets, a separate workspace (fuzz/run.sh, task fuzz)
vendor/yrs      yrs 0.28 with fixes for panics found by fuzzing, via [patch.crates-io];
                the changes are in vendor/yrs/NOTEKPR.md — re-apply on upgrade
vendor/openidconnect  openidconnect 4.0.1 on p256/p384 0.14 (a crypto-bigint advisory),
                via [patch.crates-io]; drop it once upstream moves (its NOTEKPR.md)
site/           project website: static HTML + CSS and a little hand-written JS
                (site.js, optional), no build step, no third-party requests
                (fonts, scripts, analytics) — keep it that way; screenshots in img/
deploy/         Kubernetes manifests, Dex (dev OIDC), examples/ (optional reverse
                proxies: Caddy, Traefik — never required, TLS is the operator's choice)
(root)          Dockerfile, docker-compose.yml, docker-compose.dev.yml, Taskfile.yml,
                .goreleaser.yaml
```

## Commands

Requires Rust ≥ 1.94 with the `wasm32-unknown-unknown` target and the
`wasm-bindgen` CLI **exactly** matching the crate pin in
`crates/web/Cargo.toml` (currently 0.2.128). The same pin is in
`crates/editor` and `crates/load-ui` and the Dockerfile's
`WASM_BINDGEN_VERSION`: upgrade them all together with the CLI.
Distribution packages are fine; rustup is not required. `task setup` checks
everything.

```sh
task dev                         # build + run a dev-mode server on http://localhost:8080
task demo                        # a dev-mode server with demo users, vaults and shared notes (:8099)
task load                        # load test with a live view (:8098): USERS= DURATION= CPUS= MEMORY=
task web:watch                   # rebuild the web client on change (then reload the browser)
task cli -- --profile alice ls Family   # run the CLI;  task cli:login DEV_USER=alice
task fmt / task lint             # rustfmt; clippy with warnings as errors
task test                        # all Rust tests
task test:s3                     # attachments in S3-compatible storage (Garage in Docker)
task test:e2e -- -k sharing      # browser tests (chromium + chromedriver + python3)
task test:editor                 # the editor's DOM view in headless Chromium (wasm-bindgen-test)
task build:clients               # the CLI's release files and packages into dist/clients (Docker, nothing published)
task ci                          # exactly what CI runs: ci:check (incl. audit) + ci:image + ci:clients
task release                     # from main: CI's checks, tag the next version (semrel), push the images, publish the CLI
```

Without go-task, see the plain commands in the [Taskfile](Taskfile.yml) or
[docs/development.md](docs/development.md).

## Before you call a change done

- `task fmt`, `task lint` and `task test` pass. Run the relevant browser test
  (`task test:e2e -- -k <name>`; the browser ↔ CLI tests are `cross_client`
  and `drawings`)
  for UI changes, and the whole suite for changes to sync, keys or the
  editor.
- New behaviour has a test at the right level:
  - crypto, formats, CRDT, Markdown → unit tests in `crates/core`
  - API, authorization, sync rules → `crates/server/tests/api.rs` (real
    client-side crypto against an in-process server)
  - outbox, refusals, snapshots, membership-log rules → unit and property
    tests in `crates/sync` (`src/tests.rs`, `tests/properties.rs`)
  - CLI, mirror, gateway → `crates/cli/tests/`
  - editor model, syntax, commands → unit and property tests in
    `crates/editor` (native); the editor's DOM behaviour (typing, IME,
    selection, native edits) → `crates/editor/tests/browser.rs`
    (`task test:editor`)
  - UI flows → `tests/e2e/suite.py` (several users = several browsers)
  - load test → unit tests in `crates/load`, `crates/load-events`
- Docs follow the code: API or wire change → `docs/protocol.md`; anything
  about keys, trust or metadata → `docs/security.md`; new CLI commands →
  `docs/cli.md`; requirement coverage → `docs/acceptance.md`.
- **Feature lists stay current.** A new user-facing feature, or a change to
  one (including its shortcuts), goes into the feature lists in the same
  change: the Features section of `README.md` and `site/features.html` — and
  `site/index.html` ("In the box") for headline features. Remove what no
  longer exists.
- **The demo shows every feature.** A new or changed user-facing feature
  gets demo data in `scripts/demo.py` in the same change, so that
  `task demo:reset && task demo` lets anyone see it working end to end. The
  demo creates its data through the CLI; if the CLI can't create it yet, add
  the command when it fits the CLI (the way icons were added), otherwise add a
  line to the "Things to try" list in alice's *Start here* note (e.g.
  browser-only settings). Then run the demo and click through what changed.
- Finished work leaves the TODO (it holds only what is still to be done).

## Things that are easy to get wrong

**Sync and keys**
- Updates are yrs v1 updates, encrypted per object, appended to a per-vault
  sequence log. Applying is idempotent; pushes are idempotent per
  `client_update_id`. A snapshot's `covers_seq` must cover every update of
  that object the client applied (see protocol.md §6).
- Vault keys have epochs `1, 2, …`; individually shared notes have *document
  keys* whose epochs have the top bit set (`0x8000_0001`, …). The envelope's
  epoch says which key decrypts it (`VaultReplica::decrypt_object`). Always
  encrypt through `VaultReplica::encrypt_object`, never directly with the
  vault keyring.
- The server refuses content of a shared note under an outdated key with
  `412 stale_key`; clients reload keys and re-send. Content that arrives
  before its key is deferred and replayed (`Deferred`, `retry_deferred`).
- Rotating a vault key must also rotate the document keys of its shared notes.

**Server**
- sqlx 0.9 only accepts static SQL strings: no `format!` into queries — bind
  parameters, or write variants out literally.
- Authorization goes through `access.rs` (`member_role`, `object_access`, …);
  don't re-implement checks in handlers. SQL lives in `repo/` (the log's in
  `sync.rs`): handlers don't write queries.
- A write that depends on a role re-checks it inside its `BEGIN IMMEDIATE`
  transaction (`access::require_owner(&mut *tx, …)`), and membership, role
  and share changes run under `write_lock` in such a transaction — see the
  rule at the top of `access.rs`.
- Live events go through `hub.rs`'s `EventBus` only; publish log events after
  the commit, while still holding `write_lock` (its docs list what
  subscribers rely on).
- While the server is read-only, every changing route is refused except
  those in `WRITABLE_WHEN_READ_ONLY` (`routes/mod.rs`): a new route is
  refused until added there, and only sign-in, revocation and administration
  belong there. Creating something a limit counts (accounts, vaults, agents)
  checks it inside the creating transaction (`instance.rs`).
- Only `routes/admin.rs` takes the `Admin` extractor (people with the admin
  role, or admin clients, which have no account) and checks a scope in each
  handler (`admin.require(AdminScope::…)`). Never accept it anywhere else:
  everything else takes a `Principal`, which keeps admin clients out of
  vaults.

**Web client**
- The app state (`state::Core`) is taken only at *edges* — event handlers,
  socket messages, timers, each step of an async task (between `await`s),
  views — with `state::step(|c| …)`. Everything else receives it as
  `c: &mut Core` / `c: &Core`, so it can't be taken twice. Don't add
  functions that take the state on their own; give them a `c` parameter.
  Calling `step` while the state is taken panics, naming both places.
- The editor and DOM calls that may fire events run at edges, outside
  `step` (e.g. `after_apply` pushes received text into the editor after the
  step that applied it). Code holding `c` that needs edge work done (e.g.
  `navigate`) queues it with `state::after_step`.
- The editor's DOM text must always equal the Markdown source (caret mapping
  depends on it): the editor element holds one `div.ln` per line and
  nothing else. Style with classes; hide syntax with CSS (`.mk`); anything
  that isn't source text must be `contenteditable="false"` with `data-w`.
  Tables are laid out with CSS `display: table-row/table-cell`, not widgets.
- Editor changes go through transactions (`crates/editor/src/state.rs`):
  build one in a command, dispatch it; the view re-renders only the lines it
  replaced or whose block changed. Never re-render everything per keystroke.
- Editor borrow rules (`crates/editor/src/view/mod.rs`): the state is only
  borrowed inside `Editor::model(|st| …)`, whose closure is `'static` — it
  computes, DOM and host calls happen after it returns. Every entry point is
  an operation; calls made during one (from host callbacks) are queued and
  events fired by our own DOM calls are deferred. No DOM changes while an
  IME composition is open: remote text and refreshes wait for its end.
- Leptos 0.8: share values between several closures with `StoredValue` or
  `Memo` instead of cloning chains; `impl IntoView` returns sometimes need
  `+ use<>`.
- Don't use `<For>` for lists whose items change places: tachys 0.2's keyed
  diff can leave an item in the wrong place when items move and others appear
  or go in the same update. Render such lists with `collect_view` (as the file
  tree does). Leptos also delegates events, so `current_target()` isn't the
  element with the handler: find it with `closest`.
- `Ctrl+K` inside the editor inserts a link; the quick switcher opens from
  the sidebar search button (relevant for browser tests).

**Builds and tests**
- Build the web crate with `CARGO_TARGET_DIR=target-web` (the Taskfile and
  `tests/e2e/run.sh` do) so native and wasm builds don't keep invalidating
  each other.
- `scripts/build-web.sh` replaces its output directory (`dist/web` by
  default) on every run.
- Browser tests start their own server and chromedriver on free ports with a
  temporary database; screenshots land in `target/e2e/`. Python buffers output
  when redirected — use `python3 -u` to follow progress.
- Manual CLI runs (`task cli`, `task cli:login`) create real profiles under
  your config directory (CLI tests use temporary ones); remove throwaway ones
  with `notekpr --profile NAME logout --forget --yes`.
- Don't `pkill -f` broad patterns like `notekpr`: they can match your own
  shell's command line. Kill by PID.
- The CLI's release builds (`task build:clients`) run GoReleaser in Docker
  (`packaging/Dockerfile`; tools pinned there with their checksums). Its
  Rust builder expects binaries in `target/<target>/release`, so don't set
  `CARGO_TARGET_DIR` for it: `packaging/run.sh` mounts `target-release/target`
  there instead. Packaging changes → `task ci:clients`.

## Git

- Default branch is `main`; releases are made from it (`task release`).
- Commit messages follow [Conventional Commits](https://www.conventionalcommits.org),
  in the imperative: `feat: add …`, `fix: …`, `docs: …`, `refactor: …`,
  `test: …`, `build: …`, `ci: …`, `chore: …`; `!` after the type for a
  breaking change. The next version is computed from them
  ([docs/development.md](docs/development.md#releases)). Keep commits small
  and focused.
- Merge commits too (`chore: merge …`): `task release` checks every commit
  since the last release, merges included, and stops at one that isn't.
- Formatting-only or other mechanical changes go in their own commit (`style: …`).
- Never commit `target*/`, `dist/`, `data/`, `.env` or databases
  (`.gitignore` covers them). `Cargo.lock` is committed; release builds use
  `--locked`.
