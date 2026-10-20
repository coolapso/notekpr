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
`device_revoked` (404 from `GET /devices/{id}/envelope` and
`PUT /me/session/device`: the device was removed from the account or never
belonged to it — the only 404 after which a client may forget a device's
keys), `unavailable`, `internal`. Vaults the caller is not a member of
always return `404` so their existence cannot be probed.

## 1. Data model the server sees

| Concept | Server knows | Server never knows |
|---|---|---|
| User | id, kind (human/agent), display name, e-mail (informational), OIDC `iss`+`sub` | passwords (none exist) |
| Account key | X25519 + Ed25519 public keys, self-signature | the account secret |
| Device | name, X25519 public key, status | device secret; account secret (only an HPKE ciphertext to the device) |
| Vault | id, owner, members + roles, key epoch, sequence head | name, description, icon and picture (encrypted `encrypted_meta`: `VaultMeta` JSON; the picture is a raster image of at most 128 px and 8 KiB, base64), vault keys (HPKE envelopes only) |
| Object | id (random), vault, created/updated time, size of ciphertexts | whether it is a note or the index, its path, title or content |
| Update / snapshot | ciphertext, author, sequence number | content |
| Attachment | id, size | name, type, path, content |

Each vault has exactly one **index object** (`object_id = "index"`), a CRDT map
from entry id to `{path, kind, created, modified, trashed?, size?, mime?, icon?}`
(`icon`: a note's or folder's emoji, at most 16 characters; longer ones are
not shown). Every
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
  recovery update `(user_id, recovery_envelope)`, and since R3
  `recovery-update-v2` `(user_id, version:u64be, recovery_envelope)`.
* **Recovery key**: 256 random bits + 16-bit checksum, base32 in groups of five.
  Wraps the account secret (symmetric envelope, purpose `Recovery`, context = user id).
* **Key chain**: when a vault moves to epoch N, `link_N = Enc(key_N, key_{N-1})`.
  A member holding the newest key can recover all older ones.
* **Document keys** (every note's own key, §5.1): same envelope, but the
  epoch has its top bit set (`0x8000_0001`, `0x8000_0002`, …), so a ciphertext
  says whether it is under the vault key or the note's own key. A document key
  is wrapped for vault members under the current vault key (purpose `DocKey`,
  context `vault_id, object_id, epoch`) and for recipients with HPKE (scope
  `vault_id/object_id`). The note's title and icon for recipients are
  encrypted under it (purpose `ObjectMeta`: `{title, icon?}` JSON). The vault index and attachments stay under the vault
  key.

## 3. Authentication

| Endpoint | Purpose |
|---|---|
| `GET /info` | server name, version, `dev_mode`, `oidc_enabled`, size limits (`max_attachment_bytes`, `max_update_bytes`, `max_snapshot_bytes`; older servers omit the last: it is `max(16 × max_update_bytes, 32 MiB)`), `note_keys` (notes can have their own keys, §5.1) (no auth) |
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
| `PUT /me/recovery {recovery_envelope, signature, version?}` | rotate the recovery key; signature by the account key. With a `version` (clients use the time in ms) the signature covers it and the server accepts only a higher one than the last (`409` otherwise); once one is stored, unversioned updates are refused too, so older updates can't be replayed |
| `GET/PUT /me/prefs` | encrypted per-user data with `expected_version` optimistic concurrency. Inside the encryption the web client keeps a `counter` that goes up with every save, and its own sealed copy: an older copy from the server is not used |
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
| `POST /vaults {id, encrypted_meta, key_envelope, log_entry?}` | human; the client picks the id and wraps epoch 1 to itself; `log_entry` starts the membership log (§5.2) |
| `GET/PATCH/DELETE /vaults/{v}` | read: member; update (`encrypted_meta`, `archived`): owner; delete: owner |
| `GET /vaults/{v}/keys` | member: own envelopes for every epoch granted + full key chain; the notes' document keys (`objects`, `object_chain`) and which notes are shared individually (`shared`; absent on servers from before notes had their own keys) |
| `GET /vaults/{v}/members` | member: `MemberInfo {user (with account key), role, has_current_key}` |
| `POST /vaults/{v}/members {user_id, role, key_envelope?, log_entry?}` | owner. Without an envelope the member is "pending keys" until the owner's client grants them |
| `PATCH /vaults/{v}/members/{u} {role, log_entry?}` | owner |
| `DELETE /vaults/{v}/members/{u}` (optional body `{log_entry}`) | owner removes; any non-owner can remove themselves |
| `GET /vaults/{v}/log?since=` | member: the membership log, `MemberLogResponse {entries}` (§5.2) |
| `POST /vaults/{v}/log {entry}` | owner: a `checkpoint` (vaults without a log), a `rekey`, a `share` of a note already shared, `require_signed`, or a `remove` of someone who already left (§5.2) |
| `POST /vaults/{v}/keys/grant {envelopes}` | owner fills missing envelopes for the current epoch |
| `POST /vaults/{v}/keys/rotate {new_epoch, chain_link, encrypted_meta, envelopes, objects, log_entry?}` | owner; `new_epoch = current + 1`; envelopes must cover exactly the members that have account keys; `objects` (`ObjectKeyChange {object_id, update}`) moves **every** note with a document key to its next epoch in the same transaction (each a rotation as in §5.1, wrapped under the new vault key), else `409` |
| `GET /vaults/{v}/audit` | owner: membership, key and deletion events (no content) |
| `GET /users/search?q=` | humans matching a display-name prefix or exact e-mail, plus the caller's own agents |
| `GET /users/{id}` | public profile + account key of a user the caller may interact with |

Clients **must** call `PublicAccountKey::verify()` before wrapping a key to a
fetched public key, and should pin fingerprints (trust on first use) and show
them for out-of-band comparison.

### 5.1 Note keys and sharing single notes

Every note has its own *document key*. A client creating a note registers a
key for it (**private**: wrapped only under the vault key) *before* uploading
any of the note's content, so none of it is ever under the vault key. Notes
from before this (under the vault key) move to their own key lazily, at their
next snapshot. The index and attachments stay under the vault key.

A vault owner can share one note with people who are **not** members of the
vault. Sharing starts with a **fresh** document key that has no chain link to
the note's earlier keys; the server remembers that epoch (`share_from`) and
hands recipients only content from it on. Recipients receive only that key and
can reach only that note — never the vault index, other notes, attachments,
member list, vault key, or the note's history from before it was shared.

| Endpoint | Role |
|---|---|
| `PUT /vaults/{v}/objects/{o}/key` `ObjectKeyUpdate {epoch, vault_envelope, chain_link?, envelopes, encrypted_meta, snapshot?, private?}` | See the three kinds below. `vault_envelope` must be wrapped under the vault's *current* epoch (else `412 stale_key`). A `snapshot` must be under the new key and its `covers_seq` must cover every stored update of the note (else `412 stale_key`). |
| `GET /vaults/{v}/objects/{o}/keys` | member or recipient: `ObjectKeysResponse {role, current_epoch, envelopes (own), chain, encrypted_meta, member_envelopes?, shared?}`. Recipients: the chain only from `share_from` on; their envelopes carry the owner's grant `signature` (below). Members: the note's key at every epoch wrapped under the vault key (`member_envelopes`), every chain link, and whether it is `shared` — so after a note's `key_rotated` / `shared` event they reload that note's keys alone, not the vault's |
| `PUT /vaults/{v}/objects/{o}/keys/signatures {grants: [{user_id, epoch, signature}]}` | owner: grant signatures for keys given before grants were signed |
| `PUT /vaults/{v}/objects/{o}/meta {encrypted_meta}` | writer: the title recipients see, under the current document key |
| `GET /vaults/{v}/objects/{o}/shares` | member: `ObjectShareInfo {user, role, added_at, added_by, has_current_key, key_signed}` |
| `POST /vaults/{v}/objects/{o}/shares {user_id, role, envelope, log_entry?}` | owner; `role` is `editor` or `viewer`; the envelope wraps the current document key to the person. Vault members are refused (`409`), and so is a note whose sharing has not started (`409`: give it a fresh key first). |
| `PATCH /vaults/{v}/objects/{o}/shares/{u} {role}`, `DELETE …/shares/{u}` | owner; recipients may remove themselves. Clients rotate the document key after removing someone. |
| `GET /shares` | notes shared with the caller: `SharedNote {vault_id, object_id, role, owner, shared_by, key_epoch, encrypted_meta, added_at}` |
| `GET /vaults/{v}/objects/{o}/changes?since=` | that note's part of the log (same shape as `/changes`) |

The kinds of `PUT …/key`:

| Kind | Request | Who |
|---|---|---|
| **Private first key** | `private: true`, `epoch = 0x8000_0001`, no chain link, no envelopes; a covering `snapshot` only if the note has content already (lazy move of an older note) | any writer, agents included |
| **Sharing starts** | `private: false` and either the first key (`0x8000_0001`) or `current + 1` **without** a chain link; a covering `snapshot` is required; no envelopes (people are added afterwards); refused (`409`) while the note has recipients | owner |
| **Rotation** | `epoch = current + 1` with a `chain_link`, and `envelopes` for exactly the current recipients (else `409`) | owner |

**Grant signatures.** Every note key the owner gives someone it is shared
with (`envelopes` of a share, a rotation or a vault rotation) carries
`signature`: Ed25519 by the owner over `statement("coolpsonotes/v1/sig/note-key",
[vault_id, object_id, user_id, epoch:u32be, commitment])`, the commitment
being BLAKE3 (context `coolpsonotes v1 note key commitment`) over
`statement("note-key", [vault_id, object_id, epoch:u32be, key])`. Recipients
use only their newest envelope, and only if the signature verifies with the
owner's pinned key; older keys follow from the chain. So a server can't hand
them a key of its own. Owners' clients sign the grants of keys given before
this when they open the vault (`key_signed` says which).

A repeated private first key for a note that has one is refused (`409`;
clients reload keys and see it registered). Recipients use
`GET /vaults/{v}/objects/{o}[/snapshots[/{id}]]`, `/changes` and
`POST /vaults/{v}/updates` / snapshots for that note only. Reads by recipients
return **only payloads under a document epoch at or after `share_from`**.

Once a note has a document key, the server accepts its updates and snapshots
only under the **current** document epoch; anything else is refused with
`412 stale_key` (an older client still using the vault key, or a key that was
just rotated). Clients reload keys and re-send. Rotating the vault key renews
every note's key in the same request (removed members knew them).

### 5.2 The membership log

Every vault has a **membership log** (`crates/core/src/memberlog.rs`): a
hash chain of entries signed by the vault's owner (a `leave` is signed by the
member who leaves). Clients verify it against the owner's key — their own, or
the one they pinned for the owner — so the server can neither add members nor
hand out vault keys the owner didn't make.

`LogEntry {seq, prev, signer, signed_at, body, signature}`; `seq` counts from
0, `prev` is the previous entry's hash (32 zero bytes for the first). Bodies
(`kind`):

| `kind` | Fields | Means |
|---|---|---|
| `create` | `owner` (member), `key_commit` | first entry of a new vault; commits to epoch 1's key |
| `checkpoint` | `members`, `epoch`, `key_commit` | first entry of a vault from before the log: its members as the owner's client saw them, trusted from here on |
| `add` | `member` | someone joins (keys pinned; empty keys: "pending keys") |
| `role` | `user_id`, `role` | a role changes (never to or from owner) |
| `rekey` | `member` | the owner pins a member's (new) account keys |
| `remove` / `leave` | `user_id` | someone is removed by the owner / leaves |
| `rotate` | `epoch`, `key_commit` | the vault key moves to `epoch` (current + 1) |
| `share` | `object_id`, `member` | a note is shared with someone who isn't a member (or their role on it changes) |
| `unshare` | `object_id`, `user_id` | a note is no longer shared with them |
| `require_signed` | `from_seq` | content after log sequence `from_seq` (snapshots covering more) must be signed by its author |

A member is `{user_id, role, enc_public, sign_public}`. The signature is
Ed25519 over `statement("coolpsonotes/v1/sig/member-log", [vault_id,
seq:u64be, prev, signer, signed_at:i64be, body])`, where `body` is a
length-prefixed encoding of the fields (never JSON). An entry's hash is
BLAKE3 (derive-key mode, context `coolpsonotes v1 member-log entry`) over its
fields and signature. A key commitment is BLAKE3 (context `coolpsonotes v1
vault key commitment`) over `statement("key", [vault_id, epoch:u32be, key])`.

The server appends the entry that comes with a membership or key change in
the same transaction, after checking that it continues the log, is signed by
the caller with their stored account key, and says exactly what the request
does (`400` otherwise, `409` if the log moved on meanwhile). Once a vault has
a log, changes without an entry are refused with `409` (leaving excepted).
Clients:

* verify the whole log, and that it still contains the head they verified
  before (no rollback, no rewritten history);
* check every vault key they hold against its epoch's commitment (epochs from
  before a checkpoint have none) and don't use a key that fails;
* as the owner, wrap vault keys only for members the log lists with that role
  and key, and pin a pending member's keys (`rekey`) before granting them;
* as the owner of a vault without a log, sign a `checkpoint` when they open it;
  as the owner of a log without `require_signed`, sign a `share` for every
  note already shared (keys checked against pins), then `require_signed` with
  the vault's current head. New vaults get it right after `create`;
* sign every update and snapshot (§6), and check what they receive: content
  after `from_seq` must be signed, and a signature must verify with a key the
  log ever pinned for the author while they could write the vault (or, for a
  single note, while it was shared with them as an editor). Content by an
  author the client's copy of the log doesn't know is kept aside (the sync
  cursor stops before it) until the log is reloaded, then checked again;
  what still fails is refused, never applied.

The server refuses unsigned content once the requirement is set, and content
whose signature doesn't verify with the caller's account key. Single-note
shares (`POST`/`PATCH`/`DELETE …/objects/{o}/shares…`) carry their `share`
or `unshare` entry (a recipient removing a note from their own list doesn't
need one).

## 6. The encrypted log

Each vault has one monotonically increasing sequence counter. Every accepted
update and every purge consumes one sequence number. The server writes and
publishes while holding a single write lock, so **subscribers always observe a
vault's events in strictly increasing order without gaps.**

| Endpoint | Notes |
|---|---|
| `POST /vaults/{v}/updates {updates: [{object_id, client_update_id, payload, signature?}]}` → `{results, head_seq, rejected?}` | editor/owner. Idempotent per `client_update_id`: a retry returns the original `seq`. Objects are created on first write. An update that can't be stored on its own merits is refused alone and listed in `rejected: [{client_update_id, object_id, code, message}]` with `code` `too_large` (over `max_update_bytes`), `purged` (the object was permanently deleted), `stale_key` (encrypted with an outdated key: reload keys, re-encrypt, send again), `unsigned` (the vault requires signed content, §5.2) or `bad_signature` (not signed with the author's account key); the rest of the batch is stored. Malformed requests, a batch over 500 updates and missing write access still fail the whole push. (Servers before this answered `413` / `409` / `412` for the whole batch; clients handle both.) |
| `GET /vaults/{v}/changes?since=S&limit=N&snapshots=true` | `ChangesResponse {updates, snapshots, purged, head_seq, has_more, next_since}`. For any object whose latest snapshot covers part of `(S, head]`, the snapshot is returned **instead of** the updates it covers. Request further pages with `since=next_since&snapshots=false`. Every page also holds the snapshots *carrying* changes (below) stored while the head was within it, so each catch-up passes them once. |
| `GET /vaults/{v}/objects/{o}` | `ObjectState`: latest snapshot + updates after it |
| `POST /vaults/{v}/objects/{o}/snapshots {snapshot_id, covers_seq, payload, signature?, carries?}` | editor/owner (`409` unsigned where signatures are required, `400` for a bad signature). Store a compaction (`payload` = encrypted full state). Updates covered by the *previous* snapshot are pruned — one generation is always kept so a bad snapshot is recoverable. One snapshot per person and note per `--snapshot-cooldown-secs` (default 30 s; `429` otherwise). `carries: true`: the state holds changes the log doesn't have — an update too large to push (`too_large`), which a client sends this way instead; the server sends the snapshot to everyone subscribed (`snapshot`) and with catch-ups (above), and its retention keeps the newest such snapshot of each note. |
| `GET /vaults/{v}/objects/{o}/snapshots[/{id}]` | revision history (newest first); default retention 100 per object |
| `DELETE /vaults/{v}/objects/{o}` | editor/owner: permanently delete (emptying the trash). Not allowed for `index`. |
| `PUT/GET/DELETE /vaults/{v}/attachments/{id}` | raw encrypted bytes (`application/octet-stream`). Upload: editor/owner; immutable (same id+size re-upload is an idempotent retry). |

**Client rules**

* Sign every update and snapshot with your account key (§5.2): updates over
  `statement("coolpsonotes/v1/sig/update", [vault_id, object_id, payload])`,
  snapshots over `statement("coolpsonotes/v1/sig/snapshot", [vault_id,
  object_id, covers_seq:i64be, payload])`. `Update` and `Snapshot` carry the
  `signature` (null for content from before signing).
* Encrypt every update with purpose `Update` (snapshots: `Snapshot`) and context `(vault_id, object_id)`, always using the **current** epoch. The server refuses older epochs with `412 stale_key` and unknown newer ones with `400`: reload keys, re-encrypt pending changes and send them again.
* Pages of `/changes` are bounded by row count (`limit`, at most 5000) and by size (16 MiB of update payloads); keep requesting while `has_more`.
* Apply updates idempotently (CRDT). Track `vault_seq` = highest sequence received (updates, acks, purges, `synced.head_seq`).
* A snapshot's `covers_seq` must be ≥ 1 and ≤ the client's `vault_seq` at the time it encoded the state, and the client must have applied every update of that object up to there. REST push responses do not advance `vault_seq` (they can overtake other clients' updates); a REST-only client therefore catches up via `/changes` before creating a snapshot.
* Compact when an object has accumulated many updates (clients use ~200) or after an editing session ends; add random jitter.
* An update larger than `max_update_bytes` that can't be split goes out as a snapshot of the object with `carries: true` (if the state fits `max_snapshot_bytes`); once stored, the update is done. Updates after it then reach everyone, as their CRDT dependencies arrive with it.
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
`ack {vault_id, results, rejected?}` (your own pushes, delivered in log order; `rejected` as for the REST push),
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
