# AI agents and integrations

notekpr can be the long-term knowledge base and memory of AI agents and
scripts — without giving them your account, your keys or access to every note.
Because the server cannot decrypt anything, plaintext access always goes
through a **trusted client that holds keys** — the `notekpr` CLI, running
on the agent's machine with the agent's own identity. An agent can use it in
three ways (§3): CLI commands, a folder of plain Markdown files kept in sync,
or a local HTTP API (the gateway).

## 1. The agent model

* An **agent is a separate identity** owned by a person. It authenticates with
  OAuth 2.0 client credentials issued by notekpr (not your identity
  provider), receives short-lived access tokens (1 h by default, refreshed
  automatically) and cannot create vaults, manage members, devices or agents.
* It has **its own key pair**, generated on the machine that runs it. Its
  secret key never leaves that machine; there is no recovery key — a lost agent
  is simply deleted and re-created.
* It sees **nothing until its owner shares specific vaults** with it, as
  **viewer** (read-only) or **editor** (never admin: it can't delete anything
  for good). Sharing wraps the vault key to the agent's public key, exactly
  like sharing with a person; removing it rotates the vault key. Agents are
  visible only to their owner.
* Every write is an ordinary encrypted CRDT update attributed to the agent, so
  people editing the same note concurrently never lose their changes, and the
  vault's history shows who changed what.
* The CLI, the Markdown folder and the gateway can also run with a person's
  own profile (e.g. a personal script). Then they have that person's access —
  prefer an agent for anything automated.

## 2. Setting up an agent

**Owner** (web app: Settings → AI agents → Create agent; or the CLI):

```sh
notekpr -p alice agent create "Home assistant" --secret-out ./agent.secret
# created agent 'Home assistant' [1d0a1bfc-…]
# client id:     agent_515257752d3e5580
# client secret: written to ./agent.secret (mode 0600)
```

The secret is shown (or written) only once; the server stores a hash. Move the
file to the agent's machine over a secure channel and keep it `0600`.

**Agent machine:**

```sh
notekpr --profile assistant agent setup \
    --server https://notes.example.com \
    --client-id agent_515257752d3e5580 \
    --client-secret-file /etc/notekpr/agent.secret
# Agent 'Home assistant' is set up in profile 'assistant' (new key pair created).
# Agent key fingerprint: A1E1 02F5 C9BC 4E63 4FFE 2DC1 5C55 8462
```

`agent setup` obtains a token, generates the agent's account key
(`engine::bootstrap_agent`), uploads only the public half with its
self-signature, and stores the secret half sealed in the profile's data
directory. The client secret itself is never copied into the profile; it is
re-read from the file whenever a new token is needed. Re-running the command is
harmless (it verifies the existing key). As with `login`, plain `http://` is
accepted only for a server on this machine; `--insecure-http` allows it for
other hosts.

**Owner, share a vault after checking the fingerprint:**

```sh
notekpr -p alice agent fingerprint "Home assistant"   # compare with the agent's output
notekpr -p alice vault share "Agent memory" "Home assistant" --role editor
# shared with Home assistant (agent) as editor
# Their key fingerprint: A1E1 02F5 C9BC 4E63 4FFE 2DC1 5C55 8462
```

The fingerprint printed by `agent setup` must match what `vault share` (or the
web app's share dialog) shows; it is then pinned and you are warned if it ever
changes. Use `--role viewer` for read-only agents. `notekpr agent list`
shows your agents, their fingerprints and active credentials.

## 3. How an agent works with notes

Pick whatever fits the agent; all three use the same profile, keys and sync,
and every write is an encrypted CRDT update merged with concurrent edits.

**CLI commands** — best for agents with a shell (coding agents, scripts, cron
jobs). Composable, nothing extra to run:

```sh
notekpr -p assistant ls "Agent memory" People
notekpr -p assistant cat "Agent memory" People/Alice.md
notekpr -p assistant search tag:project deadline
echo "- met Alice about the roadmap" | notekpr -p assistant write "Agent memory" Journal.md --append
notekpr -p assistant cat "Agent memory" Plan.md --json     # includes the content hash
notekpr -p assistant write "Agent memory" Plan.md --if-match <hash> < Plan.md
```

`write` merges the new text as a diff, so it never overwrites what someone else
changed meanwhile; `--if-match` refuses the write if the note changed since the
agent read it.

**A Markdown folder** — best for agents that edit files natively (Claude Code,
Codex, editors with AI features). Keep the agent's vaults mirrored and let the
agent read, grep and edit ordinary files:

```sh
notekpr -p assistant sync --dir ~/agent-notes --plaintext-ok
```

Edits on disk are picked up, merged and synced; changes from people appear in
the files (see [desktop-client.md §4](desktop-client.md#4-sync-and-the-markdown-mirror)).

**The gateway (local HTTP API)** — for programs and services that prefer HTTP
and JSON, or need change events. Described next.

**One process per profile.** `sync` and `gateway` own their profile's local
data while they run, so they can't share one profile. While one of them runs,
read commands (`ls`, `cat`, `search`) show its local copies, and changing
commands (`write`, `mv`, `rm`, …) are handed to it: it runs them on its own
copies and passes back what they print, so they work as usual. A gateway
started with `--read-only`
refuses every command handed to it. Details:
[desktop-client.md §3](desktop-client.md#3-everyday-commands).

## 4. The gateway (local HTTP API)

```sh
notekpr -p assistant gateway                       # 127.0.0.1:7878
notekpr -p assistant gateway --read-only --vault "Agent memory"
notekpr -p assistant gateway token                 # print the bearer token
notekpr -p assistant gateway token --rotate        # replace it
```

The gateway keeps the agent's replicas in sync live (WebSocket), serves
plaintext over HTTP on a **loopback address** (non-loopback addresses are
refused unless `--allow-non-loopback`, which should only ever sit behind a TLS
proxy you control), and requires `Authorization: Bearer <token>` on every
request. The token is generated once and stored in the profile (mode 0600).
`--vault` (repeatable) restricts it to some of the agent's vaults; other vaults
answer `404` as if they did not exist, and a `--vault` that matches none of
them stops the gateway from starting. `--read-only` refuses every write with
`403` (and every CLI command handed to it, §3). No endpoint ever returns key
material. If live sync ends for good (the session was revoked), the gateway
shuts down with an error instead of serving copies nobody keeps current.

Vaults are addressed by id or name, document paths are vault-relative (URL
encode spaces as `%20`; `.md` is optional). Errors are JSON
`{"error": "<code>", "message": "…"}`:

| Status | `error` | When |
|---|---|---|
| `400` | `bad_request` | a body that isn't UTF-8 (documents) or the expected JSON (move), an invalid path, an attachment read through the documents endpoint, an empty attachment, one over the server's limit or a `.md` path on the attachments endpoint, an empty search, a body that isn't an Excalidraw library file (shapes) |
| `401` | `unauthorized` | missing or wrong token |
| `403` | `read_only` | the gateway runs with `--read-only` |
| `403` | `forbidden` | the agent can't write to the vault (viewer, or a read-only local copy) |
| `404` | `not_found` | no such vault, document, revision or endpoint; saved shapes where there are none (an agent, or a gateway limited by `--vault`) |
| `409` | `conflict` | already exists, or exists as another kind |
| `412` | `precondition_failed` | `If-Match` failed |
| `500` | `internal` | anything else (e.g. the local copy could not be saved) |
| `503` | `unavailable` | not available offline (history, attachments not downloaded yet) |

Request bodies are limited to 16 MiB and attachment uploads to 128 MiB;
larger ones get `413` (plain text, not JSON).

```sh
export T=$(notekpr -p assistant gateway token)
G=http://127.0.0.1:7878/v1
```

| Endpoint | |
|---|---|
| `GET /v1/status` | `{user_id, kind, online, read_only, vaults}` (`kind`: `human` or `agent`; `vaults`: how many are exposed) |
| `GET /v1/vaults` | `[{id, name, role, archived, documents, pending_changes, seq, last_sync}]`, plus `icon`, `read_only` (why) and `too_large_changes` when set. Run with a person's own profile, their quick notes are listed too, with `kind: "quick_notes"` and `note_limit`: flat, at most that many notes, deleting is for good (an agent has none, and can't be given any) |
| `GET /v1/vaults/{v}/documents[?folder=F&all=true]` | notes and drawings below `F` (recursively): `[{vault, path, id, title, kind, tags, created, modified, size}]`, plus `mime` and `icon` when set (`all` adds attachments) |
| `GET /v1/vaults/{v}/documents/{path}` | Markdown body, `ETag: "<content hash>"`; `?format=json` → `{…info, hash, links, content}`. A drawing: its `.drawing` file (Excalidraw's JSON, `application/json`); `?format=text` → its words (texts, labels, sticky notes), one a line; `?format=json` adds them as `text` |
| `PUT /v1/vaults/{v}/documents/{path}` | create (`201`) or update (`200`) with the request body (`@today`, `@tomorrow` and `@yesterday` in it become those days' dates, `@2026-10-23`) → `{vault, path, id, created, changed, hash, bytes, pending_changes}` and the new `ETag`; `If-Match: "<hash>"` → `412` if it changed meanwhile (`If-Match: *`: it must exist); `If-None-Match: *` → create only. A path ending in `.drawing` takes a drawing file — ours, Excalidraw's JSON (`{"type":"excalidraw","elements":[…]}`) or Obsidian's — and makes or changes the drawing there, element by element |
| `POST /v1/vaults/{v}/move` | `{"from": …, "to": …, "update_links": false}` → `{vault, id, from, to}`; a `to` ending in `/` keeps the name |
| `DELETE /v1/vaults/{v}/documents/{path}` | move a document or folder to the trash → `{vault, trashed: [ids]}` |
| `GET /v1/search?q=…[&vault=v&limit=n]` | `[{vault, vault_name, path, id, title, score, snippet}]`; `limit` 50 by default, at most 500 |
| `GET /v1/vaults/{v}/backlinks/{path}` | documents linking to it |
| `GET /v1/vaults/{v}/comments/{path}` | the note's comments: `{vault_id, path, threads: [{id, quote, on_text, resolved_by, comments: [{id, author, author_name, body, mentions?, created, edited, pending, verified}]}]}` (the comment first, then its replies; `mentions`: `[{id, name}]`) |
| `POST /v1/vaults/{v}/comments/{path}` | `{"on": quote, "text": …}` comments on that text (it must appear once in the note); `{"id": …, "action": "reply"\|"edit"\|"delete"\|"resolve"\|"reopen", "text": …}` acts on a comment (`id` or the start of it) → `{id, vault_id, note_id, mentions, not_mentioned, people_unknown}`. `@Name` in the text mentions that member of the vault (`mentions`: their ids; `not_mentioned`: `@words` naming nobody there; `people_unknown`: the members couldn't be looked up) |
| `GET /v1/mentions[?vault=v&unread=true&limit=n]` | comments that mention the agent, newest first: `[{vault_id, vault, note_id, path, thread, comment, author, author_name, body, created, quote, resolved, unread}]`; `limit` 50 by default |
| `POST /v1/mentions/read` | (optional `{"vault": …}`) mark the (vault's) unread mentions read → `{marked}` |
| `GET /v1/vaults/{v}/history/{path}[?revision=ID]` | revisions `[{id, created_at, covers_seq, author_id, size}]`, or one revision's Markdown |
| `GET /v1/vaults/{v}/attachments/{path}` | raw bytes (`Content-Disposition: attachment`, `nosniff`) |
| `PUT /v1/vaults/{v}/attachments/{path}` | upload (`Content-Type` is stored as the MIME type); answers like a document `PUT`, without `ETag` |
| `GET /v1/shapes` | the saved shapes (pieces of drawings to drop into any drawing, in the person's Personal storage), newest first, as an Excalidraw library file (`.excalidrawlib`, `application/vnd.excalidrawlib+json`: `{"type": "excalidrawlib", "version": 2, "libraryItems": [{id, status, created, name?, elements}]}`). Only with a person's own profile, and a gateway not limited by `--vault`: otherwise `404` |
| `POST /v1/shapes` | an Excalidraw library file (version 1 or 2) as the body: its shapes are saved, those saved already (the same elements) left out, as `notekpr shapes import` → `{added, already, too_large, pending_changes}` (`too_large`: left out, larger than the server takes in one piece). `400` if the body isn't one |
| `GET /v1/events` | Server-Sent Events (below) — ids and paths only, never content |

Each event is named after its `type` and carries it as JSON, e.g.
`{"type": "document_changed", "vault": …, "id": …, "path": …, "origin": "remote"}`:

| Event | Fields | |
|---|---|---|
| `document_changed` | `vault, id, path, origin` | a note's text changed; `origin` is `local` (on this machine) or `remote` |
| `index_changed` | `vault, origin` | the vault's entries changed: created, moved, renamed, trashed, … |
| `document_deleted` | `vault, id` | a document was deleted permanently (trash emptied); moving it to the trash is an `index_changed` |
| `vault_added` | `vault` | a vault became available (newly shared) |
| `vault_closed` | `vault, reason` | the vault was deleted or access revoked; its local copy stays read-only |
| `connection` | `online` | the connection to the server went down or came back |
| `server_mode` | `read_only` | the server stopped (`manual`, `storage_full`) or started again (`null`) accepting changes; writes keep working locally and are uploaded when it accepts them |
| `lagged` | `missed` | the client read too slowly and missed that many events; re-read what it keeps track of |

Examples:

```sh
curl -H "Authorization: Bearer $T" $G/vaults
curl -H "Authorization: Bearer $T" "$G/vaults/Agent%20memory/documents?folder=People"

# read, then update without clobbering a concurrent change
curl -si -H "Authorization: Bearer $T" "$G/vaults/Agent%20memory/documents/People/Alice.md"
curl -X PUT -H "Authorization: Bearer $T" -H 'If-Match: "95df5d…"' \
     --data-binary @Alice.md "$G/vaults/Agent%20memory/documents/People/Alice.md"

curl -X POST -H "Authorization: Bearer $T" -H 'content-type: application/json' \
     -d '{"from":"Inbox/todo.md","to":"Archive/"}' "$G/vaults/Agent%20memory/move"
curl -X DELETE -H "Authorization: Bearer $T" "$G/vaults/Agent%20memory/documents/Archive/todo.md"
curl -H "Authorization: Bearer $T" "$G/search?q=tag:project%20deadline"
curl -X PUT -H "Authorization: Bearer $T" -H 'content-type: image/png' \
     --data-binary @chart.png "$G/vaults/Agent%20memory/attachments/img/chart.png"
curl -N -H "Authorization: Bearer $T" $G/events

# a person's own gateway: their saved shapes, as Excalidraw's library file
curl -H "Authorization: Bearer $T" $G/shapes > mine.excalidrawlib
curl -X POST -H "Authorization: Bearer $T" --data-binary @garden.excalidrawlib $G/shapes
```

A `PUT` with full content is applied as a **diff** to the current CRDT text, so
an agent rewriting a note while a person types in it only changes what the
agent actually changed. Use `If-Match` when the agent's change depends on the
version it read.

Writes are acknowledged once they are stored in the encrypted local replica;
they are uploaded immediately when online (`pending_changes` in the response
tells you if some are still queued) and retried automatically.

## 5. Credential and key rotation

* **Client secret**: `notekpr agent rotate-secret <agent> --secret-out
  new.secret` issues a new credential, writes it to the file and then — after
  asking, or with `--yes` — revokes all the old ones (their sessions end
  immediately). If the file can't be written, nothing is revoked. For a
  zero-downtime switch pass `--keep-old`, then revoke the old credential in
  the web app (Settings → AI agents → Revoke) once the agent uses the new one;
  the CLI can't revoke a single credential. Re-run `agent setup` with the new
  client id and file — the agent's key pair stays the same.
* **Gateway token**: `notekpr gateway token --rotate`, then restart the
  gateway.
* **Vault keys**: removing the agent from a vault (`vault unshare`) rotates the
  key; `vault rotate` does it on demand. Content written afterwards is
  unreadable for anyone without the new key.
* **Compromised agent machine**: delete the agent (`notekpr agent delete`,
  or the web app) — this removes its memberships and sessions, and your apps
  then rotate the keys of the vaults and notes it could read on their own.
  Create a new agent rather than reusing keys.

## 6. Audit log

Every write through the gateway is appended to
`$XDG_DATA_HOME/notekpr/<profile>/audit.jsonl` (mode 0600):

```json
{"at":1792378331454,"via":"gateway","op":"put_document","vault":"6ad6…","document":"052b…","bytes":18}
```

Operations are named after the call: `put_document` and `put_attachment`
(only when the content changed), `trash_document` (one line per trashed
entry), `move_document`, and `add_shapes` (only when shapes were saved; its
vault is the Personal storage, its size the body's). Entries contain time, channel, vault id, document
id (left out when there is none, e.g. a folder without an entry of its own)
and size — never content, titles or paths. CLI commands handed to a running
gateway (§3) are logged too, once they succeeded: `"via":"cli"`, the command
as `op` (`mkdir`, `vault rename`, …) and the id of the vault it names, if one.
(CLI commands run without a gateway and the Markdown folder are not audited
separately; their writes are attributed to the agent on the server like any
other.) On the
server side, each update is attributed to the agent's identity, and owners
see membership and key events in the vault audit
(`GET /api/v1/vaults/{v}/audit`).

## 7. Threat notes

* **The agent's machine holds plaintext** of every vault shared with the
  agent, and the agent's private key. Treat it like a server with secrets:
  full-disk encryption, an OS account of its own, no untrusted code running as
  that user. The local replica is encrypted, but its key file currently sits in
  the same profile directory, so a copy of that directory can be decrypted
  (moving it to the OS keyring is planned).
* **Anyone with the gateway token** (or able to run programs as the gateway's
  user) can read and, unless `--read-only`, change the exposed vaults. Keep the
  default loopback address; restrict with `--vault` and `--read-only`.
* **Prompt injection**: an agent reading notes may encounter instructions
  written by other members. Share only what the agent needs, prefer viewer
  access, and remember everything it writes is attributed to it and can be
  reviewed or restored from history and the trash.
* **Server compromise** does not expose content: the agent's key and all vault
  keys are generated and used client-side. A malicious server could try to
  substitute the agent's public key when the owner shares — which is what the
  fingerprint comparison and pinning are for.
* **Revocation is forward-looking**: removing an agent prevents access to
  future content after key rotation; data it already synced or exported cannot
  be recalled.

## 8. Why there is no MCP server

Earlier versions shipped `notekpr mcp`, a Model Context Protocol server on
stdio. It was removed on purpose (2026-10).

**What MCP is good at** is giving an AI application a standard way to reach a
tool it could not reach otherwise — typically a *remote* service, connected to
by URL, with nothing to install.

**A local stdio MCP server added little here.** It had to run on the agent's
machine with the agent's keys anyway — exactly where the CLI and the Markdown
folder already work. And for agents that can run programs, those are the
better tools:

* **Plain files beat tool calls.** Agents read, search, diff and edit files
  with their native tools, at any scale, without a tool schema in their
  context and without one round trip per operation.
* **The CLI composes.** `cat`, `write --append`, `search`, pipes and scripts
  cover the same operations, with the same authentication, encryption and
  merge-as-diff behaviour, and humans use the very same commands.
* **Less to secure and maintain.** The MCP server was a second plaintext write
  path (its own JSON-RPC implementation, tool definitions, audit and
  read-only enforcement) to keep correct and safe, for no capability the CLI
  lacked.

**Where MCP would add something** is the case a stdio server cannot serve
either: hosted assistants and cloud agents that cannot install or run
anything and can only connect to a URL. That is the remote connector below.

### Considered for later: a remote MCP connector

A separate, optional service — `notekpr connector`, the same binary —
that speaks MCP over HTTP (Streamable HTTP transport) to remote AI clients:

* It runs with an **agent identity**, holds only that agent's key and can only
  read the vaults explicitly shared with that agent, as viewer or editor.
  Revoking is the usual unshare + key rotation.
* It is **never part of the sync server process**: the server stays
  zero-knowledge. Self-hosters run the connector wherever they like (next to
  the server, or at home); it is a client like any other.
* **Trust trade-off, stated plainly:** whoever operates the connector can read
  the vaults shared with its agent. A hosted version must therefore be an
  explicit, per-vault opt-in ("vaults shared with the hosted connector are
  readable by the service").
* Open questions: authentication of remote MCP clients (OAuth 2.1 as the MCP
  specification describes), read-only by default, rate limits, audit, and
  which tools to offer (the gateway's operations in `crates/client/src/ops.rs`
  are a ready starting point).
