# Architecture and design decisions

> **Principle:** the server coordinates; clients own the data and the keys.

This document describes how notekpr is built, why, and which
alternatives were considered. The wire protocol is in
[protocol.md](protocol.md) and the threat model in [security.md](security.md).

## 1. Components

```
                 ┌────────────────────────── trusted clients ───────────────────────────┐
                 │                                                                      │
 Browser ──────▶ │  Web client (Rust → WebAssembly, Leptos)                             │
                 │  Desktop client `notekpr` (Rust): sync daemon, Markdown mirror       │
 AI agents ────▶ │  same client: CLI commands, a Markdown folder, local HTTP gateway    │
                 │                                                                      │
                 │      all of them link notekpr-core: crypto · keys · CRDT ·            │
                 │      protocol types · Markdown analysis · search                      │
                 └──────────────────────────────┬───────────────────────────────────────┘
                                                │ HTTPS + WebSocket (TLS at the proxy)
                                                │ ciphertext, public keys, metadata only
                                     ┌──────────▼───────────┐       ┌──────────────┐
                                     │ notekpr-server       │◀─────▶│ OIDC provider │
                                     │ (Rust, axum, SQLite) │       │ (Authelia,    │
                                     └──────────────────────┘       │ Keycloak, …)  │
                                                                    └──────────────┘
```

| Crate | Role |
|---|---|
| `crates/core` | Everything security-sensitive or shared: cryptographic formats, key hierarchy, the CRDT document model (vault index + notes), the signed membership log (`memberlog.rs`), protocol types, client engine logic (`engine.rs`), offline recovery of an encrypted export (`recover.rs`), Markdown link/tag analysis, search, path rules. Compiled natively and to wasm32. |
| `crates/sync` | The client sync engine both clients use (`notekpr-sync`): what to upload next (unsent updates merged per object, held back when too large, size-bounded pushes), what acknowledgements and refusals mean, rollback recovery, snapshot readiness, splitting large edits, and the client side of the membership log (`members.rs`: verifying it, signing the next entry, deciding whom to trust). *Sans-IO*: clients do the networking, storage and timers and feed it what happened, so it is identical on wasm and tokio and tested without a network. |
| `crates/server` | Headless sync server: OIDC relying party, sessions, OAuth device and client-credentials grants, users/devices/agents, vaults/members/key envelopes, the ordered encrypted log, snapshots, attachments, WebSocket fan-out, rate limiting, metrics, backup/restore. Works without the web client. |
| `crates/editor` | The live-preview Markdown editor: a pure-Rust document model (text with a line index, change sets, transactions, incrementally updated decorations, commands) tested natively, and a `contenteditable` DOM view tested in a headless browser. |
| `crates/web` | Browser client compiled to WebAssembly; served as static files by the server. Hosts the editor, all decryption in the browser. |
| `crates/client` | Desktop/CLI client: login (device flow), enrollment, encrypted local replica, live sync daemon with plaintext Markdown mirror, export/import, agent gateway (local HTTP API). |
| `crates/load` | The load test (`task load`): simulated people — real clients built on the CLI's library, with real keys — against the server image with CPU and memory limits; kills the server midway through each level, starts it again, and checks that nothing was lost ([load-test.md](load-test.md)). |
| `crates/load-events` | The event types the load test streams to its live dashboard. |
| `crates/load-ui` | The load test's live dashboard: Rust compiled to wasm32, drawing on a canvas (`scripts/build-load-ui.sh`). |

The server never links any code path that could decrypt content: it only
handles opaque byte strings and public keys.

### 1.1 Inside the server

| Part | Does |
|---|---|
| `routes/`, `ws.rs` | HTTP and WebSocket handlers: validate requests, ask `access.rs`, write through `repo/` or `sync.rs`, publish events. No SQL. |
| `access.rs` | Authorization decisions (`404` without access so ids can't be probed, `403` for too low a role). A write that depends on a role checks it again inside its own transaction; membership, role and share changes run under the write lock in a transaction too, so a write can't land after a removal or demotion. |
| `repo/` | All SQL except the log's, as small functions per table group (users, sessions, devices, vaults and members, keys, shares, the membership log, agents, audit, attachments, usage statistics for administrators, the instance settings). One place to review and test authorization-sensitive queries; unit tests run against a migrated database. |
| `sync.rs` | The encrypted log: push, catch-up pages, snapshots, purges, the key-epoch checks. |
| `hub.rs` | `EventBus`: vault events and per-user control messages to live connections. Today in-process channels. Its interface states what another backend (polling the log, a message broker) must guarantee — per-vault order, nothing lost after `subscribe` returns, gaps reported as lag, connection ids unique across processes — so an instance could later run as several processes on one database without touching callers. |
| `blobs.rs` | Attachment bytes. Stored in SQLite today; a directory or S3-compatible store can be added as another variant. |
| `instance.rs` | Settings administrators change at runtime — limits on accounts, agents and vaults (also per person), read-only mode, the instance's storage limit, the storage limit per vault — kept in the database and in memory, and whether the server accepts changes now. Limits are checked in the transaction that creates something; read-only mode is enforced by a guard in front of every changing route (with a short list of exceptions) and in `sync.rs`, and live connections hear when it changes. Each vault's size is kept by SQLite triggers (first migration) whichever query adds or removes content; a full vault refuses new content in the writing transaction, and its subscribers hear when it fills or has room again (`sync::storage_changed`). Administrators (`auth.rs`'s `Admin`: people with the admin role, and admin clients) are accepted only by the administration routes. |
| `admin_clients.rs` | Admin clients: scripts and services administering the instance, each with scopes and bearer tokens (stored hashed). No accounts — their tokens only open the administration routes. Made by administrators, or from `notekpr-server admin-client` (`create`, and `ensure` for declarative setups). |

One server-wide write lock serializes writes (SQLite has a single writer
anyway); log events are published after their commit while it is still held,
which is what makes each vault's live stream gapless and ordered.

## 2. Technology decisions

| Area | Choice | Alternatives considered | Why |
|---|---|---|---|
| Language | Rust everywhere, including the browser (WebAssembly) | TypeScript web client (Svelte/React + CodeMirror + Yjs) | Mandated for the core; using it in the browser too means a single implementation of crypto and CRDT logic for every client, no npm supply chain, and no JS/Rust format drift. Cost: we wrote our own editor instead of reusing CodeMirror. |
| Server framework | axum 0.8 + tokio + tower-http | actix-web, warp | Mature, minimal, first-class WebSockets, tower middleware ecosystem. |
| Database | SQLite (WAL) via sqlx, single file — **SQLite only, by decision**, also for a hosted offering (one database per instance; grow by adding instances) | PostgreSQL | Zero extra services for self-hosting; a single file is a complete, consistent backup (`VACUUM INTO`); a notes workload with no server-side content processing fits one SQLite instance for a very large number of users. The SQL is SQLite-specific (e.g. `VACUUM INTO`, `BEGIN IMMEDIATE`). Storage access sits behind small interfaces (queries in `repo/`, attachment bytes in `blobs.rs`, live events in `hub.rs`, see §1.1) so the decision stays cheap to revisit, but there are no plans for another database. |
| Attachments | Encrypted blobs inside SQLite | Filesystem/S3 | Atomic with metadata, one-file backups. Tradeoff: very large media libraries make the DB big (limit configurable, default 25 MB per file). |
| CRDT | yrs 0.28 (Rust port of Yjs, update format v1), vendored as 0.28.0 with fixes for panics on crafted updates found by fuzzing (`vendor/yrs`, wired in with `[patch.crates-io]`; see `vendor/yrs/NOTEKPR.md`) | Automerge, diamond-types, custom OT | Mature, fast, compact updates, text + map types, undo manager, sticky indices for cursors; binary compatible with the Yjs ecosystem. |
| Document model | One `Y.Text` with the Markdown source per note; one `Y.Map` index per vault | Rich-text CRDT (ProseMirror/XmlFragment) with Markdown export | Markdown stays the canonical format, so external editors (Neovim, Obsidian) map to text diffs and nothing proprietary is needed for portability. |
| Symmetric crypto | XChaCha20-Poly1305 (RustCrypto, audited) | AES-256-GCM | 192-bit random nonces are safe without counters across many devices; fast in software/WASM. |
| Public-key crypto | HPKE RFC 9180 (X25519, HKDF-SHA256, ChaCha20-Poly1305) via `hpke`; Ed25519 (`ed25519-dalek`, strict) | libsodium sealed boxes, age | Standardized construction with domain-separated info/AAD. Suite id in every envelope allows migrating to X-Wing (ML-KEM hybrid, already in the `hpke` crate) later. |
| KDF / hashing | HKDF-SHA256 for subkeys, BLAKE3 derive-key for fingerprints | — | Standard, fast. |
| Auth (people) | OIDC authorization code + PKCE (`openidconnect` crate, vendored as 4.0.1 on `p256`/`p384` 0.14 for a `crypto-bigint` advisory: `vendor/openidconnect`) | Built-in passwords (forbidden by spec), SAML | Spec requirement; works with any compliant IdP. |
| Auth (CLI) | RFC 8628 device authorization grant served by our server | Loopback redirect (RFC 8252) | Works over SSH and headless machines; approval happens in the already signed-in web app. |
| Auth (agents) | OAuth 2.0 client credentials (RFC 6749 §4.4), per-agent revocable secrets | Long-lived API keys | Standard, short-lived tokens (1 h), rotation without downtime. |
| Web UI framework | Leptos 0.8 (CSR) | Yew, Dioxus | Fine-grained reactivity, small output, active project. |
| Editor | Custom `contenteditable` editor (`crates/editor`) with a CodeMirror-like model: transactions over a line-indexed buffer, line-granular re-rendering | CodeMirror 6, ProseMirror, Lexical | No JS dependencies allowed. Live-preview styling with the full source always in the DOM keeps caret mapping exact and CRDT edits precise. |
| Markdown rendering | pulldown-cmark (CommonMark + GFM tables, task lists, strikethrough, footnotes, wiki links) | comrak | Fast, pure Rust, wasm-friendly. Raw HTML is rendered as text (no sanitizer needed). |
| Search | Client-side scan with simple scoring over decrypted notes | Tantivy in WASM, server-side index | E2EE forbids server indexing; ordinary vaults (thousands of notes) search instantly with a scan. Tantivy is an option for large desktop vaults. |
| Reverse proxy / TLS | any proxy the operator chooses (optional examples: Caddy, Traefik, nginx) | Built-in TLS | Keeps the server simple; TLS is mandatory in production (spec) and handled at the edge. |
| Agent access | Agents use the client with their own identity: CLI commands, a synced Markdown folder, or the local HTTP gateway | A local MCP server (shipped at first, then removed); MCP or plaintext APIs on the sync server | Files and the CLI are better tools than tool calls for agents that can run programs; a local MCP server only duplicated them, and anything on the sync server would break zero knowledge. A remote MCP connector for agents that cannot install anything is a candidate for later ([agents.md §8](agents.md#8-why-there-is-no-mcp-server)). |
| License | AGPL-3.0-or-later | MIT/Apache-2.0, MPL-2.0 | Protects users of hosted instances (source must be offered), compatible with all dependencies (MIT/Apache/BSD/ISC/Zlib). Tradeoff: some companies avoid AGPL; the core crate could be relicensed under MPL-2.0 if third-party integrations need it. |

## 3. Data model

* **Vault** — id (random), owner, members with roles (owner/editor/viewer),
  key epoch, sequence head, `encrypted_meta` (name, icon, picture; the
  format also has a description no client sets yet).
* **Vault index object** (`object_id = "index"`): CRDT map
  `entry id → {path, kind: note|attachment|folder, created, modified, trashed?, size?, mime?, icon?, order?}`
  (`order`: a fractional key for a place set by hand, so a move rewrites
  only the moved entry and merges like any other change).
  Paths, folder structure, file names and the trash all live here, encrypted.
  Renames and moves are map writes, so concurrent offline reorganisations merge.
* **Note objects**: one per note, CRDT text holding the Markdown source.
* **Attachments**: encrypted blobs addressed by a random id; their name and
  type are in the index.
* **Per-user private data** (favorites, recent notes, key pins, note
  widths, and a counter that goes up with every save so an older copy handed
  out by the server is noticed): one blob encrypted with the user's account
  key.
* **Quick notes**: each person's own vault of the kind `quick_notes` —
  encrypted, synced and mirrored like any vault, but never shared, flat and
  holding a limited number of notes (protocol.md §5.3). Apps show it as
  *Quick notes*, never as a vault; a note leaves it by moving to a vault
  (a copy there, then the original deleted for good).

The server sees: how many objects a vault has, their sizes and change times,
who wrote each update, and membership. It does not see names, paths, folder
structure, tags, links or content (see [security.md](security.md) for the
full metadata analysis).

## 4. Key management

```
Recovery key (256-bit, shown once) ─┐
Device key (X25519 per device) ─────┼── wraps ──▶ Account secret ─┬─ X25519 key (receives vault keys via HPKE)
                                    │                              ├─ Ed25519 key (self-signature, device approvals)
                                    │                              └─ symmetric key (private prefs)
Account public key ──── HPKE ─────▶ Vault key, epoch N  ─┬─ HKDF subkeys per purpose (update, snapshot, meta, …)
                                                          └─ key chain: key_N encrypts key_{N-1}
```

* **First device**: generates the account secret, a device key and a recovery
  key; uploads the public keys (self-signed), the account secret encrypted to
  the device, and the account secret encrypted under the recovery key.
* **New device**: registers its public key as *pending*. Any device holding the
  account secret approves it after the user compares a 60-bit verification code
  displayed on both screens (defeats a server substituting keys). The code
  also covers the account key the new device expects, which then only accepts
  a secret of that account. The approval
  is an account signature over `(user, device id, device key)` that the server
  verifies, so a stolen OIDC session alone cannot enroll a device.
* **Recovery**: on a new device, the recovery key decrypts the recovery
  envelope; the device then approves itself. No server-side master key exists.
* **Sharing**: the owner fetches the recipient's public key, verifies its
  self-signature, checks it against a trust-on-first-use pin (and shows the
  fingerprint for out-of-band comparison), and uploads the vault key encrypted
  to it. Recipients who have not set up keys yet become "pending keys" members
  and receive the key automatically when the owner's client next sees them.
* **Note keys**: every note has its own *document key*, wrapped under the
  vault key for members. A client creating a note registers its key before
  sending any of its content; older notes move to their own key at their next
  snapshot. The vault index and attachments stay under the vault key.
* **Membership log**: the owner signs every membership and key change into
  a hash-chained log per vault (members sign their own leaving). Clients
  verify it against the owner's pinned key, check each vault key against the
  owner's commitment, and the owner gives keys only to members the log
  vouches for — so a server can't add people or plant keys. Vaults from
  before the log start it with an owner-signed checkpoint. Authors sign
  every update and snapshot; once the log requires it, clients accept
  content only from people the log lets write it.
* **Rotation / revocation**: removing a member comes with a new epoch in the
  same request: a fresh vault key, wrapped for remaining members, plus a
  chain link so members can still read history, and a new epoch of every
  note's key. New content always uses the newest epoch. Someone who leaves
  on their own can't renew the key, so the server marks it and the owner's
  apps rotate it when they're next online. Already downloaded data cannot be
  revoked (stated in the UI).
* **Devices removed** lose their envelope and sessions immediately. Because a
  device held the account secret, a *compromised* device warrants rotating the
  shared vaults' keys (one click in vault settings).
* **Agents** are separate principals with their own account key generated on
  the agent host; they never receive a user's account secret.

## 5. Synchronization model

**Log.** Each vault has one ordered log of encrypted CRDT updates with a
per-vault sequence counter. Pushes carry a random `client_update_id`; the
server de-duplicates retries, so the at-least-once delivery used by clients is
effectively exactly-once. Writes and their WebSocket broadcast happen under a
single lock, so subscribers receive every vault's log gaplessly and in order.

**Live.** Clients keep one WebSocket, subscribe to every vault with the last
sequence they applied, receive a catch-up (`changes` pages) and then live
updates. Local edits are batched (~120 ms), encrypted and pushed; the server's
`ack` arrives in log order. Presence (cursor, open note) is relayed as
encrypted ephemeral messages.

**Uploads.** Both clients upload through the shared engine (`crates/sync`).
Queued updates of one note that were never sent are merged (an offline
session stays a handful of updates instead of one per keystroke), within the
edit budget so a deliberately split paste stays split. Updates larger than the
server's `max_update_bytes` are held back locally — never blocking the rest —
and released once the limit allows. The server refuses single bad updates of
a push (too large, purged object, outdated key) and stores the rest; the
engine turns each refusal into the follow-up (hold, drop and keep a copy,
reload keys and re-send).

**Offline.** Unacknowledged updates are kept (browser: local storage, already
encrypted; desktop: encrypted local store) and re-sent with the same ids after
reconnecting. CRDT merging makes the order irrelevant and guarantees
convergence; concurrent edits to the same note interleave instead of
overwriting.

**Local copy (browser).** The browser keeps each vault's documents in
IndexedDB: every note's full CRDT state, encrypted with the key its server
copy uses (the note's own key, or the vault key for the index and older notes; purpose `LocalCache`,
bound to vault and object), plus a cursor: the sequence
number the stored copy is complete up to. A page load shows the stored notes
right away and subscribes from the cursor, so only what changed since is
downloaded. The cursor moves only when everything changed in memory has been
written and never past content still waiting for a key; notes with unconfirmed
local edits aren't written until the server acknowledges them (until then the
edits live in the pending queue, and a stored state must never contain changes
the server could miss). One tab per account writes (a Web Lock held while it
is open), so a tab that is behind can't overwrite newer states. Notes shared
with us individually (their vault's key isn't ours) get one record each: the
note's state and how far it is complete, under the note's own key, written by
the same rules. Copies are deleted when access ends, when a vault or shared
note disappears from the account's lists, and by "Forget this browser"
(`crates/web/src/cache.rs`).

**Offline start (browser, opt-in).** Normally the browser asks the server for
its wrapped account key and the vault keys at every start. With "Keep this
browser usable offline" (Settings → Devices) it also stores those and a
service worker keeps the app's files, so when the server can't be reached the
app opens from the local copy and edits are queued. Once the socket connects
again, the normal checks run first (session, device still on the account),
then everything is refreshed. The trade-off is in [security.md](security.md)
(`crates/web/src/state/offline.rs`, `crates/web/static/sw.js`).

**Compaction and history.** A writer that has seen many updates for an object
uploads an encrypted full-state snapshot covering a sequence number. The server
prunes only updates covered by the *previous* snapshot, so one generation of
raw updates always remains as a safety net. Snapshots double as revision
history (also created after editing sessions); restoring a revision applies it
as a new edit, so history is never rewritten.

**Rollback detection.** Clients remember the highest sequence the server ever
reported. If the server reports less (restored backup or tampering), clients
warn and re-push their full state; CRDT merging restores anything missing.

**Deletion.** Deleting moves entries to the trash (an index field). Only
"delete forever" purges data. If someone purges a note while another person
has unsent edits to it, that person's client saves their version as a new
"(recovered)" note instead of dropping it. A purge can be undone for a while
(`--purge-undo-days`): the server hides the note at once but keeps its data,
and the index keeps its entry marked `purged`; undoing it puts the entry
back in the trash and clients fetch the note again (`restored`). After that
period the server deletes the data and an admin's client drops the entry.

## 6. Markdown portability and external editors

* Notes *are* Markdown text; export writes the vault tree as `.md` files plus
  attachments (zip in the browser, directory or zip from the CLI). Import does
  the reverse. Wiki links (`[[Note]]`, `[[Folder/Note#Heading|alias]]`),
  embeds (`![[image.png]]`), `#tags` and YAML frontmatter tags follow
  Obsidian conventions, so an Obsidian vault can be imported and exported.
* The desktop client can mirror a vault as ordinary files (explicit plaintext
  mode). Its reconciliation:
  - remote change, file untouched → write the new text atomically;
  - file changed on disk → find the base version the external editor most
    likely loaded (closest of the last few written versions), fork the CRDT at
    that base with a fresh client id, apply the text diff there, and merge into
    the live document. A stale save from Neovim therefore *adds* its changes
    without deleting concurrent remote edits;
  - deletions become trash entries (recoverable), delete+create of identical
    content becomes a rename.
* Agents and the gateway also write through text diffs, never whole-document
  replacement.

## 7. Browser client specifics

* State lives in a non-reactive core (keys, replicas, socket) plus Leptos
  signals for rendering. The core is taken only at the edges — event
  handlers, socket messages, timers, each step of an async task, views —
  and passed explicitly to everything they call, so nothing can take it
  twice; work that calls back synchronously (the editor, DOM calls that
  fire events) runs at the edges, outside it (`crates/web/src/state/mod.rs`).
* The editor (`crates/editor`) renders every source character, one `div.ln`
  per line; syntax markers get a class that CSS hides on lines without the
  caret (live preview). Every edit is a transaction on the document model —
  a change set (compose / transform / map positions), the selection after it,
  its origin and undo grouping — and the view re-renders only the lines it
  replaced plus those whose Markdown block changed (blocks are re-classified
  incrementally from the edit until the parser state matches again). Typing
  is handled via `beforeinput`; anything the browser does on its own (IME
  composition, mobile keyboards, spellcheck) is seen by a `MutationObserver`,
  read back line by line and rendered again, so the DOM can never silently
  diverge from what is synced. During an IME composition the DOM is left to
  the browser: remote text waits and is merged (operational transform) with
  the composed text when it ends.
* Lines right under a list item or quote that start no block of their own
  are shown as part of it — Markdown's lazy continuation lines, which every
  renderer folds into the item or quote, even when they look like a table —
  so the editor and the reading view agree. Ending a list or quote with Enter
  leaves a blank line for the same reason.
* Callouts (`> [!tip] Title`, `crates/editor/src/callout.rs`) are block
  quotes whose first line names a type. The line state after a quote line
  carries how deeply quotes nest, the callouts among them and whether a lazy
  line may follow, so every line of a callout (nested ones too) knows it is
  one and which; a line also looks at the next to know it is its callout's
  last. The first line's `> [!type]` is syntax like any marker; a callout
  without a title shows its type as one, in a widget. The reading view finds
  callouts in the parser's events (a quote whose first paragraph starts on
  its first line with the type) and renders a box, or a `details` element
  for one that folds (`+`, `-`).
* Dates (`crates/core/src/dates.rs`) are `@2026-10-23` in the text: typing
  `@today`, `@tomorrow` or `@yesterday` puts that day's date there (the
  editor when the word ends, or from the `@` menu it shows by the caret;
  the CLI and the gateway as they write; never the Markdown folder, whose
  files belong to their editor). The day is fixed in the note, so it never
  drifts, and every other tool sees an ISO date. Like other syntax, a
  date's source hides away from the caret beside a widget saying Today,
  Tomorrow, Yesterday or the date in the reader's language; lines with
  dates count as dynamic, and the app refreshes them (and the reading
  view) when the day changes.
* Code blocks (`crates/editor/src/code.rs`) are highlighted by a small
  hand-written lexer per language, a line at a time: what a line leaves open
  (a block comment, a multi-line string) is part of the next line's state, so
  the incremental re-classification above also re-highlights exactly the
  lines it affects. The fence's options (`title=`, `hl=`, `ln=false`,
  `vl=false`, `wrap=`) are part of the block too. The header (file name,
  language, copy button) is a `contenteditable=false` widget after the
  opening fence's text, line numbers are a CSS counter and indentation guides
  an empty widget per line (the block's indentation step travels with the
  line state too): none of them is part of the text. A line of code is a
  span (`.lx`) holding its indentation and the rest, so wrapped rows start
  under the text. When long lines don't wrap, each `.lx` scrolls sideways;
  the view keeps a block's lines at the same scroll position and as wide as
  its longest line, and scrolls them to the caret. The reading view uses
  the same parser and highlighter.
* A table's rows are lines laid out by CSS as table rows (consecutive rows
  share an anonymous table box, so columns line up); each cell is a span
  (`.td`) holding its text in another (`.tc`), which CSS sizes. Words in a
  cell never break, and on a phone a cell's text keeps its width up to
  12em before it wraps. A table that doesn't fit makes the editor's text
  (`.ed-wrap`, with the cursors and comment highlights drawn over it) scroll
  sideways, while the other lines still wrap at the editor's width; the view
  scrolls it to the caret. In the reading view each table scrolls on its own.
* The editor's text buffer is a `String` with a line index rather than a rope:
  the host (`edit(base, edits)`), the CRDT and remote updates all work on
  whole texts, so a rope would be flattened on every keystroke anyway; an
  edit costs one tail `memmove` and shifting later line starts.
* Each browser tab keeps its own session (sessionStorage), seeded from the last
  login in local storage. In development mode this lets several people be
  signed in side by side in one browser.

## 8. Accommodating future work

* **Mobile apps**: the core crate already compiles for every Rust target; a
  mobile client would wrap the same engine (e.g. via UniFFI) with an internal
  encrypted store, exactly like the desktop client minus the plaintext mirror.
  The web client is responsive and usable on phones today.
* **Sharing single notes** (implemented): each note is its own CRDT and
  ciphertext with per-object AAD under its own *document key* (epochs with the
  top bit set, so every ciphertext says which key it needs). Sharing starts a
  fresh epoch with no chain link to the earlier ones, wrapped with HPKE for
  each recipient; a snapshot under it gives recipients the current content,
  and they can neither receive nor decrypt the note's earlier history
  (the server hands out content from that epoch on). Recipients subscribe to just
  that note, the server returns them only document-key payloads, and refuses
  writes under outdated keys (`stale_key`) so clients re-encrypt. Content that
  arrives before its key is kept and replayed once the key is loaded. See
  protocol.md §5.1.
* **Sharing folders**: not planned (decided 2026-10-20) — sharing a vault
  covers it. It would have needed a folder key and a second, folder-only index.
* **Sharing between instances** (planned): independent instances sharing
  vaults or notes with each other's users. To keep it possible, ids stay
  random and globally unique, people are identified by their account keys
  (instance-qualified user ids later), one instance owns each vault's ordered
  log, and membership is verifiable without trusting a server (the signed
  membership log).
* **Signed updates** (implemented): every update and snapshot carries its
  author's signature, so clients verify authorship even against a colluding
  server (security.md, residual risks).
* **Post-quantum key wrapping**: switch the HPKE KEM to X-Wing via the version
  byte; vault content encryption is already 256-bit symmetric.

## 9. Milestones

| Milestone | Scope | Status |
|---|---|---|
| M1 Core | crypto formats, key hierarchy, CRDT model, protocol | done (`cargo test -p notekpr-core`) |
| M2 Server | auth (OIDC, dev, device flow, client credentials), vaults, sharing, log, snapshots, attachments, WebSocket, ops | done (`cargo test -p notekpr-server`) |
| M3 Web client | onboarding, devices, recovery, editor, collaboration, sharing, history, search, import/export, settings, admin | done (browser e2e tests in `tests/e2e`) |
| M4 Desktop client | login, enrollment, offline replica, live sync, Markdown mirror, export/import | done (`cargo test -p notekpr-client`) |
| M5 Agents | agent identities, CLI and Markdown-folder access, gateway | done (`crates/client`); local MCP server removed, remote MCP connector considered for later |
| M6 Packaging | Docker image, compose, Kubernetes, backup/restore docs | done |
| M7 Note sharing | share single notes outside a vault: document keys, per-note subscriptions, web + CLI | done (S `individual_note_sharing`, D `single_note_sharing_end_to_end`, E `test_note_sharing`) |
| Next | native desktop GUI shell (Tauri around the web UI), mobile apps | planned |

Acceptance criteria and how each is verified: [acceptance.md](acceptance.md).
