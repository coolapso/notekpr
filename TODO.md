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
- [ ] **Small web leaks** [W-L5, rest]: `actions::pick_files` forgets its
      change listener (`cb.forget()`), and every socket starts its own
      watchdog `set_interval` (`web/src/ws.rs`).
- [ ] **Quick-notes helpers in core**: naming, preview and default-vault
      choice exist twice (`web/src/quick.rs`, `client/src/ops.rs`).
- [ ] **Icons in the CLI**: vault, note and folder icons show in the web app
      only. `ls`/`vault list` could print them; the Markdown mirror has no
      place for them unless we add front matter (decide first).

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
