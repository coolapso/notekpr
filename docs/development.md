# Development

## Layout

```
crates/core     shared crypto, keys, CRDT model, protocol, engine, markdown, search
crates/server   sync server (axum + SQLite)            → coolpsonotes-server
crates/editor   live-preview Markdown editor (model + DOM view), used by crates/web
crates/web      browser client (Leptos → wasm32)       → dist/web
crates/client   desktop/CLI client, mirror, gateway    → coolpsonotes
site/           project website: static HTML + CSS, no build step (task site)
tests/e2e       browser end-to-end tests (Python stdlib + chromedriver)
deploy/         Caddy, Dex (dev OIDC), Kubernetes
docs/           architecture, protocol, security, operations, agents, desktop client
```

## Development dependencies

Run `task setup` to check what you have; it prints what's missing and how to
get it.

| Tool | Needed for | Arch package | Elsewhere |
|---|---|---|---|
| Rust ≥ 1.94 (`rustc`, `cargo`, `clippy`) | everything | `rust` | distro package or rustup |
| `wasm32-unknown-unknown` target | the browser client | `rust-wasm` | `rustup target add wasm32-unknown-unknown` |
| `wasm-bindgen` CLI **0.2.128** exactly | the browser client | `wasm-bindgen` (if it is 0.2.128) | `cargo install wasm-bindgen-cli --version 0.2.128 --locked` |
| go-task | the `task …` shortcuts (optional) | `go-task` | taskfile.dev |
| `wasm-opt` | smaller release web builds (optional) | `binaryen` | binaryen releases |
| Chromium + chromedriver, Python 3 | browser tests (`task test:e2e`) | `chromium` (includes chromedriver), `python` | matching Chrome/Chromium + chromedriver |
| cargo-deny, cargo-audit | supply-chain checks (`task audit`) | `cargo-deny`, `cargo-audit` | `cargo install` |
| Docker + Compose | container image, Docker dev stack, local Dex for OIDC | `docker`, `docker-compose`, `docker-buildx` | Docker docs |

The `wasm-bindgen` CLI must match the `wasm-bindgen` crate pinned in
`crates/web/Cargo.toml`. When you upgrade one, upgrade the other. A
`cargo install` puts it in `~/.cargo/bin`, which then has to come before
`/usr/bin` in `PATH`.

**Not needed:** rustup (distribution Rust packages work), Node.js, npm or any
JavaScript tooling. The few static files of the web client
(`crates/web/static`: `index.html`, a 6-line `boot.js` loader, `style.css`)
are hand-written. The server needs no external database: it embeds SQLite.

## Common tasks

A [Taskfile](../Taskfile.yml) ([go-task](https://taskfile.dev)) wraps the
commands below; `task` lists them all.

```sh
task setup                    # check the toolchain and say what's missing
task dev                      # dev server on http://localhost:8080 (no OIDC), data in ./data
task web:watch                # in a 2nd terminal: rebuild the web client on change, then reload the browser
task cli:login DEV_USER=bob   # sign the CLI in as a test user (profile "bob")
task cli -- --profile bob ls Family   # run any CLI command
task test                     # all Rust tests;  task test:e2e -- -k collab  for browser tests
task test:editor              # the editor's DOM view in headless Chromium (wasm-bindgen-test)
task fmt                      # rustfmt (default style); CI checks it with task fmt:check
task lint                     # clippy, warnings are errors
task ci                       # what CI runs
task dev:reset                # wipe the dev database
```

## Run locally with several users (no OIDC)

```sh
cargo build -p coolpsonotes-server
PROFILE=dev ./scripts/build-web.sh          # or omit PROFILE for an optimized build
./target/debug/coolpsonotes-server --dev-mode --web-dir dist/web
```

Open <http://localhost:8080> and sign in as `alice`. Open a **second tab**
(or another browser) and sign in as `bob`: each tab keeps its own session, and
each user gets their own device keys in the browser. Share a vault from alice
to bob (vault menu → Share vault, search "bob") and edit the same note in both
tabs to see live collaboration, presence and cursors.

The CLI supports multiple users via profiles:

```sh
cargo run -p coolpsonotes-client -- --profile alice login --server http://localhost:8080 --dev-user alice
cargo run -p coolpsonotes-client -- --profile bob   login --server http://localhost:8080 --dev-user bob
```

With Docker: `docker compose -f docker-compose.dev.yml up --build`
(add `--profile oidc` to also run a local Dex OIDC provider on port 5556 and
an OIDC-enabled coolpsonotes on <http://localhost:8081>; this profile uses host
networking, so it works on Linux only).

## Tests

```sh
cargo test -p coolpsonotes-core      # crypto, key hierarchy, CRDT model, diffs, markdown, search
cargo test -p coolpsonotes-server    # end-to-end API tests with real client-side crypto
cargo test -p coolpsonotes-editor    # editor model: change sets, incremental blocks, commands, syntax
cargo test -p coolpsonotes-sync      # the shared sync engine: outbox, refusals, no-loss property test
cargo test -p coolpsonotes-web       # safe rendering, zip
cargo test -p coolpsonotes-client    # sync engine, mirror, gateway against an in-process server
tests/e2e/run.sh                     # headless browsers: onboarding, collaboration, devices,
                                     # recovery, viewers, history, trash, search, offline,
                                     # attachments, mobile — then browser ↔ CLI mirror sync
```

The browser suite starts its own dev-mode server on a free port with a
temporary database; screenshots land in `target/e2e/`.

Property tests (`proptest`) run with `cargo test`; `PROPTEST_CASES=20000`
searches harder. Fuzz targets for everything that parses untrusted input
(Markdown, diffs, envelopes, protocol JSON, zip import, CRDT updates) live in
`fuzz/` and run on the stable toolchain — libFuzzer with coverage
instrumentation through `RUSTFLAGS`, no nightly needed:

```sh
task fuzz                 # every target, 60 s each
fuzz/run.sh crdt 600      # one target, longer
```

A crash or out-of-memory input is written to `fuzz/artifacts/`; add it as a
unit test next to the fix (the CRDT allocation guard in `crates/core/src/doc.rs`
came from this). With nightly and `cargo-fuzz`, `cargo fuzz run <target>`
works too and adds AddressSanitizer.

Also build the core for the browser to catch wasm-only issues:

```sh
cargo build -p coolpsonotes-core --target wasm32-unknown-unknown
```

### Editor browser tests

The editor's DOM view (typing, Enter and list continuation, tables, IME
composition sequences including Safari's event order, selection mapping,
undo/redo, remote edits, native DOM edits, drag and drop, teardown) is tested
in headless Chromium with `wasm-bindgen-test`. Its runner,
`wasm-bindgen-test-runner`, comes with the `wasm-bindgen` CLI (same pinned
version); it drives Chromium through `chromedriver`:

```sh
task test:editor
# or:
CARGO_TARGET_DIR=target-web CHROMEDRIVER=$(command -v chromedriver) \
  CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
  cargo test -p coolpsonotes-editor --target wasm32-unknown-unknown --test browser
```

Add `-- --nocapture` to see the typing-latency line of `typing_on_a_large_note`.
For the latency in the real app (CRDT, sync, Leptos around the editor), run
`python3 tests/e2e/perf.py --server target/debug/coolpsonotes-server --web dist/web`:
it types into a 5,000-line note and prints median/p95 per keystroke.

## Supply-chain checks

```sh
cargo install cargo-audit cargo-deny
cargo audit
cargo deny check licenses advisories
```

`Cargo.lock` is committed; release images are built with `--locked`.

## Conventions

* Anything that touches keys or ciphertext formats lives in `crates/core`
  and has unit tests. Clients never hand-roll crypto.
* The server must never need plaintext; if a feature seems to require it, it
  belongs in a client.
* Never log tokens, query strings, note content, names or paths.
* Keep the protocol additive; bump `/api/v1` only for breaking changes.
* Formatting-only commits go into `.git-blame-ignore-revs`; enable it once
  with `git config blame.ignoreRevsFile .git-blame-ignore-revs`.
