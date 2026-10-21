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
- **Nobody's notes may ever be locked in**: `coolpsonotes recover` rebuilds
  everything from an encrypted export and the recovery key, without server or
  apps ([docs/recovery.md](docs/recovery.md)).
- **"Use your own editor" is a core principle**: the Markdown folder stays a
  first-class way to use coolpsonotes, even if a native app arrives.
- **No folder sharing** (2026-10-20): sharing a vault covers it; single notes
  stay shareable on their own.
- **Key epochs**: document keys keep the top-bit epochs, and sharing a note
  always starts with a fresh, unlinked key, so recipients can't decrypt the
  note's earlier history even with the server's help (wire formats are
  append-only).

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

- [ ] **Browser test for rotations with a changed key**: rotations leave out
      people whose account key changed, and "I verified it" in the sharing
      dialogs gives them access again. Server and CLI tests cover the rules;
      the web flow has none, because the e2e suite can't stage a changed
      account key (needs a way to reset a test user's keys).
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
- [ ] **Updates without downtime**. Today an update is a short outage: one
      server process per database, so Kubernetes runs `replicas: 1` with
      `strategy: Recreate` (`deploy/kubernetes/coolpsonotes.yaml`); on SIGTERM
      the server stops cleanly (`with_graceful_shutdown`), but nothing tells
      the clients it's a restart. They see the connection drop, keep working
      offline (nothing is lost) and retry with growing delays (CLI 1, 2, 4 …
      60 s, `client/src/livesync.rs`; web 0.5–1, 1–2 … 30 s,
      `web/src/ws.rs`), so a restart of a few seconds is a comeback of about
      8 s, everyone at once; Caddy has no retry, so page loads, sign-ins and
      uploads get 502 meanwhile.
  - **First, make it unnoticeable** (small, no architecture change):
    - Caddy holds requests while the server restarts: `lb_try_duration 30s`
      and `lb_try_interval 250ms` in the `reverse_proxy` of
      `deploy/Caddyfile` and `Caddyfile.lan` — requests wait and then succeed,
      WebSocket upgrades too (document the same for other proxies).
    - On SIGTERM the server closes every WebSocket with close code 1012
      ("service restart") before draining; both clients then reconnect on a
      short jittered schedule (e.g. every 250–750 ms for up to a minute)
      instead of their backoff, spreading the reconnect rush.
    - Kubernetes: keep `Recreate`; the ingress answers 503 while no pod is
      ready, so put Caddy (with the retry) in front as its own deployment,
      or an ingress that can hold requests. Check start-up time and the
      readiness probe's period, so the new pod gets traffic as soon as
      `/readyz` answers.
    - Measure it with the load test through the Compose stack: `task
      load:remote SERVER=… -- --kill-command "docker compose up -d
      --force-recreate coolpsonotes" --start-command true --downtimes 0s`
      (a real update every level). Expected: a 1–3 s pause in live
      delivery, no errors.
  - **Then, if ever needed, old and new running side by side** (weeks): the
    server as several processes on one database — live events across
    processes (an `EventBus` backend that polls the log; `hub.rs` states what
    it must guarantee), role re-checks inside the transactions (above), the
    global write lock replaced by the database's (see *One writer for all
    vaults*), migrations both versions can run with (expand, then contract),
    and both processes on the same host and volume (SQLite locking doesn't
    work over network storage); then `RollingUpdate` with `maxSurge: 1,
    maxUnavailable: 0` and the pods on one node. It still isn't high
    availability: a machine going down needs a replicated database.
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
- [ ] **Release engineering**: versioned images on GHCR [D-H4]; prebuilt
      binaries; signed releases and an SBOM; Kubernetes manifest fixes
      [D-L3] — a `/tmp` volume, `ingressClassName`, the TLS secret, and the
      ingress body limit vs the attachment size.
- [ ] **External security and cryptography audit** before selling on
      end-to-end encryption.
- [ ] **Legal**: terms, privacy policy, data processing agreement.

## Ideas for later

- **Excalidraw** (parked 2026-10-20, to discuss): Excalidraw itself is a
  JavaScript/React app — ruled out (no npm or JavaScript packages, and
  third-party code would see decrypted notes and keys). Today `.excalidraw`
  files already sync as encrypted attachments and open in Obsidian's
  Excalidraw plugin through the Markdown folder. Options: a read-only preview
  of drawings in notes (parse the documented JSON, also Obsidian's compressed
  `.excalidraw.md`, and draw it in Rust — a few days; the hand-drawn look is
  extra), or a drawing editor of our own in Rust saving Excalidraw's format,
  synced as a CRDT like notes (several weeks).

- **Mobile apps and a native desktop app** (Tauri around the web UI), keeping
  "use your own editor" first-class.
- **Remote MCP connector**: an optional `coolpsonotes connector` speaking MCP
  over HTTP for hosted assistants and cloud agents that can't install
  anything. It runs with an agent identity, sees only vaults shared with that
  agent, and is never part of the sync server. Whoever operates it can read
  those vaults, so a hosted version must be an explicit per-vault opt-in
  ([docs/agents.md §8](docs/agents.md#8-why-there-is-no-mcp-server)).
- **Tasks** (undecided): something between a checklist and a light kanban,
  related to notes, with a history. Worth keeping: tasks stay plain Markdown
  (`- [ ] …` in notes, maybe daily notes like `Daily/2026-10-19.md`), and the
  app adds a view collecting open tasks across notes, linking back to where
  they live, with completion dates (`✅ 2026-10-19`, compatible with
  Obsidian's Tasks plugin) — so they work in your own editor too. Open: due
  dates and recurrence, a board view, how longer tasks relate to daily ones.
- **Sharing between instances** (much later): independent self-hosted
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
