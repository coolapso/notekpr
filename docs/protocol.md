# coolpsonotes sync protocol and HTTP API (v1)

All endpoints live under `/api/v1`. Requests and responses are JSON unless
noted. Binary values (keys, ciphertexts) are **standard base64 with padding**.
Timestamps are Unix **milliseconds**. The authoritative type definitions are in
[`crates/core/src/protocol.rs`](../crates/core/src/protocol.rs), shared by the
server and every Rust client (CLI, agent gateway, browser).

Authenticated calls send `Authorization: Bearer <token>`. Errors return
`{"error": "<code>", "message": "..."}` with codes `bad_request`,
`unauthorized`, `forbidden`, `not_found`, `conflict`, `stale_key` (412: content
was encrypted with an outdated key — the vault's or an individually shared
note's; reload keys and re-encrypt), `too_large`, `rate_limited` (429),
`unavailable`, `internal`. Vaults the caller is not a member of
always return `404` so their existence cannot be probed.

## 1. Data model the server sees

| Concept | Server knows | Server never knows |
|---|---|---|
| User | id, kind (human/agent), display name, e-mail (informational), OIDC `iss`+`sub` | passwords (none exist) |
| Account key | X25519 + Ed25519 public keys, self-signature | the account secret |
| Device | name, X25519 public key, status | device secret; account secret (only an HPKE ciphertext to the device) |
| Vault | id, owner, members + roles, key epoch, sequence head | name/description (encrypted `encrypted_meta`), vault keys (HPKE envelopes only) |
| Object | id (random), vault, created/updated time, size of ciphertexts | whether it is a note or the index, its path, title or content |
| Update / snapshot | ciphertext, author, sequence number | content |
| Attachment | id, size | name, type, path, content |

Each vault has exactly one **index object** (`object_id = "index"`), a CRDT map
from entry id to `{path, kind, created, modified, trashed?, size?, mime?}`. Every
note is its own object holding a CRDT text with the Markdown source. Both are
[Yjs/yrs](https://github.com/y-crdt/y-crdt) documents; payloads are yrs **update
v1** encodings, encrypted.

## 2. Encryption formats

Defined in `crates/core/src/crypto.rs` and `keys.rs`.

* **Symmetric envelope** (every update, snapshot, vault meta, attachment, key-chain link, prefs):
  `0x01 || epoch:u32be || nonce:24 || XChaCha20-Poly1305(ciphertext || tag)`.
  The key is `HKDF-SHA256(salt="coolpsonotes/v1/hkdf", ikm=vault_key_epoch, info=purpose-label)`.
  AAD = `"coolpsonotes/v1/aead" || purpose:u8 || header || len32(vault_id) || vault_id || len32(object_id) || object_id`.
  A ciphertext therefore cannot be moved to another vault, object, purpose or epoch.
* **HPKE sealed box** (vault key → member, account secret → device):
  `0x01 || enc:32 || ciphertext` using RFC 9180 base mode, DHKEM(X25519, HKDF-SHA256),
  HKDF-SHA256, ChaCha20-Poly1305, with domain-specific `info` and AAD binding the
  vault id, epoch and recipient user id (or user id and device id).
* **Signatures**: Ed25519 (strict) over length-prefixed statements:
  account key self-signature `(user_id, key_version, enc_public, sign_public)`,
  device approval `(user_id, device_id, device_public)`,
  recovery update `(user_id, recovery_envelope)`.
* **Recovery key**: 256 random bits + 16-bit checksum, base32 in groups of five.
  Wraps the account secret (symmetric envelope, purpose `Recovery`, context = user id).
* **Key chain**: when a vault moves to epoch N, `link_N = Enc(key_N, key_{N-1})`.
  A member holding the newest key can recover all older ones.
* **Document keys** (individually shared notes, §5.1): same envelope, but the
  epoch has its top bit set (`0x8000_0001`, `0x8000_0002`, …), so a ciphertext
  says whether it is under the vault key or the note's own key. A document key
  is wrapped for vault members under the current vault key (purpose `DocKey`,
  context `vault_id, object_id, epoch`) and for recipients with HPKE (scope
  `vault_id/object_id`). The note's title for recipients is encrypted under it
  (purpose `ObjectMeta`).

## 3. Authentication

| Endpoint | Purpose |
|---|---|
| `GET /info` | server name, version, `dev_mode`, `oidc_enabled`, size limits (no auth) |
| `GET /auth/oidc/start?return_to=/path` | 302 to the identity provider (authorization code + PKCE + nonce); sets an HttpOnly, SameSite=Lax `cpn_login` cookie that binds the login to this browser |
| `GET /auth/oidc/callback` | provider redirect target; requires the `cpn_login` cookie of the browser that started the login (otherwise `#error=login_failed`, which closes login CSRF); redirects to `/auth/complete#code=…&return_to=…` |
| `POST /auth/exchange {code}` | one-time code (120 s) → `SessionResponse {token, expires_at, user}` |
| `POST /auth/dev-login {username, display_name?}` | **development mode only** (`--dev-mode`), otherwise 404 |
| `POST /auth/logout` | revoke the current session |
| `GET /me/sessions`, `DELETE /me/sessions/{id}` | list / revoke own sessions |

Identities are keyed by `(iss, sub)`; the e-mail claim is never used for
identity. Successful login creates the user but grants access to nothing.

**Native clients** use the RFC 8628 device authorization grant:

1. `POST /oauth/device_authorization` (form: `client_id`, `client_name`) →
   `{device_code, user_code, verification_uri, verification_uri_complete, expires_in, interval}`.
2. The user opens `verification_uri_complete` in the web app (signed in via OIDC);
   the web app calls `GET /oauth/device?user_code=` and
   `POST /oauth/device/decision {user_code, approve}`.
3. The client polls `POST /oauth/token` (form: `grant_type=urn:ietf:params:oauth:grant-type:device_code`,
   `device_code`) and gets `authorization_pending`, `slow_down`, `access_denied`,
   `expired_token` or a `TokenResponse`.

**Agents** use the client credentials grant: `POST /oauth/token`
`grant_type=client_credentials` with HTTP Basic (`client_id:client_secret`) or
form fields. Tokens are short-lived (default 1 h).

## 4. Account keys, devices, recovery

| Endpoint | Notes |
|---|---|
| `GET /me` | `MeResponse {user, account_key?, has_recovery, session}` |
| `PUT /me/account-key` | one-time `AccountSetupRequest`. Humans must include `recovery_envelope` and `device` (first device with its account envelope and an approval signature). Agents send only `key`. |
| `GET /me/recovery` | the recovery envelope (encrypted under the recovery key) |
| `PUT /me/recovery {recovery_envelope, signature}` | rotate the recovery key; signature by the account key |
| `GET/PUT /me/prefs` | encrypted per-user data with `expected_version` optimistic concurrency |
| `GET /me/export` | `EncryptedExport`: everything the caller can read, as stored — vaults (keys, the whole log, attachments) and individually shared notes, plus the recovery envelope. For offline decryption with `coolpsonotes recover`; nothing in it is readable without the caller's keys. `coolpsonotes-server export-user` writes the same from a database or backup file. |
| `GET /me/audit` | the newest 200 audit entries about the caller (devices, keys, agents, sessions) |
| `PUT /me/session/device {device_id}` | label the current session with its device |
| `GET /devices` | own non-revoked devices |
| `POST /devices {device_id, name, enc_public}` | register a **pending** device; other connections get `user_event: device_pending` |
| `GET /devices/{id}/envelope` | account secret encrypted to that device (only when active) |
| `POST /devices/{id}/activate {account_envelope, signature}` | approve; `signature` = account signature over the device approval statement. Used by an existing device, or by the new device itself after recovery. |
| `PATCH /devices/{id} {name}`, `DELETE /devices/{id}` | rename / remove (revokes the device's sessions) |

Device approval UX: both devices display
`device_verification_code(user_id, device_id, device_public, account_public)`
(12 base32 characters, statement `device-code-v2`); the user approves only if
they match. The approving device uses its own account key; the new device uses
the published account key (`GET /me`, self-signature checked) and pins it. An
envelope that unwraps to a secret of any other account key is refused, so a
server can't hand a new device a secret of its own: anyone can encrypt to a
device's public key, but the code would differ.

## 5. Vaults, members, keys

| Endpoint | Role |
|---|---|
| `GET /vaults` | caller's vaults with role, `encrypted_meta`, `key_epoch`, `head_seq` |
| `POST /vaults {id, encrypted_meta, key_envelope}` | human; the client picks the id and wraps epoch 1 to itself |
| `GET/PATCH/DELETE /vaults/{v}` | read: member; update (`encrypted_meta`, `archived`): owner; delete: owner |
| `GET /vaults/{v}/keys` | member: own envelopes for every epoch granted + full key chain |
| `GET /vaults/{v}/members` | member: `MemberInfo {user (with account key), role, has_current_key}` |
| `POST /vaults/{v}/members {user_id, role, key_envelope?}` | owner. Without an envelope the member is "pending keys" until the owner's client grants them |
| `PATCH /vaults/{v}/members/{u} {role}` | owner |
| `DELETE /vaults/{v}/members/{u}` | owner removes; any non-owner can remove themselves |
| `POST /vaults/{v}/keys/grant {envelopes}` | owner fills missing envelopes for the current epoch |
| `POST /vaults/{v}/keys/rotate {new_epoch, chain_link, encrypted_meta, envelopes}` | owner; `new_epoch = current + 1`; envelopes must cover exactly the members that have account keys |
| `GET /vaults/{v}/audit` | owner: membership, key and deletion events (no content) |
| `GET /users/search?q=` | humans matching a display-name prefix or exact e-mail, plus the caller's own agents |
| `GET /users/{id}` | public profile + account key of a user the caller may interact with |

Clients **must** call `PublicAccountKey::verify()` before wrapping a key to a
fetched public key, and should pin fingerprints (trust on first use) and show
them for out-of-band comparison.

### 5.1 Sharing single notes

A vault owner can share one note with people who are **not** members of the
vault. The note gets its own *document key*; recipients receive only that key
and can reach only that note — never the vault index, other notes, attachments,
member list or vault key.

| Endpoint | Role |
|---|---|
| `PUT /vaults/{v}/objects/{o}/key` `ObjectKeyUpdate {epoch, vault_envelope, chain_link?, envelopes, encrypted_meta, snapshot?}` | owner. **First share**: `epoch = 0x8000_0001`, plus a `snapshot` of the note's full state under the new key whose `covers_seq` covers every stored update of the note (else `412 stale_key`). **Rotation**: `epoch = current + 1`, a `chain_link`, and `envelopes` for exactly the current recipients (else `409`). `vault_envelope` must be wrapped under the vault's *current* epoch. |
| `GET /vaults/{v}/objects/{o}/keys` | member or recipient: `ObjectKeysResponse {role, current_epoch, envelopes (own), chain, encrypted_meta}` |
| `PUT /vaults/{v}/objects/{o}/meta {encrypted_meta}` | writer: the title recipients see, under the current document key |
| `GET /vaults/{v}/objects/{o}/shares` | member: `ObjectShareInfo {user, role, added_at, added_by, has_current_key}` |
| `POST /vaults/{v}/objects/{o}/shares {user_id, role, envelope}` | owner; `role` is `editor` or `viewer`; the envelope wraps the current document key to the person. Vault members are refused (`409`). |
| `PATCH /vaults/{v}/objects/{o}/shares/{u} {role}`, `DELETE …/shares/{u}` | owner; recipients may remove themselves. Clients rotate the document key after removing someone. |
| `GET /shares` | notes shared with the caller: `SharedNote {vault_id, object_id, role, owner, shared_by, key_epoch, encrypted_meta, added_at}` |
| `GET /vaults/{v}/objects/{o}/changes?since=` | that note's part of the log (same shape as `/changes`) |

Recipients use `GET /vaults/{v}/objects/{o}[/snapshots[/{id}]]`, `/changes` and
`POST /vaults/{v}/updates` / snapshots for that note only. Reads by recipients
return **only payloads under the document key** (epoch top bit set): the note's
history from before it was shared, which is under the vault key, is never handed
out. `GET /vaults/{v}/keys` gives members the document-key envelopes
(`objects`) and chains (`object_chain`) of the vault's shared notes.

Once a note has a document key, the server accepts its updates and snapshots
only under the **current** document epoch; anything else is refused with
`412 stale_key` (an older client still using the vault key, or a key that was
just rotated). Clients reload keys and re-send. When a vault key is rotated,
the owner's client also rotates every document key (removed members knew them).

## 6. The encrypted log

Each vault has one monotonically increasing sequence counter. Every accepted
update and every purge consumes one sequence number. The server writes and
publishes while holding a single write lock, so **subscribers always observe a
vault's events in strictly increasing order without gaps.**

| Endpoint | Notes |
|---|---|
| `POST /vaults/{v}/updates {updates: [{object_id, client_update_id, payload}]}` | editor/owner. Idempotent per `client_update_id`: a retry returns the original `seq`. Objects are created on first write. Writing to a purged object → `409`. |
| `GET /vaults/{v}/changes?since=S&limit=N&snapshots=true` | `ChangesResponse {updates, snapshots, purged, head_seq, has_more, next_since}`. For any object whose latest snapshot covers part of `(S, head]`, the snapshot is returned **instead of** the updates it covers. Request further pages with `since=next_since&snapshots=false`. |
| `GET /vaults/{v}/objects/{o}` | `ObjectState`: latest snapshot + updates after it |
| `POST /vaults/{v}/objects/{o}/snapshots {snapshot_id, covers_seq, payload}` | editor/owner. Store a compaction (`payload` = encrypted full state). Updates covered by the *previous* snapshot are pruned — one generation is always kept so a bad snapshot is recoverable. One snapshot per person and note per `--snapshot-cooldown-secs` (default 30 s; `429` otherwise). |
| `GET /vaults/{v}/objects/{o}/snapshots[/{id}]` | revision history (newest first); default retention 100 per object |
| `DELETE /vaults/{v}/objects/{o}` | editor/owner: permanently delete (emptying the trash). Not allowed for `index`. |
| `PUT/GET/DELETE /vaults/{v}/attachments/{id}` | raw encrypted bytes (`application/octet-stream`). Upload: editor/owner; immutable (same id+size re-upload is an idempotent retry). |

**Client rules**

* Encrypt every update with purpose `Update` (snapshots: `Snapshot`) and context `(vault_id, object_id)`, always using the **current** epoch. The server refuses older epochs with `412 stale_key` and unknown newer ones with `400`: reload keys, re-encrypt pending changes and send them again.
* Pages of `/changes` are bounded by row count (`limit`, at most 5000) and by size (16 MiB of update payloads); keep requesting while `has_more`.
* Apply updates idempotently (CRDT). Track `vault_seq` = highest sequence received (updates, acks, purges, `synced.head_seq`).
* A snapshot's `covers_seq` must be ≥ 1 and ≤ the client's `vault_seq` at the time it encoded the state, and the client must have applied every update of that object up to there. REST push responses do not advance `vault_seq` (they can overtake other clients' updates); a REST-only client therefore catches up via `/changes` before creating a snapshot.
* Compact when an object has accumulated many updates (clients use ~200) or after an editing session ends; add random jitter.
* Persist unacknowledged updates and retry them with the same `client_update_id` after reconnecting.
* If the server reports a `head_seq` lower than one seen before, warn the user (possible rollback/restore) and re-push local state; CRDT merging restores anything missing.

## 7. WebSocket `/api/v1/ws`

JSON text frames, tagged with `"type"`.

Client → server: `auth {token}` (must be first, within 10 s),
`subscribe {vault_id, since?, object_id?}`, `unsubscribe {vault_id, object_id?}`,
`push {vault_id, updates}`, `awareness {vault_id, object_id, payload}`,
`ping {nonce}`.

Server → client: `ready {user_id, session_id, conn_id}`,
`changes {vault_id, changes}` (catch-up pages after `subscribe` with `since`),
`synced {vault_id, head_seq, object_id?}`, `update {vault_id, update}`,
`ack {vault_id, results}` (your own pushes, delivered in log order),
`snapshot {vault_id, snapshot}` (a snapshot someone else stored), `snapshot_created {…}` (yours),
`purged {vault_id, object_id, seq}`,
`awareness {vault_id, object_id, from_user, from_conn, payload}` (empty payload = that connection left),
`vault_event {vault_id, event}` (`members_changed`, `key_rotated`, `meta_changed`, `deleted`, `access_revoked`, `role_changed`),
`object_event {vault_id, object_id, event}` (`shared`, `key_rotated`, `meta_changed`, `shares_changed`, `access_revoked`, `deleted`),
`user_event {event}` (`device_pending`, `devices_changed`, `vaults_changed`, `shares_changed`, `session_revoked`),
`error {code, message, vault_id?}`, `pong {nonce}`.

A client must subscribe to a vault before pushing to it over the socket.
Pushes and (un)subscriptions share a per-connection budget (20 per second,
bursts of 400); beyond it the server answers `error {code: "rate_limited"}`
and drops the message, so clients keep the changes pending and retry.
Awareness payloads are encrypted by clients (purpose `Awareness`); the server
only relays them (rate-limited, ≤ 16 KiB).

**Single-note subscriptions** (`object_id` set) are for recipients of an
individually shared note: they deliver only that note's updates, snapshots,
awareness and `object_event`s (catch-up limited to document-key payloads, as
in §5.1). Awareness sent with a non-empty `object_id` reaches that note's
subscribers too, so members working on a shared note announce themselves there
as well (encrypted under the document key); clients that already know a
connection vault-wide ignore its note-level messages. When a connection closes,
the server sends an empty awareness on the vault and on every note it
announced itself on.

## 8. Agents

| Endpoint | Notes |
|---|---|
| `GET /agents`, `POST /agents {name}` | owner's agents; creation returns a client id + secret **once** |
| `DELETE /agents/{id}` | deletes the agent, its memberships and sessions |
| `POST /agents/{id}/credentials`, `DELETE /agents/{id}/credentials/{client_id}` | rotate / revoke secrets (revocation ends sessions) |

An agent creates its own account key (`PUT /me/account-key` with only `key`).
Its owner shares vaults with it like with any person, choosing viewer or editor.
Agents cannot create vaults, manage members, devices or agents.

## 9. Administration

| Endpoint | Notes |
|---|---|
| `GET /admin/users` | administrators: accounts with kind, e-mail, owner (agents), admin and disabled flags, last sign-in, vault and device counts — no content |
| `PATCH /admin/users/{id} {disabled?, is_admin?}` | disable (ends the person's sessions and their agents') or (un)make administrator |
| `GET /admin/stats` | counts of users, agents, vaults, objects, updates, snapshots and attachments, and total storage |

## 10. Operations endpoints

`/healthz` (liveness), `/readyz` (database reachable), `/metrics` (Prometheus;
optional bearer token). All responses carry a strict CSP and security headers.
