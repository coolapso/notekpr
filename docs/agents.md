# AI agents and integrations

coolpsonotes can be the long-term knowledge base and memory of AI agents and
scripts — without giving them your account, your keys or access to every note.
Because the server cannot decrypt anything, plaintext access always goes
through a **trusted client that holds keys**: the `coolpsonotes gateway` (local
HTTP API) or `coolpsonotes mcp` (Model Context Protocol over stdio).

## 1. The agent model

* An **agent is a separate identity** owned by a person. It authenticates with
  OAuth 2.0 client credentials issued by coolpsonotes (not your identity
  provider), receives short-lived access tokens (1 h by default, refreshed
  automatically) and cannot create vaults, manage members, devices or agents.
* It has **its own key pair**, generated on the machine that runs it. Its
  secret key never leaves that machine; there is no recovery key — a lost agent
  is simply deleted and re-created.
* It sees **nothing until its owner shares specific vaults** with it, as
  **viewer** (read-only) or **editor**. Sharing wraps the vault key to the
  agent's public key, exactly like sharing with a person; removing it rotates
  the vault key. Agents are visible only to their owner.
* Every write is an ordinary encrypted CRDT update attributed to the agent, so
  people editing the same note concurrently never lose their changes, and the
  vault's history shows who changed what.
* The gateway and MCP server can also run with a person's own profile (e.g. a
  personal script). Then they have that person's access — prefer an agent for
  anything automated.

## 2. Setting up an agent

**Owner** (web app: Settings → AI agents → Create agent; or the CLI):

```sh
coolpsonotes -p alice agent create "Home assistant" --secret-out ./agent.secret
# created agent 'Home assistant' [1d0a1bfc-…]
# client id:     agent_515257752d3e5580
# client secret: written to ./agent.secret (mode 0600)
```

The secret is shown (or written) only once; the server stores a hash. Move the
file to the agent's machine over a secure channel and keep it `0600`.

**Agent machine:**

```sh
coolpsonotes --profile assistant agent setup \
    --server https://notes.example.com \
    --client-id agent_515257752d3e5580 \
    --client-secret-file /etc/coolpsonotes/agent.secret
# Agent 'Home assistant' is set up in profile 'assistant' (new key pair created).
# Agent key fingerprint: A1E1 02F5 C9BC 4E63 4FFE 2DC1 5C55 8462
```

`agent setup` obtains a token, generates the agent's account key
(`engine::bootstrap_agent`), uploads only the public half with its
self-signature, and stores the secret half sealed in the profile's data
directory. The client secret itself is never copied into the profile; it is
re-read from the file whenever a new token is needed. Re-running the command is
harmless (it verifies the existing key).

**Owner, share a vault after checking the fingerprint:**

```sh
coolpsonotes -p alice agent fingerprint "Home assistant"   # compare with the agent's output
coolpsonotes -p alice vault share "Agent memory" "Home assistant" --role editor
# shared with Home assistant (agent) as editor
# Their key fingerprint: A1E1 02F5 C9BC 4E63 4FFE 2DC1 5C55 8462
```

The fingerprint printed by `agent setup` must match what `vault share` (or the
web app's share dialog) shows; it is then pinned and you are warned if it ever
changes. Use `--role viewer` for read-only agents. `coolpsonotes agent list`
shows your agents, their fingerprints and active credentials.

## 3. The gateway (local HTTP API)

```sh
coolpsonotes -p assistant gateway                       # 127.0.0.1:7878
coolpsonotes -p assistant gateway --read-only --vault "Agent memory"
coolpsonotes -p assistant gateway token                 # print the bearer token
coolpsonotes -p assistant gateway token --rotate        # replace it
```

The gateway keeps the agent's replicas in sync live (WebSocket), serves
plaintext over HTTP on a **loopback address** (non-loopback addresses are
refused unless `--allow-non-loopback`, which should only ever sit behind a TLS
proxy you control), and requires `Authorization: Bearer <token>` on every
request. The token is generated once and stored in the profile (mode 0600).
`--vault` (repeatable) restricts it to some of the agent's vaults; other vaults
answer `404` as if they did not exist. `--read-only` refuses every write with
`403`. No endpoint ever returns key material.

Vaults are addressed by id or name, document paths are vault-relative (URL
encode spaces as `%20`; `.md` is optional). Errors are JSON
`{"error": "<code>", "message": "…"}` with `401` (token), `403` (read-only or
viewer), `404`, `409` (already exists), `412` (`If-Match` failed), `503`
(not available offline).

```sh
export T=$(coolpsonotes -p assistant gateway token)
G=http://127.0.0.1:7878/v1
```

| Endpoint | |
|---|---|
| `GET /v1/status` | identity, online state, read-only flag |
| `GET /v1/vaults` | `[{id, name, role, archived, documents, pending_changes, seq, last_sync}]` |
| `GET /v1/vaults/{v}/documents[?folder=F&all=true]` | `[{vault, path, id, title, kind, tags, created, modified, size}]` (`all` adds attachments) |
| `GET /v1/vaults/{v}/documents/{path}` | Markdown body, `ETag: "<content hash>"`; `?format=json` → `{…info, hash, links, content}` |
| `PUT /v1/vaults/{v}/documents/{path}` | create (`201`) or update (`200`) with the request body; `If-Match: "<hash>"` → `412` if it changed meanwhile; `If-None-Match: *` → create only |
| `POST /v1/vaults/{v}/move` | `{"from": …, "to": …, "update_links": false}`; a `to` ending in `/` keeps the name |
| `DELETE /v1/vaults/{v}/documents/{path}` | move a document or folder to the trash |
| `GET /v1/search?q=…[&vault=v&limit=n]` | `[{vault, vault_name, path, id, title, score, snippet}]` |
| `GET /v1/vaults/{v}/backlinks/{path}` | documents linking to it |
| `GET /v1/vaults/{v}/history/{path}[?revision=ID]` | revisions `[{id, created_at, covers_seq, author_id, size}]`, or one revision's Markdown |
| `GET /v1/vaults/{v}/attachments/{path}` | raw bytes (`Content-Disposition: attachment`, `nosniff`) |
| `PUT /v1/vaults/{v}/attachments/{path}` | upload (`Content-Type` is stored as the MIME type) |
| `GET /v1/events` | Server-Sent Events: `document_changed {vault, id, path, origin}`, `index_changed`, `document_deleted`, `vault_added`, `vault_closed`, `connection` — ids and paths only, never content |

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
```

A `PUT` with full content is applied as a **diff** to the current CRDT text, so
an agent rewriting a note while a person types in it only changes what the
agent actually changed. Use `If-Match` when the agent's change depends on the
version it read.

Writes are acknowledged once they are stored in the encrypted local replica;
they are uploaded immediately when online (`pending_changes` in the response
tells you if some are still queued) and retried automatically.

## 4. MCP

`coolpsonotes mcp` speaks MCP (protocol revision `2025-06-18`, JSON-RPC 2.0,
one message per line on stdio; logs go to stderr). It runs the same live sync
as the gateway. Tools:

| Tool | Arguments |
|---|---|
| `list_vaults` | — |
| `list_documents` | `vault`, `folder?` |
| `read_document` | `vault`, `path` → content item 1: Markdown; item 2: metadata JSON incl. `hash` |
| `search` | `query`, `vault?`, `limit?` |
| `create_document` | `vault`, `path`, `content` (fails if it exists) |
| `update_document` | `vault`, `path`, `content`, `expected_hash?` (full content, merged as a diff) |
| `append_to_document` | `vault`, `path`, `content` |
| `move_document` | `vault`, `from`, `to`, `update_links?` |
| `delete_document` | `vault`, `path` (moves to the trash) |
| `get_backlinks` | `vault`, `path` |
| `document_history` | `vault`, `path`, `revision?` |

With `--read-only` the five write tools are not offered at all (and refused if
called). `--vault` restricts the vaults as for the gateway. Tool failures are
returned as results with `isError: true`.

Example client configuration (Claude Desktop / Claude Code `mcpServers`
format; other MCP clients use the same command and arguments):

```json
{
  "mcpServers": {
    "coolpsonotes": {
      "command": "/home/me/.local/bin/coolpsonotes",
      "args": ["--profile", "assistant", "mcp", "--vault", "Agent memory"]
    }
  }
}
```

Claude Code: `claude mcp add coolpsonotes -- coolpsonotes --profile assistant mcp`.

## 5. Credential and key rotation

* **Client secret**: `coolpsonotes agent rotate-secret <agent> --secret-out
  new.secret` issues a new credential and revokes the old ones (their sessions
  end immediately; `--keep-old` for a zero-downtime switch, then delete the old
  one later). Re-run `agent setup` with the new client id and file — the
  agent's key pair stays the same.
* **Gateway token**: `coolpsonotes gateway token --rotate`, then restart the
  gateway.
* **Vault keys**: removing the agent from a vault (`vault unshare`) rotates the
  key; `vault rotate` does it on demand. Content written afterwards is
  unreadable for anyone without the new key.
* **Compromised agent machine**: delete the agent (`coolpsonotes agent delete`,
  or the web app) — this removes its memberships and sessions — and rotate the
  keys of the vaults it could read. Create a new agent rather than reusing
  keys.

## 6. Audit log

Every write through the gateway or MCP is appended to
`$XDG_DATA_HOME/coolpsonotes/<profile>/audit.jsonl` (mode 0600):

```json
{"at":1792378331454,"via":"gateway","op":"put_document","vault":"6ad6…","document":"052b…","bytes":18}
```

Operations: `put_document`, `create_document`, `update_document`,
`append_to_document`, `move_document`, `trash_document`/`delete_document`,
`put_attachment`. Entries contain time, channel, vault id, document id and size
— never content, titles or paths. On the server side, each update is
attributed to the agent's identity, and owners see membership and key events
in the vault audit (`GET /api/v1/vaults/{v}/audit`).

## 7. Threat notes

* **The gateway host holds plaintext** of every vault shared with the agent,
  and the agent's private key. Treat it like a server with secrets: full-disk
  encryption, an OS account of its own, no untrusted code running as that user.
  The local key file protects the replica only against copying the data
  directory, not against processes of the same user.
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
