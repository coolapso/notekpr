# Architecture and design decisions

> **Principle:** the server coordinates; clients own the data and the keys.

This document describes how coolpsonotes is built, why, and which
alternatives were considered. The wire protocol is in
[protocol.md](protocol.md) and the threat model in [security.md](security.md).

## 1. Components

```
                 ┌────────────────────────── trusted clients ───────────────────────────┐
                 │                                                                      │
 Browser ──────▶ │  Web client (Rust → WebAssembly, Leptos)                             │
                 │  Desktop client `coolpsonotes` (Rust): sync daemon, Markdown mirror  │
 AI agents ────▶ │  same client: CLI commands, a Markdown folder, local HTTP gateway    │
                 │                                                                      │
                 │      all of them link coolpsonotes-core: crypto · keys · CRDT ·       │
                 │      protocol types · Markdown analysis · search                      │
                 └──────────────────────────────┬───────────────────────────────────────┘
                                                │ HTTPS + WebSocket (TLS at the proxy)
                                                │ ciphertext, public keys, metadata only
                                     ┌──────────▼───────────┐       ┌──────────────┐
                                     │ coolpsonotes-server  │◀─────▶│ OIDC provider │
                                     │ (Rust, axum, SQLite) │       │ (Authelia,    │
                                     └──────────────────────┘       │ Keycloak, …)  │
                                                                    └──────────────┘
```

| Crate | Role |
|---|---|
| `crates/core` | Everything security-sensitive or shared: cryptographic formats, key hierarchy, the CRDT document model (vault index + notes), protocol types, client engine logic (`engine.rs`), Markdown link/tag analysis, search, path rules. Compiled natively and to wasm32. |
| `crates/server` | Headless sync server: OIDC relying party, sessions, OAuth device and client-credentials grants, users/devices/agents, vaults/members/key envelopes, the ordered encrypted log, snapshots, attachments, WebSocket fan-out, rate limiting, metrics, backup/restore. Works without the web client. |
| `crates/web` | Browser client compiled to WebAssembly; served as static files by the server. Custom Markdown editor, all decryption in the browser. |
| `crates/client` | Desktop/CLI client: login (device flow), enrollment, encrypted local replica, live sync daemon with plaintext Markdown mirror, export/import, agent gateway (local HTTP API). |

The server never links any code path that could decrypt content: it only
handles opaque byte strings and public keys.

## 2. Technology decisions

| Area | Choice | Alternatives considered | Why |
|---|---|---|---|
| Language | Rust everywhere, including the browser (WebAssembly) | TypeScript web client (Svelte/React + CodeMirror + Yjs) | Mandated for the core; using it in the browser too means a single implementation of crypto and CRDT logic for every client, no npm supply chain, and no JS/Rust format drift. Cost: we wrote our own editor instead of reusing CodeMirror. |
| Server framework | axum 0.8 + tokio + tower-http | actix-web, warp | Mature, minimal, first-class WebSockets, tower middleware ecosystem. |
| Database | SQLite (WAL) via sqlx, single file — **SQLite only, by decision**, also for a hosted offering (one database per instance; grow by adding instances) | PostgreSQL | Zero extra services for self-hosting; a single file is a complete, consistent backup (`VACUUM INTO`); a notes workload with no server-side content processing fits one SQLite instance for a very large number of users. The SQL is SQLite-specific (e.g. `VACUUM INTO`, `BEGIN IMMEDIATE`). Storage access is to move behind small interfaces (see [TODO.md](../TODO.md)) so the decision stays cheap to revisit, but there are no plans for another database. |
| Attachments | Encrypted blobs inside SQLite | Filesystem/S3 | Atomic with metadata, one-file backups. Tradeoff: very large media libraries make the DB big (limit configurable, default 25 MB per file). |
| CRDT | yrs 0.28 (Rust port of Yjs, update format v1) | Automerge, diamond-types, custom OT | Mature, fast, compact updates, text + map types, undo manager, sticky indices for cursors; binary compatible with the Yjs ecosystem. |
| Document model | One `Y.Text` with the Markdown source per note; one `Y.Map` index per vault | Rich-text CRDT (ProseMirror/XmlFragment) with Markdown export | Markdown stays the canonical format, so external editors (Neovim, Obsidian) map to text diffs and nothing proprietary is needed for portability. |
| Symmetric crypto | XChaCha20-Poly1305 (RustCrypto, audited) | AES-256-GCM | 192-bit random nonces are safe without counters across many devices; fast in software/WASM. |
| Public-key crypto | HPKE RFC 9180 (X25519, HKDF-SHA256, ChaCha20-Poly1305) via `hpke`; Ed25519 (`ed25519-dalek`, strict) | libsodium sealed boxes, age | Standardized construction with domain-separated info/AAD. Suite id in every envelope allows migrating to X-Wing (ML-KEM hybrid, already in the `hpke` crate) later. |
| KDF / hashing | HKDF-SHA256 for subkeys, BLAKE3 derive-key for fingerprints | — | Standard, fast. |
| Auth (people) | OIDC authorization code + PKCE (`openidconnect` crate) | Built-in passwords (forbidden by spec), SAML | Spec requirement; works with any compliant IdP. |
| Auth (CLI) | RFC 8628 device authorization grant served by our server | Loopback redirect (RFC 8252) | Works over SSH and headless machines; approval happens in the already signed-in web app. |
| Auth (agents) | OAuth 2.0 client credentials (RFC 6749 §4.4), per-agent revocable secrets | Long-lived API keys | Standard, short-lived tokens (1 h), rotation without downtime. |
| Web UI framework | Leptos 0.8 (CSR) | Yew, Dioxus | Fine-grained reactivity, small output, active project. |
| Editor | Custom `contenteditable` editor (`crates/web/src/editor`) | CodeMirror 6, ProseMirror, Lexical | No JS dependencies allowed. Live-preview styling with the full source always in the DOM keeps caret mapping exact and CRDT edits precise. |
| Markdown rendering | pulldown-cmark (CommonMark + GFM tables, task lists, strikethrough, footnotes, wiki links) | comrak | Fast, pure Rust, wasm-friendly. Raw HTML is rendered as text (no sanitizer needed). |
| Search | Client-side scan with simple scoring over decrypted notes | Tantivy in WASM, server-side index | E2EE forbids server indexing; ordinary vaults (thousands of notes) search instantly with a scan. Tantivy is an option for large desktop vaults. |
| Reverse proxy / TLS | Caddy example (automatic HTTPS); any proxy works | Built-in TLS | Keeps the server simple; TLS is mandatory in production (spec) and handled at the edge. |
| Agent access | Agents use the client with their own identity: CLI commands, a synced Markdown folder, or the local HTTP gateway | A local MCP server (shipped at first, then removed); MCP or plaintext APIs on the sync server | Files and the CLI are better tools than tool calls for agents that can run programs; a local MCP server only duplicated them, and anything on the sync server would break zero knowledge. A remote MCP connector for agents that cannot install anything is a candidate for later ([agents.md §8](agents.md#8-why-there-is-no-mcp-server)). |
| License | AGPL-3.0-or-later | MIT/Apache-2.0, MPL-2.0 | Protects users of hosted instances (source must be offered), compatible with all dependencies (MIT/Apache/BSD/ISC/Zlib). Tradeoff: some companies avoid AGPL; the core crate could be relicensed under MPL-2.0 if third-party integrations need it. |

## 3. Data model

* **Vault** — id (random), owner, members with roles (owner/editor/viewer),
  key epoch, sequence head, `encrypted_meta` (name, description).
* **Vault index object** (`object_id = "index"`): CRDT map
  `entry id → {path, kind: note|attachment|folder, created, modified, trashed?, size?, mime?}`.
  Paths, folder structure, file names and the trash all live here, encrypted.
  Renames and moves are map writes, so concurrent offline reorganisations merge.
* **Note objects**: one per note, CRDT text holding the Markdown source.
* **Attachments**: encrypted blobs addressed by a random id; their name and
  type are in the index.
* **Per-user private data** (favorites, recent notes, key pins): one blob
  encrypted with the user's account key.

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
  displayed on both screens (defeats a server substituting keys). The approval
  is an account signature over `(user, device id, device key)` that the server
  verifies, so a stolen OIDC session alone cannot enroll a device.
* **Recovery**: on a new device, the recovery key decrypts the recovery
  envelope; the device then approves itself. No server-side master key exists.
* **Sharing**: the owner fetches the recipient's public key, verifies its
  self-signature, checks it against a trust-on-first-use pin (and shows the
  fingerprint for out-of-band comparison), and uploads the vault key encrypted
  to it. Recipients who have not set up keys yet become "pending keys" members
  and receive the key automatically when the owner's client next sees them.
* **Rotation / revocation**: removing a member triggers a new epoch: a fresh
  vault key, wrapped for remaining members, plus a chain link so members can
  still read history. New content always uses the newest epoch. Already
  downloaded data cannot be revoked (stated in the UI).
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

**Offline.** Unacknowledged updates are kept (browser: local storage, already
encrypted; desktop: encrypted local store) and re-sent with the same ids after
reconnecting. CRDT merging makes the order irrelevant and guarantees
convergence; concurrent edits to the same note interleave instead of
overwriting.

**Local copy (browser).** The browser keeps each vault's documents in
IndexedDB: every note's full CRDT state, encrypted with the key its server
copy uses (the vault key, or a shared note's own key; purpose `LocalCache`,
bound to vault and object), plus a cursor: the sequence
number the stored copy is complete up to. A page load shows the stored notes
right away and subscribes from the cursor, so only what changed since is
downloaded. The cursor moves only when everything changed in memory has been
written and never past content still waiting for a key; notes with unconfirmed
local edits aren't written until the server acknowledges them (until then the
edits live in the pending queue, and a stored state must never contain changes
the server could miss). One tab per account writes (a Web Lock held while it
is open), so a tab that is behind can't overwrite newer states. Copies are
deleted when access ends, when a vault
disappears from the account's list, and by "Forget this browser"
(`crates/web/src/cache.rs`).

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
"(recovered)" note instead of dropping it.

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
  signals for rendering.
* The editor renders every source character; syntax markers get a class that
  CSS hides on lines without the caret (live preview). Typing is handled via
  `beforeinput`; anything the browser does on its own (IME composition, mobile
  keyboards) is reconciled by diffing DOM text against the model, so the DOM can
  never silently diverge from what is synced.
* Each browser tab keeps its own session (sessionStorage), seeded from the last
  login in local storage. In development mode this lets several people be
  signed in side by side in one browser.

## 8. Accommodating future work

* **Mobile apps**: the core crate already compiles for every Rust target; a
  mobile client would wrap the same engine (e.g. via UniFFI) with an internal
  encrypted store, exactly like the desktop client minus the plaintext mirror.
  The web client is responsive and usable on phones today.
* **Sharing single notes** (implemented): each note is its own CRDT and
  ciphertext with per-object AAD, so a note can move to its own *document key*
  (epochs with the top bit set, so every ciphertext says which key it needs).
  The key is wrapped under the vault key for members and with HPKE for each
  recipient; a snapshot under the new key gives recipients the current content
  without the note's earlier, vault-key history. Recipients subscribe to just
  that note, the server returns them only document-key payloads, and refuses
  writes under outdated keys (`stale_key`) so clients re-encrypt. Content that
  arrives before its key is kept and replayed once the key is loaded. See
  protocol.md §5.1.
* **Sharing folders**: builds on the same mechanism — a shared folder would get
  its own key and a small index object listing only its entries, with its notes
  under that key.
* **Sharing between instances** (much later): independent instances sharing
  vaults, folders or notes with each other's users. To keep it possible, ids
  stay random and globally unique, people are identified by their account
  keys (instance-qualified user ids later), one instance owns each vault's
  ordered log, and membership is to become verifiable without trusting a
  server (signed membership log). Details and guardrails in
  [TODO.md](../TODO.md#other).
* **Signed updates**: updates could carry an Ed25519 signature by the author's
  account key, letting clients verify authorship even against a colluding
  server (see security.md, residual risks).
* **Post-quantum key wrapping**: switch the HPKE KEM to X-Wing via the version
  byte; vault content encryption is already 256-bit symmetric.

## 9. Milestones

| Milestone | Scope | Status |
|---|---|---|
| M1 Core | crypto formats, key hierarchy, CRDT model, protocol | done (`cargo test -p coolpsonotes-core`) |
| M2 Server | auth (OIDC, dev, device flow, client credentials), vaults, sharing, log, snapshots, attachments, WebSocket, ops | done (`cargo test -p coolpsonotes-server`) |
| M3 Web client | onboarding, devices, recovery, editor, collaboration, sharing, history, search, import/export, settings, admin | done (browser e2e tests in `tests/e2e`) |
| M4 Desktop client | login, enrollment, offline replica, live sync, Markdown mirror, export/import | see `crates/client` |
| M5 Agents | agent identities, CLI and Markdown-folder access, gateway | done (`crates/client`); local MCP server removed, remote MCP connector considered for later |
| M6 Packaging | Docker image, compose, Kubernetes, backup/restore docs | done |
| M7 Note sharing | share single notes outside a vault: document keys, per-note subscriptions, web + CLI | done (S `individual_note_sharing`, D `single_note_sharing_end_to_end`, E `test_note_sharing`) |
| Next | native desktop GUI shell (Tauri around the web UI), mobile apps, folder sharing, signed updates, offline start of the web app | planned |

Acceptance criteria and how each is verified: [acceptance.md](acceptance.md).
