# sqlx-sqlite 0.9.0, patched

This is sqlx-sqlite 0.9.0 (https://github.com/launchbadge/sqlx, MIT or
Apache-2.0 — see `LICENSE-MIT`, `LICENSE-APACHE`), the SQLite driver of
sqlx, allowed a newer `libsqlite3-sys`. 0.9.0 is the latest release and
accepts `libsqlite3-sys` `>=0.30.1, <0.38.0`, whose bundled SQLite (before
3.54.0) can return uninitialized page-cache memory when a database is opened
with a `-wal` file of a different page size (RUSTSEC-2026-0459,
CVE-2026-119375). Only `libsqlite3-sys` 0.39 bundles a fixed SQLite (3.54),
so no lockfile update can help: the range has to move.

The server opens only its own database in its own data directory, so the
files would have to be written by someone who can already write there; but
`cargo deny` (and good hygiene) wants the fixed version.

Wired in through `[patch.crates-io]` in the workspace `Cargo.toml`. Changes
(marked `notekpr patch` in the file):

* `Cargo.toml` (and `Cargo.toml.orig`): `libsqlite3-sys` `<0.38.0` →
  `<0.40.0`. Upstream raises the top of this range in patch releases (see
  the note at the top of `src/lib.rs`); the code needed no change.
* The license files are the texts from the sqlx crate: the published
  crate's are links to `../LICENSE-*`. Its `Cargo.lock` isn't copied.

Drop this directory once a sqlx release accepts `libsqlite3-sys` 0.39.
