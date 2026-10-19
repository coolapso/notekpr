# AGENTS.md

Guide for anyone — AI coding agents and humans — changing this repository.
Read this first; it is short on purpose and links to the detailed docs.

> Not to be confused with [docs/agents.md](docs/agents.md), which documents
> the *product feature* that lets AI agents use coolpsonotes as a knowledge base
> (CLI, Markdown folder, local HTTP gateway).

## What this is

coolpsonotes is a self-hosted, local-first, **end-to-end encrypted**
Markdown notes platform with real-time collaboration, written entirely in
Rust: a zero-knowledge sync server, a browser client compiled to WebAssembly,
and a desktop/CLI client (sync daemon, Markdown mirror, agent gateway).
The original requirements are in [spec.md](spec.md).

| Doc | Read it when you… |
|---|---|
| [docs/architecture.md](docs/architecture.md) | need the big picture and why things are the way they are |
| [docs/protocol.md](docs/protocol.md) | touch the HTTP API, WebSocket messages or ciphertext formats |
| [docs/security.md](docs/security.md) | touch keys, crypto, authorization, logging or anything a server could learn |
| [docs/development.md](docs/development.md) | set up a machine, run things locally, run tests |
| [docs/acceptance.md](docs/acceptance.md) | want to know which test proves which requirement |
| [docs/desktop-client.md](docs/desktop-client.md), [docs/operations.md](docs/operations.md) | change the CLI or deployment/ops behaviour |

Known work, review findings and decisions are tracked in [TODO.md](TODO.md) —
check it before starting something, and keep it current.

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
8. **Never edit an applied migration.** Add a new numbered file in
   `crates/server/migrations/`.

## Layout

```
crates/core     crypto.rs (envelopes, HPKE, signatures) · keys.rs (account/device/
                recovery/vault/document keys) · engine.rs (VaultReplica: apply,
                encrypt, defer, snapshot; sharing helpers) · doc.rs (yrs CRDT index
                + notes) · protocol.rs (every API/WS type) · markdown, search, paths, textdiff
crates/server   axum + SQLite. routes/ (REST) · ws.rs (WebSocket, subscriptions) ·
                sync.rs (encrypted log, snapshots, key checks) · access.rs (who may
                do what) · hub.rs (pub/sub) · auth.rs, oidc.rs · migrations/
crates/web      Leptos CSR → wasm32. state/ (Core + Ui in mod.rs; session, vaults,
                shares, sync, presence, prefs, nav, offline) · cache.rs (IndexedDB
                copy) · static/sw.js (service worker, only with offline use on) ·
                actions.rs (user operations) · editor/ (contenteditable live-preview
                editor, syntax.rs, vim.rs) · views/ · render.rs (reading view)
crates/client   `coolpsonotes` CLI: session.rs + livesync.rs (sync), mirror.rs
                (Markdown files), gateway.rs, ops.rs, note_shares.rs
tests/e2e       browser tests: suite.py, cross_client.py, wd.py (stdlib WebDriver)
site/           project website: static HTML + CSS, no build step, no third-party
                requests (fonts, scripts, analytics) — keep it that way
deploy/         Caddyfiles, Dex (dev OIDC), Kubernetes, compose overrides
(root)          Dockerfile, docker-compose.yml, docker-compose.dev.yml, Taskfile.yml
```

## Commands

Requires Rust ≥ 1.94 with the `wasm32-unknown-unknown` target and the
`wasm-bindgen` CLI **exactly** matching the crate pin in
`crates/web/Cargo.toml` (currently 0.2.128). Distribution packages are fine;
rustup is not required. `task setup` checks everything.

```sh
task dev                         # build + run a dev-mode server on http://localhost:8080
task web:watch                   # rebuild the web client on change (then reload the browser)
task cli -- --profile alice ls Family   # run the CLI;  task cli:login DEV_USER=alice
task fmt / task lint             # rustfmt; clippy with warnings as errors
task test                        # all Rust tests
task test:e2e -- -k sharing      # browser tests (chromium + chromedriver + python3)
task ci                          # what CI runs (CI also runs cargo-deny: task audit)
```

Without go-task, see the plain commands in the [Taskfile](Taskfile.yml) or
[docs/development.md](docs/development.md).

## Before you call a change done

- `task fmt`, `task lint` and `task test` pass. Run the relevant browser test
  (`task test:e2e -- -k <name>`) for UI changes, and the whole suite for
  changes to sync, keys or the editor.
- New behaviour has a test at the right level:
  - crypto, formats, CRDT, Markdown → unit tests in `crates/core`
  - API, authorization, sync rules → `crates/server/tests/api.rs` (real
    client-side crypto against an in-process server)
  - CLI, mirror, gateway → `crates/client/tests/`
  - editor syntax and rendering → unit tests in `crates/web` (run natively)
  - UI flows → `tests/e2e/suite.py` (several users = several browsers)
- Docs follow the code: API or wire change → `docs/protocol.md`; anything
  about keys, trust or metadata → `docs/security.md`; new CLI commands →
  `docs/desktop-client.md`; requirement coverage → `docs/acceptance.md`.

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
  don't re-implement checks in handlers.

**Web client**
- `state::with(|core| …)` borrows a thread-local `RefCell`. Never call into
  the editor, or anything that might call `with` again, while inside it —
  copy what you need out first.
- The editor's DOM text must always equal the Markdown source (caret mapping
  depends on it). Style with classes; hide syntax with CSS (`.mk`); anything
  that isn't source text must be `contenteditable="false"` with `data-w`.
  Tables are laid out with CSS `display: table-row/table-cell`, not widgets.
- Leptos 0.8: share values between several closures with `StoredValue` or
  `Memo` instead of cloning chains; `impl IntoView` returns sometimes need
  `+ use<>`.
- `Ctrl+K` inside the editor inserts a link; the quick switcher opens from
  the sidebar search button (relevant for browser tests).

**Builds and tests**
- Build the web crate with `CARGO_TARGET_DIR=target-web` (the Taskfile does)
  so native and wasm builds don't keep invalidating each other.
- `scripts/build-web.sh` replaces its output directory (`dist/web` by
  default) on every run.
- Browser tests start their own server and chromedriver on free ports with a
  temporary database; screenshots land in `target/e2e/`. Python buffers output
  when redirected — use `python3 -u` to follow progress.
- CLI tests or manual runs create real profiles under your config directory;
  remove throwaway ones with `coolpsonotes --profile NAME logout --forget --yes`.
- Don't `pkill -f` broad patterns like `coolpsonotes`: they can match your own
  shell's command line. Kill by PID.

## Git

- Default branch is `dev`. Keep commits small and focused, with imperative
  messages ("Add …", "Fix …").
- Formatting-only or other mechanical changes go in their own commit.
- Never commit `target*/`, `dist/`, `data/`, `.env` or databases
  (`.gitignore` covers them). `Cargo.lock` is committed; release builds use
  `--locked`.
