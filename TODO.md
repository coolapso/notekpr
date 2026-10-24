# TODO

What is still to be done, with the context needed to do it. Finished work
lives in the git history and the docs, not here; when you finish an item,
remove it (and update the docs it touches).

IDs like `[S-M4]` point to the full review of 2026-10-19: `D` docs and
deployment, `S` server and core, `W` web client, `C` CLI client, then
severity (`H`igh, `M`edium, `L`ow) and number.

## Decisions

Settled; don't reopen them without a new reason.

- **SQLite only**, also for a hosted offering: one database per instance,
  grow by adding instances. Attachments and live events sit behind interfaces
  (`server/src/blobs.rs`, `hub.rs`), so other stores can be added as variants.
  No per-vault write locks: SQLite has one writer, so they wouldn't help —
  revisit only if profiling shows contention.
- **No local MCP server.** Agents use the CLI, a synced Markdown folder or the
  local gateway ([docs/agents.md §8](docs/agents.md#8-why-there-is-no-mcp-server)).
- **Nobody's notes may ever be locked in**: `notekpr recover` rebuilds
  everything from an encrypted export and the recovery key, without server or
  apps ([docs/recovery.md](docs/recovery.md)).
- **"Use your own editor" is a core principle**: the Markdown folder stays a
  first-class way to use notekpr, even if a native app arrives.
- **No folder sharing** (2026-10-20): sharing a vault covers it; single notes
  stay shareable on their own.
- **Key epochs**: document keys keep the top-bit epochs, and sharing a note
  always starts with a fresh, unlinked key, so recipients can't decrypt the
  note's earlier history even with the server's help (wire formats are
  append-only).

## UX and Bugs 

## Feature creep

## Security and data safety

- [ ] **Owner-recoverable deletes** [S-M4]: an editor can permanently delete
      notes (emptying the trash) and attachments, and the owner can't get
      them back. Options: a grace period during which purges are only marked
      and the owner can undo them, or owner-only purges. Snapshot flooding is
      already limited (one per person and note per cooldown).
- [ ] **Window between removing someone and renewing the key** [S-M1]:
      removing a member and rotating the vault key are two requests. If the
      rotation doesn't happen (client closed, error), new writes still use a
      key the removed person knows (readable by them only with the server's
      help). Idea: the server marks the vault "rotation pending" on removal
      and refuses writes under the old epoch until the owner's client rotates.
- [ ] **What recipients of a single note learn** [S-L7]: they see the vault's
      sequence numbers (and so its activity) and members' user ids. Fixing it
      needs per-object sequence numbers for recipients.
- [ ] **Device-code approval** [S-L5]: the approval screen shows when the
      code was requested, not from where; showing the requesting IP would help
      spot phishing (the server has it at request time; don't log it).
- [ ] **CLI secrets in the OS keyring** [C-H9]: `local.key` (Secret Service,
      Keychain, DPAPI; separate fallback location), session and gateway
      tokens. Deferred (2026-10-19): not worth the dependency weight yet; the
      docs say plainly that `local.key` sits next to the data.

## Engineering

- [ ] **Vendored crates: check upstream now and then** (agent task; nothing
      here is to be sent upstream by us). `vendor/openidconnect` is 4.0.1
      moved to `p256`/`p384` 0.14 for RUSTSEC-2026-0387 (`crypto-bigint`):
      drop it once an openidconnect release uses them. `vendor/yrs` is 0.28.0
      with fuzzing fixes: drop it once yrs releases equivalent fixes. Each
      `NOTEKPR.md` says what changed; `task audit` shows new advisories.
- [ ] **Role re-checks before running several server processes** [S-L1, rest]:
      push and snapshot check access under the write lock just before their
      `BEGIN IMMEDIATE` transaction rather than inside it (every other write
      re-checks inside, see `access.rs`). Fine with one process; move the check
      in before a second one.
- [ ] **Shared sync engine, second half** (R1): catch-up paging, live
      subscription bookkeeping, key-reload flows and presence are still
      implemented in each client (the CLI catches up and pushes over REST, the
      web over the WebSocket). Move them into `crates/sync` once a client gets
      a second transport; until then it would only move code.
- [ ] **Performance on large vaults** [W-M8, rest]: the review found the
      index rebuilt per lookup, backlinks recomputed and no cache for
      attachments that failed to load. Per-note analysis and link checks are
      cached now (`web/src/knowledge.rs`); measure on a big vault
      (`tests/e2e/perf.py` covers typing only) before changing more. Notes
      shared with you still load one by one at startup.
- [ ] **Server memory per connected person**: about 0.22 MB each in the load
      test (`task load REALISTIC=1`), the first limit when sizing a server —
      CPU while people work is about 0.035 % of a core each. Find out what it
      is (WebSocket buffers and tasks per connection, the per-vault event
      channels — already cut from 2048 to 256 slots —, glibc's per-thread
      malloc arenas) before changing more.
- [ ] **One writer for all vaults**: every push takes the global write lock
      (`state.write_lock`) and does its access checks, duplicate and object
      lookups and signature checks inside it, so a server tops out at about
      1,200 pushes a second however many cores it has (load test: 4,000
      realistic people on 2 cores used one and delivery fell behind). First:
      check signatures before taking the lock; then see whether pushes to
      different vaults need to wait for each other at all (SQLite has one
      writer, but the transaction itself is short).
- [ ] **Holding requests during an update on Kubernetes** (to test): with
      `strategy: Recreate` there's a moment without a ready pod, and an
      ingress has nothing to send requests to, so people opening the page
      then get an error (the apps themselves cope: they're told the server is
      restarting and come back within a second of it). Possibility: Traefik's
      `retry` middleware with the route sent to the Service's address
      (`nativeLB: true`), commented out in
      `deploy/examples/traefik/ingressroute.yaml`. Whether it holds requests
      depends on how the cluster routes Service traffic: try it on k3s and on
      a cloud cluster, then document it as working (docs/operations.md §4,
      "Holding requests during an update") or drop it.
- [ ] **Small web leaks** [W-L5, rest]: `actions::pick_files` forgets its
      change listener (`cb.forget()`), and every socket starts its own
      watchdog `set_interval` (`web/src/ws.rs`).
- [ ] **Quick-notes helpers in core**: naming, preview and default-vault
      choice exist twice (`web/src/quick.rs`, `client/src/ops.rs`).
- [ ] **Icons in the Markdown mirror**: the CLI sets and lists icons (`icon`,
      `vault icon`), but the mirrored files carry none; they'd need front
      matter (decide first — it would show in every other editor).

## Product track (hosted offering)

- [ ] **c4stack as the front door** (OIDC identity provider for registration,
      authentication and billing): plan and quota claims in the ID token or a
      provisioning API; lifecycle hooks (suspend, delete, export); a support
      policy that is honest about end-to-end encryption — a forgotten password
      never means recovered data without the user's recovery key.
- [ ] **Shared tier** (one instance, many users): storage, attachment and
      vault quotas [S-M7]; exact-e-mail user directory only [S-L9]; per-user
      rate limits and abuse handling; account deletion and export (GDPR: the
      encrypted export bundle exists); document the metadata the server sees.
- [ ] **Dedicated instances** (one per customer): provisioning automation
      (Helm chart or Terraform), an OIDC client per instance in c4stack,
      backups to object storage (a blob-store variant), managed upgrades,
      monitoring, custom domains.
- [ ] **Scaling by instances**: route users and tenants to instances; decide
      how sharing works across instances (sharing groups on one instance, or a
      "home instance" model).
- [ ] **Telemetry, private by default**: server-side aggregates only (active
      users, storage, error rates) from metadata the server already has;
      client error reports opt-in, scrubbed of content and titles,
      first-party only; a written policy.
- [ ] **Release engineering**: the release version in the binaries (the
      server and the CLI report Cargo's `0.1.0`; the image build gets it as
      the `VERSION` argument already); prebuilt binaries; signed releases
      and an SBOM; Kubernetes manifest fixes
      [D-L3] — a `/tmp` volume, `ingressClassName`, the TLS secret, and the
      ingress body limit vs the attachment size.
- [ ] **External security and cryptography audit** before selling on
      end-to-end encryption.
- [ ] **Legal**: terms, privacy policy, data processing agreement.
- [ ] **MCP Connector**: MCP connector should be billed as an extra, agents can use local cli client, the mcp connecter is an extra client just as CLI running on a diferent server

## Ideas for later

### Before Live

- **Excalidraw** (parked 2026-10-20, to discuss): Excalidraw itself is a
  JavaScript/React app — ruled out (no npm or JavaScript packages, and
  third-party code would see decrypted notes and keys). Today `.excalidraw`
  files already sync as encrypted attachments and open in Obsidian's
  Excalidraw plugin through the Markdown folder. Options: a read-only preview
  of drawings in notes (parse the documented JSON, also Obsidian's compressed
  `.excalidraw.md`, and draw it in Rust — a few days; the hand-drawn look is
  extra), or a drawing editor of our own in Rust saving Excalidraw's format,
  synced as a CRDT like notes (several weeks).
- **Native Desktop App**:
- **Remote MCP connector**: an optional `notekpr connector` speaking MCP
  over HTTP for hosted assistants and cloud agents that can't install
  anything. It runs with an agent identity, sees only vaults shared with that
  agent, and is never part of the sync server. Whoever operates it can read
  those vaults, so a hosted version must be an explicit per-vault opt-in
  ([docs/agents.md §8](docs/agents.md#8-why-there-is-no-mcp-server)).

### Idea to think probably before live

* Road Map / Kanban / Task list
* Cards view with notes and voting
* Quick View / home: A static place where users can create something like a vault home page
- **admin**: Special admin settings to allow some admin settings to be configured on the fly without having to restart the server.
    - All the existing admin settings available, people, agents vaults documents files database
    - Instance admin / settings that are only available to vault owners via via env variable
    - Instance admin api with special credentials, not access to the vaults and only has access to the servers settings
    - Server settings are: Number of users, amount of storage allowed, amount of vaults allowed, read only mode
- **Admin Less instance**: Instance without Admin and only the api admin can acess the instnace settings
    - Use case: A single shared instance for free users in the hosted cloud


### After live

- **Puliic Notes**: Allow a note to be shared as a public note / link. When viewing the note from the public link, the note is read only and in reading mode.
    * Comments should not be visible
    * only note content should be visible
- **Notes Templates**: Create a note template and show "From template" when creating a new note, have a template assigned for quick notes.
- **Mobile apps:** See native client notes 
- **AI Support**: Support either remote or local AI to talk with and transform the thoughts into written notes.
- **Tasks** (undecided): something between a checklist and a light kanban,
  related to notes, with a history. Worth keeping: tasks stay plain Markdown
  (`- [ ] …` in notes, maybe daily notes like `Daily/2026-10-19.md`), and the
  app adds a view collecting open tasks across notes, linking back to where
  they live, with completion dates (`✅ 2026-10-19`, compatible with
  Obsidian's Tasks plugin) — so they work in your own editor too. Open: due
  dates and recurrence, a board view, how longer tasks relate to daily ones.
- **Sharing between instances**: independent self-hosted
  instances share vaults or notes with each other's users. Not planned, but
  today's decisions must not rule it out:
  - ids stay random and globally unique (vaults, objects, devices), never
    sequential or instance-scoped;
  - people are identified by their account keys, not only a local user id;
    plan for instance-qualified ids (`user@instance`);
  - one instance owns a vault's ordered log; remote members sync against it
    (no global sequence numbers);
  - membership stays verifiable without trusting the other server (the
    signed membership log);
  - per-note keys make "share a note with someone elsewhere" the same
    mechanism as sharing locally;
  - still needed then: instance identity and server-to-server auth,
    discovery (`/.well-known`), abuse limits, and documenting what a remote
    instance learns (members, timing).
  See also [architecture.md §8](docs/architecture.md#8-accommodating-future-work).

## Native client notes

## Going-live track (2026-10-23)

The way to go live: free users share one instance, anyone can move to a
dedicated instance easily, and updates happen without downtime. Still SQLite
and one writer per instance (see Decisions). Items elsewhere in this file
belong to it: **admin** and **Admin Less instance** (Ideas, before live),
**Shared tier**, **Dedicated instances** and **Scaling by instances**
(Product track), **Holding requests during an update on Kubernetes**
(Engineering) and **Sharing between instances** (Ideas, after live).

- [ ] **Instance settings without a restart**: the limits (users, vaults and
      storage per user, attachment size), read-only mode and the other admin
      settings live in the database and take effect at once. Flags and the
      environment keep what is needed to start (data directory, public URL,
      OIDC) and may pin a setting so it can't be changed at runtime. An
      instance admin API with its own credentials sees settings and usage,
      never vaults. Fewer settings that need a restart means fewer restarts.
- [ ] **One shared instance for free users**: the per-user limits above,
      checked by the server on the metadata it already has; Shared tier has
      the rest (rate limits, abuse, deletion and export). Accounts that never
      come back cost only disk: memory and CPU go with people online (about
      0.22 MB each, see Engineering).
- [ ] **Moving people to another instance**: a server-to-server copy of
      ciphertext — the account (public keys, encrypted key bundles, devices),
      its vaults' logs (each vault has its own sequence numbers), snapshots,
      key envelopes, the membership log and attachments. Nothing is
      decrypted, so history, comments and signatures survive (unlike the
      Markdown export and import). The vaults are read-only during the copy;
      afterwards the old instance answers "moved" for that account and the
      apps follow to the new address. Open: the people it shares with stay
      where they are, so shares across the move need **Sharing between
      instances** — bring it forward if needed. A home-instance model (one
      instance owns each vault's log, remote members sync against it) is
      enough; full federation is optional.
- [ ] **Updates without downtime: handoff on one node**: the new server starts
      next to the old one on the same database file and takes over writing
      when the old one stops.
      - Kubernetes: `RollingUpdate` with `maxSurge: 1, maxUnavailable: 0` on
        the existing `ReadWriteOnce` volume (that's per node, so both pods
        fit; `ReadWriteOncePod` wouldn't). SQLite's WAL lets two processes on
        one machine share the file — not over a network filesystem.
      - A writer lease (e.g. a lock on a file next to the database): only its
        holder writes and publishes live events, so the in-process write lock
        and event bus keep every vault's stream gapless and ordered as they
        do today. The old server gives the lease up on SIGTERM, after telling
        apps it's restarting; the new one takes it at once.
      - The new server is ready before it holds the lease: it serves the web
        app and reads, and holds writes and WebSockets until it has the lease
        (answering "retry" if that takes too long). This replaces **Holding
        requests during an update on Kubernetes**.
      - Sticky sessions on the ingress can keep connections on the writer
        during the switch. The lease is what guarantees one writer, so they
        stay an optional, documented example (no proxy is imposed).
      - Migrations: after the first deployment every migration must also work
        with the previous release (add, start using, drop a release later),
        because the old server runs on the new schema for a moment. The new
        server migrates as it starts, after the automatic backup it already
        takes.
- **Evaluated and not taken** (2026-10-23): Turso's Rust rewrite of SQLite
  (pre-1.0, no sqlx driver, and the ceiling is our write lock, not SQLite's
  writer) and PostgreSQL (a network round trip per query under the write lock
  lowers that ceiling; it buys several servers per instance, which this track
  doesn't need).
