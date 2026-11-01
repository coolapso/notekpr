# Development

## Layout

```
crates/core     shared crypto, keys, CRDT model, protocol, engine, markdown, search
crates/sync     client sync engine both clients use (outbox, refusals, snapshots, membership log)
crates/server   sync server (axum + SQLite)            → notekpr-server
crates/editor   live-preview Markdown editor (model + DOM view), used by crates/web
crates/web      browser client (Leptos → wasm32)       → dist/web
crates/cli      the CLI, Markdown mirror, gateway      → notekpr
crates/load     load test runner (task load)           → notekpr-load
crates/load-events  what the load runner streams to its live view
crates/load-ui  the load test's live view (wasm32)     → dist/load
scripts/        build-web.sh, build-load-ui.sh, demo.py (task demo), logo.py (the logo)
site/           project website: static HTML + CSS and a little JS, no build step (task site)
tests/e2e       browser end-to-end tests (Python stdlib + chromedriver)
fuzz/           fuzz targets, a separate workspace (fuzz/run.sh, task fuzz)
vendor/yrs      yrs 0.28 with fixes, via [patch.crates-io] (see its NOTEKPR.md)
vendor/openidconnect  openidconnect 4.0.1 on newer curve crates (see its NOTEKPR.md)
deploy/         Kubernetes, Dex (dev OIDC), examples/ (optional reverse proxies)
docs/           architecture, protocol, security, operations, agents, CLI;
                assets/ (the logo and icon)
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
| cargo-deny | supply-chain checks (`task audit`) | `cargo-deny` | `cargo install cargo-deny --locked` |
| Docker + Compose | container image, Docker dev stack, local Dex for OIDC, the load test (`task load`), the S3 file store's test (`task test:s3`, with Garage) | `docker`, `docker-compose`, `docker-buildx` | Docker docs |

The `wasm-bindgen` CLI must match the `wasm-bindgen` crate pinned
(`=0.2.128`) in `crates/web/Cargo.toml`, `crates/editor/Cargo.toml` and
`crates/load-ui/Cargo.toml`; the Dockerfile installs the CLI version in its
`ARG WASM_BINDGEN_VERSION`. `scripts/build-web.sh` and
`scripts/build-load-ui.sh` check the CLI against their crate's pin. When you
upgrade, upgrade all of them together with the CLI. A `cargo install` puts
it in `~/.cargo/bin`, which then has to come before `/usr/bin` in `PATH`.

**Not needed:** rustup (distribution Rust packages work), Node.js, npm or any
JavaScript tooling. The few static files of the web client
(`crates/web/static`: `index.html`, a small `boot.js` loader, `style.css`,
the `sw.js` service worker, an icon and a web app manifest) are
hand-written. The server needs no external database: it embeds SQLite.

## Common tasks

A [Taskfile](../Taskfile.yml) ([go-task](https://taskfile.dev)) wraps the
commands below; `task` lists them all.

```sh
task setup                    # check the toolchain and say what's missing
task dev                      # dev server on http://localhost:8080 (no OIDC), data in ./data, with an admin client
task dev:adminless            # the same, admin-less: no administrators among the people, only admin clients
task demo                     # a demo with users, vaults and shared notes on http://localhost:8099
task load                     # load test with a live view on http://localhost:8098 (needs Docker)
task web:watch                # in a 2nd terminal: rebuild the web client on change, then reload the browser
task cli:login DEV_USER=bob   # sign the CLI in as a test user (profile "bob")
task cli -- --profile bob ls Family   # run any CLI command
task test                     # all Rust tests;  task test:e2e -- -k collab  for browser tests
task test:editor              # the editor's DOM view in headless Chromium (wasm-bindgen-test)
task test:s3                  # attachments in S3-compatible storage (Garage in Docker)
task fmt                      # rustfmt (default style); CI checks it with task fmt:check
task lint                     # clippy, warnings are errors
task ci                       # exactly what CI runs: checks, every test, audit, the images
task audit                    # supply-chain checks (cargo deny check)
task dev:reset                # wipe the dev database
task build                    # debug build of server, CLI and web client;  task build:release  for release builds
task check:wasm               # build the core for wasm32 (catches wasm-only issues)
task backup                   # a timestamped backup of ./data into backups/ (keeps the newest 10)
task docker:dev               # the dev server in Docker (docker:dev:oidc adds Dex, docker:dev:down stops it)
task clean                    # remove build outputs (keeps the dev database)
```

## A demo with data

```sh
task demo          # builds, creates the data on the first run, serves http://localhost:8099
task demo:reset    # start over
task demo:cli -- --profile bob vault list   # the CLI as one of the demo users
```

[`scripts/demo.py`](../scripts/demo.py) (Python standard library) starts a
development-mode server with its own database and CLI profiles in
`data/demo/` (apart from `task dev`'s, so both can run; `task dev:reset`
deletes both) and fills it through the real CLI, so everything is encrypted
like any other data:

| User | Owns | Shared with them |
|---|---|---|
| alice | *Home* (~35 linked notes for the graph, pictures, quick notes, history, a note in the trash), *Work*, *Family*, *Assistant* (with an agent), *Old projects* (archived) | *Book club*, *Garden club*; Bob's *Training plan* and Carol's *Logo ideas* as single notes |
| bob | *Book club*, *Running* | *Family* (editor) |
| carol | *Design* | *Family* (viewer), *Book club*; Alice's *Apple pie* and *Design brief* as single notes |
| dave | — | *Work* (editor) |
| eve | *Garden club* | — |

Icons, tables, tasks, code, footnotes, heading links, a link to a missing
note and edits by several people are all in there; alice is also the
server's administrator. *Start here* in alice's *Home* lists things to try,
including what the script can't set up (favorites, settings, pictures, …).

Sign in as one of them (one per tab). A browser is a new device of its user:
choose **Approve from another device** and the running demo approves it
within a couple of seconds (`--no-approve` turns that off; the recovery keys
are in `data/demo/recovery-keys.txt`). The server's log goes to
`data/demo/server.log`. `task demo -- --seed-only` stops once the data is
there; `task demo -- -v` prints every CLI command it runs. The script
refuses to seed a server that isn't in development mode. To change the
content, edit the script and `task demo:reset`. Port 8099 taken? `task demo
DEMO_LISTEN=127.0.0.1:8097`. To put the demo online for others, resetting
itself every hour: [operations.md §10](operations.md#10-a-public-demo).

**Keep it complete.** The demo is meant to show every user-facing feature,
so it doubles as a hands-on end-to-end check: after `task demo:reset && task
demo` you can click through everything with real, encrypted data from
several users. A new or changed feature gets demo data in the same change
(the rule is in [AGENTS.md](../AGENTS.md#before-you-call-a-change-done)):
created through the CLI where it can be, otherwise as a line in the "Things
to try" list of alice's *Start here* note. Seeding stops at the first CLI
command that fails, and the server log is in `data/demo/server.log`, so a
broken CLI or server path shows up while seeding too.

## A load test you can watch

```sh
task load                                  # 10 levels, 100 → 1000 people, against the server image (1 CPU, 512 MB); live view on :8098
task load LEVELS=1 DURATION=2m             # one crowd of 100
task load USERS=300 STEP=300 LEVELS=3 CPUS=2 MEMORY=1g
task load REALISTIC=1 USERS=500 STEP=500   # people as busy as real ones
task load:native                           # no Docker: a local server process, no limits
task load:remote SERVER=https://…          # a throwaway dev-mode server elsewhere
```

Simulated people — real clients with real keys and encryption — sign up,
share vaults, type together, upload pictures; the crowd grows level by
level and the server is killed in each, for longer each time; at the end
every copy of every note is compared. Needs Docker (`task load`
builds the image). What it measures and how: [load-test.md](load-test.md).

## Run locally with several users (no OIDC)

```sh
cargo build -p notekpr-server
CARGO_TARGET_DIR=target-web PROFILE=dev ./scripts/build-web.sh   # or omit PROFILE for an optimized build
./target/debug/notekpr-server --dev-mode --web-dir dist/web
```

A dev build of the web client is unoptimized (~15 MB): its first load takes
a second or two, the optimized one well under one. Its function names are
left out, so stack traces in the browser console show `wasm-function[…]`
(panic messages keep their file and line); build with `WASM_NAMES=1` to keep
them, at about 6 seconds per first load.

Open <http://localhost:8080> and sign in as `alice`. Open a **second tab**
(or another browser) and sign in as `bob`: each tab keeps its own session, and
each user gets their own device keys in the browser. Share a vault from alice
to bob (vault menu → Share vault, search "bob") and edit the same note in both
tabs to see live collaboration, presence and cursors.

`task dev` also sets up an admin client called `dev`, with every scope and
a fixed token (the Taskfile's `DEV_ADMIN_TOKEN`, through `notekpr-server
admin-client ensure`; the task prints a `curl` line to start from), to try
the admin API as a billing system would
([operations.md](operations.md#admin-clients-scripts-and-services)):

```sh
curl -H "Authorization: Bearer notekpr_a_dev-admin-token-for-this-machine-only" \
     -X PATCH http://localhost:8080/api/v1/admin/settings \
     -H 'Content-Type: application/json' -d '{"read_only": true}'
```

The CLI supports multiple users via profiles:

```sh
cargo run -p notekpr-cli -- --profile alice login --server http://localhost:8080 --dev-user alice
cargo run -p notekpr-cli -- --profile bob   login --server http://localhost:8080 --dev-user bob
```

With Docker: `task docker:dev` (`docker compose -f docker-compose.dev.yml up
--build`); `task docker:dev:oidc` (`--profile oidc`) also runs a local Dex
OIDC provider on port 5556 and an OIDC-enabled notekpr on
<http://localhost:8081> — this profile uses host networking, so it works on
Linux only. `task docker:dev:down` stops the stack and keeps its data.

## Tests

```sh
cargo test -p notekpr-core      # crypto, key hierarchy, CRDT model, diffs, markdown, search
cargo test -p notekpr-server    # end-to-end API tests with real client-side crypto
cargo test -p notekpr-editor    # editor model: change sets, incremental blocks, commands, syntax
cargo test -p notekpr-sync      # the shared sync engine: outbox, refusals, no-loss property test
cargo test -p notekpr-web       # safe rendering, zip
cargo test -p notekpr-cli       # sync engine, mirror, gateway against an in-process server
cargo test -p notekpr-load -p notekpr-load-events   # the load test: world, outages, metrics, verdict
tests/e2e/run.sh                     # headless browsers: every UI flow (TESTS in suite.py:
                                     # onboarding, collaboration, devices and recovery, offline,
                                     # attachments, note sharing, tables, editor, mobile, icons,
                                     # graph, …) — then browser ↔ CLI mirror sync
```

The browser suite starts its own dev-mode server on a free port with a
temporary database; screenshots land in `target/e2e/` (`NOTEKPR_E2E_OUT`
changes that). It runs `/usr/bin/chromium`; for another browser set
`NOTEKPR_CHROME=/path/to/chrome` (with a matching chromedriver on `PATH`).
`-k NAME` (`task test:e2e -- -k collab`) runs only the tests whose name
contains NAME; the browser ↔ CLI tests are `cross_client` and `drawings`
(drawing in the browser, checked with the CLI).

Property tests (`proptest`) run with `cargo test`; `PROPTEST_CASES=20000`
searches harder. Fuzz targets for everything that parses untrusted input
(Markdown, HTML in notes, diffs, envelopes, protocol JSON, zip import, CRDT
updates) and for the editor's model live in `fuzz/` (a separate workspace)
and run on the stable toolchain — libFuzzer with coverage instrumentation
through `RUSTFLAGS`, no nightly needed:

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
task check:wasm
# or:
CARGO_TARGET_DIR=target-web cargo build -p notekpr-core --target wasm32-unknown-unknown
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
  cargo test -p notekpr-editor --target wasm32-unknown-unknown --test browser
```

Add `-- --nocapture` to see the typing-latency line of `typing_on_a_large_note`.
For the latency in the real app (CRDT, sync, Leptos around the editor), run
`python3 tests/e2e/perf.py --server target/debug/notekpr-server --web dist/web`:
it types into a 5,000-line note and prints median/p95 per keystroke.

## Supply-chain checks

```sh
task audit                # or: cargo deny check
```

`cargo deny check` checks advisories (RustSec), licenses, banned crates and
sources as `deny.toml` sets them (`task ci` runs it too). `Cargo.lock` is
committed; release images are built with `--locked`.

**CI** is a fresh developer machine: the workflows in `.github/workflows/`
only install the tooling and then run tasks — `task setup`, then `task ci`
(and `task fuzz SECS=300` once a week). Anything CI does, `task ci` does
here; a check that should run in CI goes into a task first. CI runs `task ci`'s
two halves, `task ci:check` and `task ci:image`, as separate jobs (the image
job starts once the checks pass): one GitHub runner's disk (14 GB guaranteed)
doesn't hold the test builds and the image build together. `task ci:image`
also seeds the demo in its image the way Kubernetes runs it, with nothing
writable but `/data` (`task docker:test:demo`).

## Releases

Releases are made from this machine, from `main`, with
[semrel](https://www.semrel.io) (`.semrel.yaml`). It computes the next
version from the commit messages since the last release, so they follow
[Conventional Commits](https://www.conventionalcommits.org): `feat:` bumps
the minor version; `fix:`, `perf:`, `refactor:` and `build:` the patch
version; other types (`docs:`, `test:`, `ci:`, `chore:`, `style:`) don't
make a release on their own. Below 1.0.0 a breaking change (`feat!:`, or
`BREAKING CHANGE:` in the body) bumps the minor version too: 0.1.0 → 0.2.0
(`version_ceiling` in `.semrel.yaml`).

```sh
task tools:release      # once: semrel (needs Go); also needs jq, and gh signed in (or GITHUB_TOKEN)
task release:plugins    # once: semrel's plugins, pinned in .semrel.lock, into .semrel/
task release:check      # CI's checks, then a preview of the next version and its notes
task release            # the same, then the release
```

`task release` tags the next version (`v0.2.0`), pushes the tag, creates
the GitHub Release with the notes, then pushes both images for amd64 and
arm64, `ghcr.io/coolapso/notekpr` and `ghcr.io/coolapso/notekpr-demo`,
tagged `0.2.0` and `latest`, and publishes the CLI (below). Without commits
that make a release it does nothing. It doesn't push `main`: push it as
usual. The working tree must be clean, untracked files included: GoReleaser
refuses to publish otherwise.

### The CLI's packages

The server is released as images only; the CLI as files and packages,
made by [GoReleaser](https://goreleaser.com) (the free one) from
`.goreleaser.yaml`. GoReleaser runs in a build image
(`packaging/Dockerfile`: Rust with the Linux targets, zig and cargo-zigbuild
to build both architectures for an old enough glibc, rpmbuild for COPR's
source RPM), so Docker is all a machine needs — no rustup, no cross
compilers. `packaging/run.sh` runs a command in it, as your user, with the
checkout at `/src`.

```sh
task build:clients      # everything into dist/clients, nothing published (a snapshot)
task test:packages      # then: install the .deb in Debian and the .rpm in AlmaLinux 8, run them
task test:srpm          # and: build the source RPM in Fedora, as COPR does, install it, run it
task ci:clients         # all three, as CI runs them
task release:clients    # publish for the release tag at HEAD (task release does it)
task release:copr       # only the COPR build, again (release:clients does it)
task copr:project       # once: create the COPR project
```

A release adds to the GitHub Release semrel made (its notes stay):

| File | What |
|---|---|
| `notekpr-cli_<v>_linux_{amd64,arm64}.tar.gz` | `notekpr` (glibc 2.28 or newer), completions, the systemd user unit |
| `notekpr-cli_<v>_{amd64,arm64}.deb`, `notekpr-cli-<v>-1.{x86_64,aarch64}.rpm` | the same as packages, installed from the file (no package repository yet) |
| `notekpr-cli-<v>.tar.gz` | the source, which COPR's source RPM is made from |
| `notekpr-cli_<v>_checksums.txt` | SHA-256 of all of them |

and publishes the AUR package `notekpr-cli-bin`. It is pushed with the
SSH key your own ssh uses for `aur.archlinux.org` (`AUR_KEY_FILE` to choose
another), in CI (`$CI` set) with the `aur` secret in Infisical; the key's
contents in `AUR_KEY` take precedence over both. The key must have no
passphrase.

Last, COPR builds the CLI for Fedora in `coolapso/notekpr`, and the release
waits for its builds (Ctrl+C stops waiting, not the builds). GoReleaser has
no pipe for COPR: `packaging/rpm/srpm.sh` makes a source RPM
(`notekpr-cli-<v>-1.src.rpm` in `dist/clients`, from the spec
`packaging/rpm/notekpr-cli.spec.in` and the release's source) and
`copr-cli` uploads it. COPR builds it with each Fedora release's own Rust
(at least `Cargo.toml`'s `rust-version`); cargo fetches the crates
`Cargo.lock` pins, so the builds have internet access. `copr-cli` runs on
the machine releasing (Arch: `copr-cli`; elsewhere `task tools:copr`
installs it with pipx, as CI does) with its configuration from
https://copr.fedorainfracloud.org/api/ (signed in with a Fedora account;
the token in it lasts 180 days) saved as `~/.config/copr`, in CI with the
`copr` secret in Infisical; `COPR_CONFIG` holding the configuration takes
precedence over both. `task release` checks that copr-cli and its
configuration are there before it releases anything.
`task copr:project` creates the project, once: every Fedora release and
rawhide, amd64 and arm64; COPR adds each new release when Fedora branches
it.

If publishing fails halfway, fix it and run
`task release:clients` again on the tagged commit (the working tree clean):
it builds again and replaces the files already on the release. Linux only
for now. Snapshots are named like the images' versions
(`0.2.0-3-gabc1234`).

Between releases, `task docker:push` and `task docker:push:demo` push an
image tagged `latest` and with the version `git describe` gives after
fetching the tags: `0.2.0` on a release, `0.2.0-3-gabc1234` three commits
after it, with `-dev` appended when there are uncommitted changes.
`VERSION=x.y.z` sets it instead. The image's
`org.opencontainers.image.version` label carries it too.

`VERSION=x.y.z` chooses the version instead (semrel's `--next-version`).
The first release is `task release VERSION=0.1.0`: the history before it
predates Conventional Commits, so `task release:check` lints commit
messages only once a release tag exists.

The version is also baked into what the image holds: the server
(`--version`, `/info`, the start-up log, the admin page and
`/admin/stats`), the CLI (`--version`, its requests) and the web app
(Settings → Account, its requests). The Dockerfile hands its `VERSION`
build argument to the builds as `NOTEKPR_VERSION` (GoReleaser sets it for
the CLI's builds), which `crates/core` reads at compile time
(`notekpr_core::VERSION`; Cargo rebuilds when it changes). Without it — `task dev`, `cargo build`, `task docker:build` —
everything says `dev`. The crates' own versions in `Cargo.toml` aren't
used for this and stay `0.1.0`.

Installed apps (the CLI now, desktop and phone apps later) keep running
for months. A release that stops working with older apps — say it changes
what they must understand — raises `OLDEST_APP_VERSION` in
`crates/server/src/apps.rs` to the first version that works with it: older
apps are then told to update instead of misbehaving
([protocol.md §11](protocol.md#11-older-apps)). Development builds are never
refused.

## Logo and icon

`docs/assets/` holds the logo and the icon, as SVG and PNG with transparent
backgrounds:

| File | What |
|---|---|
| `logo.svg`, `logo.png` | the name, for light backgrounds |
| `logo-dark.svg`, `logo-dark.png` | the same for dark backgrounds |
| `icon.svg`, `icon.png` | the icon alone (512×512): avatars, thumbnails |

The README shows the logo for GitHub's theme with a `<picture>` element.

The logo is "notekpr" in [Fira Code](https://github.com/tonsky/FiraCode)
SemiBold, with the icon's diamond ring as the "o"; the letters are stored as
outlines, so it looks the same without the font. `scripts/logo.py` draws it —
its sizes, spacing and colours are at its top — and writes all six files:

```sh
python3 scripts/logo.py path/to/FiraCode-SemiBold.ttf
```

It needs fontTools (`python-fonttools`, or `pip install fonttools`),
`rsvg-convert` (librsvg) and the font, which isn't in the repository:
`ttf/FiraCode-SemiBold.ttf` in the zip of a
[Fira Code release](https://github.com/tonsky/FiraCode/releases) (the logo
was made with 6.2). None of it is part of the build, `task setup` or CI: run
it by hand when the logo or the icon changes, and commit what it writes.

The icon is the favicon, `crates/web/static/favicon.svg`. It has copies in
`site/favicon.svg` and, drawn in the theme's colours, in the header of each
page in `site/`. Change them together, then run `scripts/logo.py` for
`icon.svg` and `icon.png`.

## Conventions

* Anything that touches keys or ciphertext formats lives in `crates/core`
  and has unit tests. Clients never hand-roll crypto.
* The server must never need plaintext; if a feature seems to require it, it
  belongs in a client.
* Never log tokens, query strings, note content, names or paths.
* Keep the protocol additive; bump `/api/v1` only for breaking changes.
* Formatting-only commits go into `.git-blame-ignore-revs`; enable it once
  with `git config blame.ignoreRevsFile .git-blame-ignore-revs`.
