# TODO

Everything that is known to need doing, in the order it should be done.
It comes from the full review of 2026-10-19 (four parallel reviews: docs and
deployment, server and core, web client, CLI client) and the decisions taken
after it.

**IDs** point back to the review: `D` docs/deployment, `S` server/core, `W` web
client, `C` CLI client, then severity (`H`igh, `M`edium, `L`ow) and number.
**✔** = confirmed by hand (reproduced or re-read), the rest are code traces
from the review. File references are as of commit `68638d5`.

Several findings disappear with one of the redesigns in [Redesign
tracks](#redesign-tracks). Those findings are listed under their track and
not repeated in the phases, unless they are too dangerous to wait — then the
phase has an *interim* fix and the track owns the real one.

## Decisions (2026-10-19)

- **SQLite only**, also for a hosted offering: one database per instance, grow
  by adding instances. No Postgres. The cheap foundation that keeps the
  choice revisitable is track [R4](#r4-storage-foundation-sqlite-stays).
- **The local MCP server is removed** (done). Agents use the CLI, a synced
  Markdown folder or the local gateway. A *remote* MCP connector for agents
  that can't install anything is an idea for later — reasoning in
  [docs/agents.md §8](docs/agents.md#8-why-there-is-no-mcp-server), item in
  [Later](#later-and-ideas).
- **Nobody's notes may ever be locked in**: an offline recovery tool that
  needs neither the server nor the apps ([Phase 3](#phase-3--escape-hatch-offline-recovery)).
- **"Use your own editor" is a core product principle** — the Markdown folder
  stays a first-class way to use coolpsonotes, even if a native desktop app
  arrives. coolpsonotes doubles as an end-to-end encrypted sync server for an
  Obsidian vault.
- The **shared sync engine** ([R1](#r1-one-sync-engine-for-all-clients--separate-session))
  gets its own session.

## Done

- [x] Removed the stdio MCP server (`coolpsonotes mcp`) and its tests; docs
      explain why and describe the remote connector idea.
- [x] Guide for using your own editor with an always-running mirror:
      [docs/own-editor.md](docs/own-editor.md) (incl. known mirror limitations).
- [x] Recorded the SQLite-only and agent-access decisions in
      [docs/architecture.md](docs/architecture.md) (also fixes its inaccurate
      "Postgres can be added behind the same queries" [D-L9]).
- [x] Corrected the false claim that a copy of the CLI's data directory
      reveals nothing (`local.key` lives in it) [C-H9 ✔, docs part].

---

## Phase 1 — before real use (quick, high value)

Small fixes; roughly half a day together. Do these before putting real notes
in it.

### Crashes and leaks

- [x] **Deleted text leaks into snapshots** [S-H1 ✔] — with undo enabled (the
      web editor), `NoteDoc::encode_state` keeps deleted text, so the snapshot
      sent to people a single note is shared with contains text deleted
      before sharing. Encode snapshots from a reloaded copy
      (`NoteDoc::from_updates([state]).encode_state()`) in `start_sharing` and
      `snapshot_request` (`core/src/engine.rs`); core test: delete with undo
      on, share, assert the bytes are gone.
- [x] **`percent_decode` panics on `%` + multibyte** [W-H4 ✔] —
      `core/src/markdown.rs:274` slices a `&str` mid-character; a link like
      `[x](50%増.md)` crashes the editor and reading view for everyone. Decode
      on bytes.
- [ ] **Formatting shortcuts panic next to multibyte text** [W-H3 ✔] —
      `web/src/editor/mod.rs:825`; use `text.get(..)`.
- [ ] **IME commit double-borrows the editor state** [W-H1 ✔] —
      `web/src/editor/mod.rs:1256`; take `pending_remote` before `set_text`.
      Otherwise the editor dies and typing is silently lost.
- [ ] Vim visual-mode anchor not clamped after remote edits shrink the text
      [W-L1] — `web/src/editor/vim.rs:247`.
- [x] **Per-note WebSocket subscriptions survive removal from the vault**
      [S-H2] — `server/src/ws.rs` `RevokeVault` only drops `(v, None)`. Drop
      member-granted `(v, Some(_))` subscriptions too.
- [x] Recipients can miss the share snapshot [S-M3] — apply the document-key
      filter inside the latest-snapshot subquery (`server/src/sync.rs`
      `changes_since`) and require `covers_seq >= max(snapshots.covers_seq)` in
      `set_key`. (R2 removes the first-share snapshot; this is cheap now.)
- [x] **OIDC login CSRF** [S-M6] — bind `state` to the browser with an
      HttpOnly SameSite=Lax nonce cookie (`server/src/routes/auth.rs`).
- [ ] Links in the live editor open any URL scheme [W-L4] — reuse
      `render::safe_external` in `NoteHost::open_link`.
- [ ] Attachments open as top-level `blob:` documents in the app's origin
      (SVG/HTML could run as the app) [W-M2] — download non-raster types or
      serve them as `application/octet-stream`; add `noopener`.
- [ ] Clear decrypted caches (`ANALYSIS`, `LINKS`) and revoke attachment blob
      URLs on sign-out [W-L5, part].

### Deployment configuration

- [x] **Dev and prod compose files share project and service name** [D-H1 ✔]
      — `docker compose -f docker-compose.dev.yml up` on the prod host replaces
      prod with a dev-mode server behind your domain. Add `name:` to both
      files; never share a project between them.
- [x] **Dev-mode safety** [S-L10, D-L10] — refuse `--dev-mode` with an https
      public URL, OIDC or a non-loopback listen unless an explicit override
      flag; revoke dev-issued sessions at startup when dev mode is off; dev
      users never become the first admin.
- [x] OIDC token endpoint auth: only `client_secret_basic` works although the
      docs promise `client_secret_post` [D-H2 ✔] — make it configurable
      (`server/src/oidc.rs`), fix the docs either way.
- [x] IdP certificates: only bundled Mozilla roots are trusted, so private-CA
      IdPs (typical homelab) fail [D-H3] — use the platform verifier / native
      roots or add a CA-bundle option.
- [x] Empty env vars count as set [D-M2] — treat `""` as unset in
      `server/src/config.rs`; remove `COOLPSONOTES_METRICS_TOKEN: ""` from the
      Kubernetes manifest.
- [x] `/metrics` is public through Caddy by default [D-M1, S-L10] — block it
      in the Caddyfile, add `METRICS_TOKEN` to `.env.example`, document it.
- [x] `.dockerignore` misses `.env` and `.claude/` [D-L4].

### Docs needed for a first local deployment

- [x] A LAN/homelab path [gap 1]: Caddy `tls internal` (and trusting its root
      CA) or DNS-01, and a Caddy-less compose variant behind an existing proxy.
- [x] Identity-provider requirements [gap 2, D-M3]: issuer reachable from the
      container at the same URL as from the browser; publicly trusted
      certificate (until D-H3); name/e-mail claims in the ID token (Authelia
      `claims_policy` example) — or add a userinfo fallback.
- [x] Backups for compose [D-M4, gap 5]: scheduling, copying backups off the
      volume, a restore procedure (and for Kubernetes), `--data-dir` in the
      host example, `--out` needs an existing directory, what a restore loses
      (sessions, devices, memberships created later).
- [x] Upgrades [D-M5]: exact commands (`git pull && docker compose build --pull
      && docker compose up -d`).
- [x] First admin [D-L10, gap 6]: sign in yourself first or set `ADMINS`; how
      to find your `issuer|subject`; removing from `ADMINS` doesn't revoke;
      never reuse a dev-mode data directory.
- [x] From-source deployment [D-M7]: serve a web build outside the checkout
      (`build-web.sh` replaces `dist/web`), secret via env file not argv,
      `--trust-proxy`, `--locked`, a systemd unit with its own user.
- [x] Small fixes: `login --dev-user` needs `--server` on first login [D-M6];
      env var names for every flag [D-L11]; `X-Forwarded-For` warning [D-L1];
      plain-HTTP-on-LAN caveats (CLI `--insecure-http`, clipboard needs a
      secure context) [gap 4]; bind-mount ownership (uid 65532) [gap 5].

---

## Phase 2 — data safety

Before trusting the Markdown mirror and the CLI with real notes.

### Markdown mirror safety (`crates/client/src/mirror.rs`)

- [ ] **A missing, moved or unmounted mirror root trashes every note**
      [C-H3 ✔] — don't recreate a missing remembered root (refuse to start);
      pause and ask when a vault directory or more than N files/N% disappear
      in one pass; never apply mass deletes in a final pass.
- [ ] **Reverting a file is overwritten** with the newer text [C-H1] —
      `choose_base` picks an older base at cost 0; track which bases were read
      from disk vs written by us.
- [ ] A save during a remote write can be overwritten unseen [C-M3] — use the
      pre-read stat, re-check right before the rename, keep and ingest the
      replaced file if it differs.
- [ ] Edited locally while trashed remotely: the file is deleted [C-M4].
- [ ] Leftover mirror files are imported into a different vault with the
      same name [C-M5] — use a suffixed directory or ask.
- [ ] One bad file aborts whole passes and the daemon start [C-M2] — handle
      errors per entry; shorter temp names.
- [ ] Mirror task failure is silent [C-M13] — `sync` must notice and report.
- [ ] Case-insensitive filesystems (macOS, Windows): duplicates, ping-pong
      corruption, two vaults in one directory [C-H6].
- [ ] Windows: vault paths can escape the mirror/export directory (`C:` in a
      segment) [C-H7] — reject drive prefixes, reserved names and characters;
      check every joined component.
- [ ] Replacing a synced file with a symlink trashes the note [C-L7].
- [ ] `plaintext_ok` is remembered per profile, not per directory; plain
      `sync` silently reuses the remembered mirror [C-L3].
- [ ] Ingest calls `server_info()` over the network while holding the vault
      lock [C-L12].

### CLI client data loss

- [ ] **A one-shot command and the running daemon overwrite each other's
      replica files** [C-H2, C-L9] — per-profile `flock`; one-shot commands
      wait, refuse, or hand work to the daemon.
- [ ] **Login deletes the only account secret on HTTP 403/404/409** [C-M8]
      (`client/src/account.rs:219-232`) — act only on an explicit "device
      revoked" code; move aside, never delete; ask.
- [ ] `Vault::save` partial failure loses remote updates [C-M6] — remove from
      the dirty set only after a successful save.
- [ ] A missing `local.key` is silently regenerated, making every replica
      unreadable [C-L5].
- [ ] First device uploads its account key before saving secrets locally
      [C-L4]; same in the web client's `setup_keys` [W-L10].
- [ ] `logout --forget` and `vault leave` drop unpushed changes and leave the
      plaintext mirror [C-M11] — push first or warn; offer to remove the files.
- [ ] Transient errors end live sync; the gateway then serves stale data
      [C-M1] — reconnect instead of exiting; surface a dead live task.
- [ ] Live uploads stop after a reconnect if any vault is read-only [C-H4].
- [ ] Offline fallback doesn't treat HTTP 5xx as offline [C-M10].
- [ ] `import --skip-existing` compares paths case-sensitively; add
      `--dry-run` [C-L1]. Export writes `0644`, leaves `.partial` and temp
      files [C-L2].

### Sync robustness in both clients (interim — R1 then owns them)

- [ ] **Oversized changes block syncing forever** [W-H5, C-H5] — chunk pushes
      by bytes (`max_update_bytes` from `/info`, cached offline), refuse or
      split oversized edits and imports, quarantine a rejected item and keep
      going, show the error.
- [x] A snapshot can claim to cover updates still waiting for a key
      [S-M2, W-L9] — fix once in core (`snapshot_request`,
      `compaction_candidates`): no snapshot while deferred items touch the
      object.
- [x] Past `MAX_DEFERRED`, updates are dropped while `seq` advances [C-L11]
      (core).
- [ ] **IME composition is applied against a stale text base** [W-H2] —
      interim: in `NoteHost::edit`, resync instead of applying when the CRDT
      text differs from the editor's base (prevents misplaced text and
      panics). Real fix in R6.
- [ ] A rejected batch leaves the rest of its updates unsent [W-M3] —
      `resend_pending` after `drop_rejected` and `apply_purge`.
- [ ] `persist_pending` writes an empty list if one entry fails to encrypt
      [W-L9]; a key reload already in progress swallows the next request
      [W-L9].
- [ ] `missing_epoch` misreads document-key epochs, so every page refetches
      keys [C-L10].
- [ ] An uploaded file's link can land in a different note [W-M11].
- [ ] Every DOM resync turns non-breaking spaces into spaces [W-M5].
- [ ] Remote edits scroll the view back to the caret [W-M7].
- [ ] Ticking a task in reading view edits the first `[ ]` of the line [W-L6].
- [ ] Sign-out doesn't flush; add "forget this browser" (device secret stays
      in localStorage) [W-L10].
- [ ] Zip import has no size limit (zip bomb); export lacks zip64 [W-L8].

---

## Phase 3 — escape hatch: offline recovery

Nobody's notes may be locked in because a server or an app is unavailable.

- [ ] **`coolpsonotes recover`** — fully offline, no login, no server:
      ```
      coolpsonotes recover --backup coolpsonotes.db --recovery-key - --out ./notes
      ```
      recovery key → account secret → vault keys (all epochs, via the key
      chain) → document keys → snapshots + updates → Markdown files and
      attachments, with the vault's folder structure. Built into the CLI (one
      static binary), reusing `crates/core`; reads the recovery key from stdin
      or a file, never argv.
- [ ] Also accept a device's local profile instead of the recovery key, and a
      hosted-user **encrypted export bundle** (`GET /me/export`: the user's
      vault ciphertexts, envelopes and chains) — hosted users can't get a
      server backup file.
- [ ] `coolpsonotes export` works fully offline from the local replica
      (attachments cached or marked missing).
- [ ] Test: backup → recover → byte-identical notes and attachments; include
      rotated keys, shared notes and trashed entries.
- [ ] Docs: "Leaving coolpsonotes / disaster recovery", and a format
      description precise enough for an independent implementation.

---

## Phase 4 — hardening not covered by a redesign

- [x] **Stale vault-key epochs are accepted** after a rotation [S-M1] — `412
      stale_key` for epochs below the current one, `400` above; consider a
      "rotation pending" flag that refuses writes until vault and document
      keys are rotated. Re-check the writer's role inside the transaction
      [S-L1].
      (done: 412 for older epochs, access re-checked under the write lock; the "rotation pending" flag is still open)
- [x] **Editors can destroy content and history server-side** [S-M4] —
      soft-delete purges and attachment deletes with an owner-recoverable
      grace period (or owner-only purges); rate-limit snapshots per
      object/author; keep at least one older snapshot by time or author;
      audit snapshot creation.
      (partly: snapshot cooldown per person and note; owner-recoverable deletes still open)
- [x] Denial of service [S-M7] — per-connection and per-user limits for
      WebSocket push/subscribe; byte-bounded pages including snapshots.
      (Quotas: product track. Global write lock: R4.)
      (partly: WebSocket request budget and 16 MiB pages; quotas are in the product track, the global lock in R4)
- [x] Note shares outlive later vault membership [S-L2].
- [x] Agent revocation gaps [S-L3] — deleting an agent must revoke its live
      streams; disabling a person must revoke their agents' tokens.
- [x] Rate-limit IP from the rightmost trusted hop, not the leftmost
      `X-Forwarded-For` [S-L4, D-L1].
- [x] Device-code flow [S-L5] — auth-level rate limit on `/oauth/device*`,
      unbiased and longer user codes, show creation IP and time on approval.
      (rate limit and unbiased codes done; showing time/IP on approval still open)
- [x] Names in the audit log [S-L6] — device names (raw, unbounded), agent
      names, client names.
- [x] Recipients of a single note learn vault-wide metadata [S-L7] — vault
      `head_seq` and gaps, member ids; `get_user` returns members' e-mail to
      recipients even in exact-e-mail directory mode.
      (e-mail no longer shared; vault head_seq and member ids remain until R2)
- [x] Store `email_verified`; only verified e-mails are searchable [S-L9].
- [ ] Gateway audit log misses moves and deletes (only `put_document` and
      `put_attachment` are recorded).
- [ ] Note-key rotation skips the trust-on-first-use pin check [C-M7]
      (interim until R3).
- [ ] Revocation can be left half-done [C-M12] — retry/recovery hint, a CLI
      command to rotate a note key, exclude (don't abort on) members with a
      changed pin, handle purged shared notes.
- [ ] `vault share`/`share-note` accept a single prefix match ("bob" →
      "bobby") [C-M9].
- [ ] `--recovery-key` on the command line ends up in shell history [C-L6].
- [ ] `agent rotate-secret` revokes old credentials without confirmation
      [C-L8].
- [ ] CLI secrets in the OS keyring [C-H9]: `local.key` (Secret Service,
      Keychain, DPAPI; separate fallback location), session and gateway tokens.

---

## Redesign tracks

The "if I started over" list. Each track names the findings it resolves —
don't fix those separately unless a phase above has an interim item.

Suggested order: **R7 → R4 → R1 → R6 + R5 → R2 → R3** (R7 and R4 are cheap
and independent; R1 first so R5, R6, R2 and R3 are implemented once instead
of in two clients).

### R1. One sync engine for all clients — separate session

The web client (`state.rs`) and the CLI (`session.rs`, `livesync.rs`)
implement the same protocol twice; note sharing had to be built twice and
several review bugs exist twice. Build a platform-neutral sync crate (async,
with traits for storage, transport and timers) used by the browser (wasm) and
the desktop client, with one test suite.

- [ ] Design the crate boundary: replica state, pending queue, catch-up,
      live subscription, acks, key reloads, deferred content, snapshots,
      presence.
- [ ] Move the CLI onto it, then the web client.
- Resolves: oversized pushes [W-H5, C-H5], rejected batches [W-M3], pending
  persistence and key-reload edge cases [W-L9], connection handling [C-M1,
  C-H4, C-M10], document-epoch handling [C-L10], pending merge per object
  [W-M9], and makes S-M2/C-L11 fixes shared by construction.

### R2. Per-document keys from day one

Give every note its own key, wrapped by its vault key (later: by a folder
key). Single-note and folder sharing become the same mechanism.

- [ ] Key hierarchy vault → (folder) → document; migration that wraps
      existing content lazily.
- [ ] Remove the top-bit epoch scheme and the first-share re-encryption.
- Resolves: the first-share snapshot dance and its races [S-M3, stale_key on
  first share]; reduces recipient metadata exposure [S-L7, with per-object
  sequence numbers]; prerequisite for folder sharing.

### R3. Signed membership log and signed updates

Owners sign membership and key changes into a hash-chained log per vault that
every client verifies; updates are signed by the author's key.

- [ ] Signed, hash-chained membership/key log; clients refuse unsigned
      changes.
- [ ] Signed updates (authorship verifiable even against a colluding server).
- [ ] Bind the account key into device approval (interim fix worth doing
      first: include the account-key fingerprint in the verification code and
      verify the unwrapped secret against the published key) [S-M5, W-M1].
- Resolves: the owner's client giving the vault key to any member the server
  lists [C-H8]; new-device secret substitution [S-M5, W-M1]; trust pins
  rolled back by replaying old prefs [W-M10]; recovery-key rotation replay
  [S-L8]; pin checks in rotations [C-M7]; residual risk #2 in
  [security.md](docs/security.md) (viewers forging content with a colluding
  server).

### R4. Storage foundation (SQLite stays)

Lay the foundation now while it is cheap — only the parts that pay off even
with SQLite; no SQL-dialect abstraction.

- [ ] Per-vault write locks (or `BEGIN IMMEDIATE` per vault transaction)
      instead of the server-wide `write_lock` — one busy vault no longer
      stalls everyone.
- [ ] Attachments behind a blob-store interface, filesystem by default —
      smaller database, faster backups; S3-compatible later for hosted
      instances.
- [ ] Event fan-out (`hub.rs`) behind an interface — enables more than one
      process per instance later.
- [ ] Queries behind a small repository layer — testability, one place for
      authorization-sensitive SQL.
- Resolves: global lock part of S-M7, re-checking roles inside the write
  transaction [S-L1]; prepares the hosted tier's backups and scaling by
  instances.

### R5. Local-first web client

- [ ] Persist encrypted replicas in IndexedDB (sealed with a non-extractable
      WebCrypto key); start from local state, then catch up incrementally.
- [ ] Load vault keys and shares in parallel at startup.
- Resolves: slow start and full re-download on every page load [W-L12];
  pending edits of two tabs overwriting each other [W-M4]; quadratic offline
  pending queue and silent localStorage quota failures [W-M9]; offline reading.

### R6. Web client structure and an editor crate

- [ ] Split `crates/web/src/state.rs` (2,000+ lines of global `RefCell`
      state) into services with Leptos contexts/stores — no more re-entrancy
      footgun.
- [ ] Move the editor into its own crate with a document model (rope +
      transactions + decorations) instead of per-line re-render and DOM
      diffing; browser-level tests (`wasm-bindgen-test`).
- Resolves: IME composition rebasing [W-H2, real fix]; full rebuild during
  composition [W-M6]; performance on large notes and vaults (re-render per
  keystroke, index rebuilt per lookup, backlinks recomputed, no attachment
  failure cache) [W-M8]; stray DOM nodes after native edits [W-L2]; Safari
  composition input types [W-L3]; leaked closures, listeners and intervals
  [W-L5]; drag-and-drop at the drop point [W-L7].

### R7. Fuzzing and property tests

- [ ] `cargo-fuzz` targets: link/percent decoding and Markdown analysis,
      editor syntax classification and styling (multibyte!), envelope and
      protocol parsing, zip import, text diff.
- [ ] Property tests: CRDT convergence under random concurrent edits, mirror
      path mapping (case-insensitive and Windows rules), table/key helpers.
- [ ] In CI from now on.
- Would have caught W-H3 and W-H4; guards the mirror path fixes (C-H6, C-H7).

---

## Product track (hosted offering)

- [ ] **c4stack as the front door** (OIDC identity provider for registration,
      authentication and billing): plan and quota claims in the ID token or a
      provisioning API; lifecycle hooks (suspend, delete, export); a support
      policy that is honest about E2EE — a forgotten password never means
      recovered data without the user's recovery key.
- [ ] **Shared tier** (one instance, many users): storage/attachment/vault
      quotas [S-M7]; exact-e-mail user directory only [S-L9]; per-user rate
      limits and abuse handling; account deletion and export (GDPR, Phase 3
      bundle); document the metadata the server sees.
- [ ] **Dedicated instances** (one per customer): provisioning automation
      (Helm chart or Terraform), an OIDC client per instance in c4stack,
      backups to object storage (needs R4 blob store), managed upgrades,
      monitoring, custom domains.
- [ ] **Scaling by instances**: route users/tenants to instances; decide how
      sharing works across instances (keep sharing groups on one instance, or
      a "home instance" model).
- [ ] **Telemetry, private by default**: server-side aggregates only (active
      users, storage, error rates) from metadata the server already has;
      client error reports opt-in, scrubbed of content and titles,
      first-party only; written policy.
- [ ] **Release engineering**: publish versioned images to GHCR [D-H4];
      prebuilt binaries [gap 3]; `binaryen` in the image build [D-L5]; ~~CI runs
      the wasm clippy step and reads the `wasm-bindgen` version from
      `crates/web/Cargo.toml` [D-L8]~~ (done); Kubernetes manifest fixes — `/tmp`
      volume, `ingressClassName`, TLS secret, body limit vs attachment size
      [D-L3]; signed releases, SBOM.
- [ ] **External security and cryptography audit** before selling on
      end-to-end encryption.
- [ ] Legal: terms, privacy policy, data processing agreement.
- [ ] Mobile apps; a native desktop app (Tauri around the web UI) — while
      keeping "use your own editor" first-class.

---

## Docs and housekeeping

- [ ] `protocol.md`: document `GET /me/audit`, `/admin/*` and the WebSocket
      `snapshot` message [D-L2].
- [x] `development.md`: Dex is on port 5556 (8081 is the OIDC-enabled server);
      `network_mode: host` is Linux-only [D-L7].
- [x] `AGENTS.md` and `development.md`: `task cli -- --profile alice ls` needs
      a vault argument [D-L6]; Dockerfile and compose files are at the repo
      root, not in `deploy/` [D-L12]; `task ci` vs the GitHub workflow
      (wasm clippy, cargo-deny) [D-L8].
- [ ] `acceptance.md`: wasm size refers to the optimized build; the Docker
      image's bundle is larger until `binaryen` is added [D-L5].
- [ ] `.git-blame-ignore-revs` with the rustfmt commit `eb5c184`.

## UX polish

- [ ] The welcome note says Ctrl+K jumps to a note and `?` shows shortcuts,
      but the editor is focused (Ctrl+K inserts a link, `?` types) [W-L11].
- [ ] Reloading during the recovery-key screen loses the key without warning
      [W-L11].
- [ ] Menus don't close on an outside click; `autofocus` on dynamically
      inserted inputs does nothing [W-L11].
- [ ] Accessibility: icon buttons rely on `title` only; dialogs lack
      `aria-modal` and a focus trap; the tree lacks `aria-expanded`/
      `aria-level`; the note title has no visible focus outline [W-L11].
- [ ] Back/forward navigation doesn't refresh presence; e-mail autolinks
      become "create note" links [W-L11].
- [ ] Pipe-less GFM tables (`a | b`) render in reading view but not in live
      preview.
- [ ] Wiki links to headings (`[[Note#Heading]]`) open the note but don't
      jump to the heading.

---

## Later and ideas

### Remote MCP connector (to think about)

An optional `coolpsonotes connector` that speaks MCP over HTTP for hosted
assistants and cloud agents that can't install anything. It runs with an
agent identity, sees only vaults shared with that agent, and is never part of
the sync server. Trade-off: whoever operates it can read those vaults, so a
hosted version must be an explicit per-vault opt-in. Full reasoning and open
questions: [docs/agents.md §8](docs/agents.md#8-why-there-is-no-mcp-server).

### Quick notes (brain dump)

A capture-first space for thoughts that don't have a home yet.

- A sidebar section that can be turned on or off in settings, above
  "Recent": a one-click / one-shortcut "new quick note", plus the latest
  few quick notes.
- An overview page showing all quick notes at a glance (cards, newest
  first), with "promote to a regular note" (move into a folder) and archive.
- Stored as ordinary notes in a `Quick notes/` folder of a chosen vault, so
  they sync, mirror to Markdown files and work in your own editor too.
- Open questions: one quick-notes vault or per vault; naming (timestamp vs
  first line); shortcut; mobile-friendly capture.

### Tasks (undecided — capturing ideas)

Something between a checklist and a light kanban; tasks related to notes,
with a history.

- Possibly a "daily" list (today's tasks) plus longer-running ones.
- Idea worth keeping: tasks stay **plain Markdown** (`- [ ] …` items in notes,
  maybe daily notes like `Daily/2026-10-19.md`), and the app adds a view that
  collects open tasks across notes, links back to where they live, and keeps
  history via completion dates (`✅ 2026-10-19`, compatible with Obsidian's
  Tasks plugin). That way tasks also work in your own editor.
- Open questions: due dates and recurrence; a kanban-ish board view; how
  "longer" tasks relate to daily ones.

### Other

- [ ] Folder sharing (builds on R2).
- [ ] Mobile apps; native desktop GUI.
- [ ] **Sharing between instances** (very later): independent self-hosted
      instances share vaults, folders or notes with each other's users.
      Not planned yet, but decisions made now must not rule it out:
      - ids stay random and globally unique (vaults, objects, devices) —
        never sequential or instance-scoped;
      - people are identified by their **account keys**, not only by a
        local user id; plan for instance-qualified ids (`user@instance`);
      - one instance owns a vault's ordered log; remote members sync
        against it (no global sequence numbers);
      - membership must be verifiable without trusting the other server —
        another reason for the signed membership log (R3);
      - per-document and folder keys (R2) make "share a note or folder with
        someone elsewhere" the same mechanism as sharing locally;
      - still needed then: instance identity and server-to-server auth,
        discovery (`/.well-known`), abuse limits, and documenting what a
        remote instance learns (members, timing).
      See also [architecture.md §8](docs/architecture.md#8-accommodating-future-work).
