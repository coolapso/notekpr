# notekpr sync protocol and HTTP API (v1)

All endpoints live under `/api/v1`, except the operations endpoints at the
root (§10). Requests and responses are JSON unless noted. Binary values (keys,
ciphertexts) are **standard base64 with padding**. Timestamps are Unix
**milliseconds**. The authoritative type definitions are in
[`crates/core/src/protocol.rs`](../crates/core/src/protocol.rs), shared by the
server and every Rust client (CLI, agent gateway, browser).

Authenticated calls send `Authorization: Bearer <token>`. Every app also
sends `Notekpr-App: <app>/<version>` with each request — `web/0.3.0`,
`cli/0.3.0`: which app it is and the release it was built as (`dev` for
builds not made for a release) — so the server can later tell an app too
old for it to update. The server accepts requests without it (older apps,
scripts). Errors return
`{"error": "<code>", "message": "..."}` (`/oauth/token` uses the OAuth shape
instead, §3) with codes `bad_request`, `unauthorized`, `forbidden`,
`not_found`, `conflict`, `stale_key` (412: content was encrypted with an
outdated key — the vault's or an individually shared note's; reload keys and
re-encrypt; pushes list it per update instead, §6), `too_large`,
`rate_limited` (429; with `Retry-After: <seconds>` when the request wasn't
handled at all, so the same request may be sent again then — the CLI waits
and retries up to 30 s),
`device_revoked` (404 from `GET /devices/{id}/envelope` and
`PUT /me/session/device`: the device was removed from the account or never
belonged to it — the only 404 after which a client may forget a device's
keys), `read_only` (503: the server refuses changes for now, §9 — clients
keep note edits and send them once it says it accepts changes again, §7;
other changes, such as creating a vault, fail and are tried again by the
person), `limit_reached` (403: an instance limit on accounts, vaults,
agents or quick notes, §9, §5.3; the message says which), `unavailable`, `internal`. Vaults the
caller is not a member of always return `404` so their existence cannot be
probed.

## 1. Data model the server sees

| Concept | Server knows | Server never knows |
|---|---|---|
| User | id, kind (human/agent), display name, e-mail (informational), OIDC `iss`+`sub` | passwords (none exist) |
| Account key | X25519 + Ed25519 public keys, self-signature | the account secret |
| Device | name, X25519 public key, status | device secret; account secret (only an HPKE ciphertext to the device) |
| Vault | id, owner, members + roles, key epoch, sequence head | name, description, icon and picture (encrypted `encrypted_meta`: `VaultMeta` JSON; the picture is a raster image of at most 128 px and 8 KiB, base64), vault keys (HPKE envelopes only) |
| Object | id (random; the index's is always `index`), vault, creator, created/updated time, size of ciphertexts | its path, title or content |
| Update / snapshot | ciphertext, author (for updates also the author's device), sequence number, time | content |
| Attachment | id, size, uploader, upload time | name, type, path, content |

Each vault has exactly one **index object** (`object_id = "index"`), a CRDT map
from entry id to `{path, kind, created, modified, trashed?, size?, mime?, icon?, order?}`
(`icon`: a note's or folder's emoji, at most 16 characters; longer ones are
not shown. `order`: its place among the entries of its folder when put there
by hand, a string of base-62 digits `0-9A-Za-z` compared bytewise, not ending
in `0`, at most 32 long; others are ignored. Folders come first, then entries
with an `order` by it, then the rest A–Z; equal keys sort A–Z. Clients drop
it when an entry moves to another folder — `crates/core/src/order.rs`). Every
note is its own object holding a CRDT text with the Markdown source. Both are
[Yjs/yrs](https://github.com/y-crdt/y-crdt) documents; payloads are yrs **update
v1** encodings, encrypted.

## 2. Encryption formats

Defined in `crates/core/src/crypto.rs` and `keys.rs`.

* **Symmetric envelope** (every update, snapshot, vault meta, attachment, key-chain link, prefs):
  `0x01 || epoch:u32be || nonce:24 || XChaCha20-Poly1305(ciphertext || tag)`.
  The key is `HKDF-SHA256(salt="notekpr/v1/hkdf", ikm=vault_key_epoch, info=purpose-label)`.
  AAD = `"notekpr/v1/aead" || purpose:u8 || header || len32(vault_id) || vault_id || len32(object_id) || object_id`.
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
| `GET /info` | server name, `version` (the release it was built as; `dev` for builds not made for a release), `dev_mode`, `oidc_enabled`, `oidc_display_name` (the login button's label; null without OIDC), size limits (`max_attachment_bytes`, `max_update_bytes`, `max_snapshot_bytes`; older servers omit the last: it is `max(16 × max_update_bytes, 32 MiB)`), `note_keys` (notes can have their own keys, §5.1), `read_only` (`manual` or `storage_full` while the server refuses changes, §9; absent otherwise), `purge_undo_days` (how long deleting something for good can be undone, §6; 0 or absent: not at all) (no auth) |
| `GET /auth/oidc/start?return_to=/path` | 303 to the identity provider (authorization code + PKCE + nonce); sets an HttpOnly, SameSite=Lax `notekpr_login` cookie that binds the login to this browser |
| `GET /auth/oidc/callback` | provider redirect target; requires the `notekpr_login` cookie of the browser that started the login (otherwise `#error=login_failed`, which closes login CSRF); redirects to `/auth/complete#code=…&return_to=…` |
| `POST /auth/exchange {code}` | one-time code (120 s) → `SessionResponse {token, expires_at, user}` |
| `POST /auth/dev-login {username, display_name?}` | **development mode only** (`--dev-mode`), otherwise 404 |
| `POST /auth/logout` | revoke the current session |
| `GET /me/sessions`, `DELETE /me/sessions/{id}` | list / revoke own sessions |

Identities are keyed by `(iss, sub)`; the e-mail claim is never used for
identity. Successful login creates the user but grants access to nothing.
A first login is refused while the server is read-only or at its accounts
limit (§9): `dev-login` answers `read_only` / `limit_reached`, the OIDC
callback redirects with `#error=no_new_accounts`. People who have an account
still sign in.

**Native clients** use the RFC 8628 device authorization grant:

1. `POST /oauth/device_authorization` (form: `client_id`, `client_name`) →
   `{device_code, user_code, verification_uri, verification_uri_complete, expires_in, interval}`.
2. The user opens `verification_uri_complete` in the web app (signed in via OIDC);
   the web app calls `GET /oauth/device?user_code=` →
   `{user_code, client_name, created_at, expires_at, requested_from?}` and
   `POST /oauth/device/decision {user_code, approve}`. `requested_from` is
   the address step 1 came from (with `--trust-proxy`, the one the proxy
   reports), shown so a code someone else asked for — sent to the user to
   phish an approval — stands out. The server keeps it only with the pending
   code and never logs it ([security](security.md)).
3. The client polls `POST /oauth/token` (form: `grant_type=urn:ietf:params:oauth:grant-type:device_code`,
   `device_code`) and gets `authorization_pending`, `slow_down`, `access_denied`,
   `expired_token`, `invalid_grant` (unknown or used code), `invalid_request`
   (no `device_code`) or a `TokenResponse`.

**Agents** use the client credentials grant: `POST /oauth/token`
`grant_type=client_credentials` with HTTP Basic (`client_id:client_secret`) or
form fields. Tokens are short-lived (default 1 h). Missing or wrong
credentials (or a revoked secret, a disabled agent or owner) get `401
invalid_client`.

Errors from `/oauth/token` are RFC 6749 `{error, error_description?}` (`400`
unless noted), not the `{error, message}` shape above; an unknown
`grant_type` gets `unsupported_grant_type`.

## 4. Account keys, devices, recovery

| Endpoint | Notes |
|---|---|
| `GET /me` | `MeResponse {user, account_key?, has_recovery, session}` |
| `PUT /me/account-key` | one-time `AccountSetupRequest`. Humans must include `recovery_envelope` and `device` (first device with its account envelope and an approval signature). Agents send only `key`. |
| `GET /me/recovery` | the recovery envelope (encrypted under the recovery key) |
| `PUT /me/recovery {recovery_envelope, signature, version?}` | rotate the recovery key; signature by the account key. With a `version` (clients use the time in ms) the signature covers it and the server accepts only a higher one than the last (`409` otherwise); once one is stored, unversioned updates are refused too, so older updates can't be replayed |
| `GET/PUT /me/prefs` | encrypted per-user data with `expected_version` optimistic concurrency. Inside the encryption the web client keeps a `counter` that goes up with every save, and its own sealed copy: an older copy from the server is not used |
| `GET /me/export` | `EncryptedExport`: everything the caller can read, as stored — vaults (keys, the whole log, attachments, `owner_id`) and individually shared notes (with their vault's `owner_id`), plus the recovery envelope and `users` (`{id, display_name}` of the members of the caller's vaults and of the authors of what it holds; for notes shared with the caller only their owner and authors, never their vault's other members), so a recovery can name comments' authors. For offline decryption with `notekpr recover`; nothing else in it is readable without the caller's keys. `notekpr-server export-user` writes the same from a database or backup file. |
| `GET /me/audit` | the newest 200 audit entries about the caller (devices, keys, agents, sessions) |
| `PUT /me/session/device {device_id}` | label the current session with its device |
| `GET /devices` | own non-revoked devices |
| `POST /devices {device_id, name, enc_public}` | register a **pending** device; other connections get `user_event: device_pending`. `409` before the account has keys, or while 10 devices are pending |
| `GET /devices/{id}/envelope` | account secret encrypted to that device (only when active; `409` while pending) |
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

Roles: `owner` (one per vault: members, keys, settings, deleting the vault),
`admin` (an editor who also runs the vault's tasks: emptying the trash —
deleting notes and attachments for good — and the activity log; people
only, never agents), `editor` (writes, moves things to the trash) and
`viewer` (reads; comments unless the owner turns it off, §6.1). Single notes
are shared as `editor` or `viewer`. "editor/owner" below includes admins.
Apps keep roles from newer releases they don't know (§11); the server
refuses roles it doesn't know (`400`).

| Endpoint | Role |
|---|---|
| `GET /vaults` | caller's vaults with role, `encrypted_meta`, `key_epoch`, `head_seq`, `viewers_comment` (§6.1), `kind` (`regular`, or `quick_notes`, §5.3; absent on older servers) and `note_limit` (quick notes: how many notes they may hold; `"unlimited"` for every other vault). For the owner also the keys to renew (§5.4): `rotation_pending` (the vault key) and `notes_rotation_pending` (note ids); absent when there are none |
| `POST /vaults {id, encrypted_meta, key_envelope, log_entry?, kind?}` | human; the client picks the id and wraps epoch 1 to itself; `log_entry` starts the membership log (§5.2); `kind: "quick_notes"` makes the caller's quick notes (§5.3; `409` if they have them) |
| `GET/PATCH/DELETE /vaults/{v}` | read: member; update (`encrypted_meta`, `archived`, `viewers_comment`): owner (never quick notes: `400`); delete: owner |
| `GET /vaults/{v}/keys` | member: own envelopes for every epoch granted + full key chain; the notes' document keys (`objects`, `object_chain`), which notes are shared individually (`shared`; absent on servers from before notes had their own keys) and for those where sharing started (`share_from`: note → first shared epoch) |
| `GET /vaults/{v}/members` | member: `MemberInfo {user (with account key), role, has_current_key}` |
| `POST /vaults/{v}/members {user_id, role, key_envelope?, log_entry?}` | owner; never in quick notes (`400`, §5.3). Without an envelope the member is "pending keys" until the owner's client grants them. Role `owner` is refused (`400`), and `admin` for an agent (`400`); an agent can be added only by its owner (`404` otherwise). Membership supersedes single-note shares: adding someone removes their shares of notes in that vault and those notes' keys |
| `PATCH /vaults/{v}/members/{u} {role, log_entry?}` | owner; never to `owner`, nor `admin` for an agent (`400`) |
| `DELETE /vaults/{v}/members/{u}` `RemoveMemberRequest {log_entry?, rotation?}` | owner removes, with `rotation`: the vault's new key for everyone else (`RotateKeysRequest`, checked as `keys/rotate` once the person is gone), stored in the same transaction — without it `400`, and nothing changes if it is refused. The person removed gets `access_revoked`, never the new key's `key_rotated`. Any non-owner can remove themselves (no `rotation`, `400` otherwise; the body is optional); if they had the vault key it is to be renewed (§5.4) |
| `GET /vaults/{v}/log?since=` | member: the membership log, `MemberLogResponse {entries}` (§5.2) |
| `POST /vaults/{v}/log {entry}` | owner: a `checkpoint` (vaults without a log), a `rekey`, a `share` of a note already shared, `require_signed`, or a `remove` of someone who already left (§5.2) |
| `POST /vaults/{v}/keys/grant {envelopes}` | owner fills missing envelopes for the current epoch |
| `POST /vaults/{v}/keys/rotate {new_epoch, chain_link, encrypted_meta, envelopes, objects, log_entry?, without?}` | owner; `new_epoch = current + 1`; envelopes must cover exactly the members that have account keys, except those listed in `without` — left out on purpose because their account key changed and the owner hasn't verified it (they stay members without the current key and get it later through `keys/grant`); `objects` (`ObjectKeyChange {object_id, update}`) moves **every** note with a document key to its next epoch in the same transaction (each a rotation as in §5.1, wrapped under the new vault key), else `409` |
| `GET /vaults/{v}/audit` | owner and admins: the newest 500 membership, key and deletion events (no content) |
| `GET /users/search?q=` | humans matching a display-name prefix or exact e-mail (with `--user-directory exact-email`: exact e-mail only), plus the caller's own agents; at least 2 characters (shorter: no results), at most 10 results |
| `GET /users/{id}` | public profile + account key of a user the caller may interact with; without the e-mail for people connected only through an individually shared note |

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
| `PUT /vaults/{v}/objects/{o}/key` `ObjectKeyUpdate {epoch, vault_envelope, chain_link?, envelopes, encrypted_meta, snapshot?, private?, without?}` | See the three kinds below. `vault_envelope` must be wrapped under the vault's *current* epoch (else `412 stale_key`). A `snapshot` must be under the new key and its `covers_seq` must cover every stored update of the note (else `412 stale_key`). |
| `GET /vaults/{v}/objects/{o}/keys` | member or recipient: `ObjectKeysResponse {object_id, role, current_epoch, envelopes (own), chain, encrypted_meta, member_envelopes?, shared?}`. Recipients: the chain only from `share_from` on; their envelopes carry the owner's grant `signature` (below). Members: the note's key at every epoch wrapped under the vault key (`member_envelopes`), every chain link, and whether it is `shared` — so after a note's `key_rotated` / `shared` event they reload that note's keys alone, not the vault's |
| `PUT /vaults/{v}/objects/{o}/keys/signatures {grants: [{user_id, epoch, signature}]}` | owner: grant signatures for keys given before grants were signed |
| `PUT /vaults/{v}/objects/{o}/meta {encrypted_meta}` | writer: the title recipients see, under the current document key (else `412 stale_key`) |
| `GET /vaults/{v}/objects/{o}/shares` | member: `ObjectShareInfo {user, role, added_at, added_by, has_current_key, key_signed}` |
| `POST /vaults/{v}/objects/{o}/shares {user_id, role, envelope, log_entry?}` | owner; never in quick notes (`400`, §5.3); `role` is `editor` or `viewer`; the envelope wraps the current document key to the person (else `412 stale_key`). Vault members are refused (`409`), and so is a note whose sharing has not started (`409`: give it a fresh key first). Someone it is shared with already is refused too (`409`), unless they lack the current key (left out of a rotation, `without`): then they get it, with this role. |
| `PATCH /vaults/{v}/objects/{o}/shares/{u} {role, log_entry?}` | owner |
| `DELETE …/shares/{u}` `RemoveShareRequest {log_entry?, rotation?}` | owner, with `rotation`: the note's new key for the people it stays shared with (an `ObjectKeyUpdate` rotation, below), stored in the same transaction — without it `400`, and nothing changes if it is refused. The person removed gets `access_revoked`, never the note's `key_rotated`. Recipients may remove the note from their list (no `rotation`, `400` otherwise; the body is optional); its key is then to be renewed (§5.4) |
| `GET /shares` | notes shared with the caller: `SharedNote {vault_id, object_id, role, owner, shared_by, key_epoch, encrypted_meta, added_at}` |
| `GET /vaults/{v}/objects/{o}/changes?since=` | that note's part of the log, with its comments (§6.1) (same shape as `/changes`) |

The kinds of `PUT …/key`:

| Kind | Request | Who |
|---|---|---|
| **Private first key** | `private: true`, `epoch = 0x8000_0001`, no chain link, no envelopes; a covering `snapshot` only if the note has content already (lazy move of an older note) | any writer, agents included |
| **Sharing starts** | `private: false` and either the first key (`0x8000_0001`) or `current + 1` **without** a chain link; a covering `snapshot` is required; no envelopes (people are added afterwards); refused (`409`) while the note has recipients | owner |
| **Rotation** | `epoch = current + 1` with a `chain_link`, and `envelopes` for exactly the current recipients (else `409`), apart from those listed in `without` (their key changed; sharing the note with them again gives them the key) | owner |

**Grant signatures.** Every note key the owner gives someone it is shared
with (`envelopes` of a share, a rotation or a vault rotation) carries
`signature`: Ed25519 by the owner over `statement("notekpr/v1/sig/note-key",
[vault_id, object_id, user_id, epoch:u32be, commitment])`, the commitment
being BLAKE3 (context `notekpr v1 note key commitment`) over
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
only under the **current** document epoch; anything else (an older client
still using the vault key, or a key that was just rotated) is refused as
`stale_key` — in a push, that update alone is listed in `rejected` (§6); a
snapshot gets `412`. Clients reload keys and re-send. Rotating the vault key
renews every note's key in the same request (removed members knew them).

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

A member is `{user_id, role, enc_public, sign_public}` (`role` as in §5;
in `share`, `editor` or `viewer`; a role an app doesn't know is kept as
written and verifies, §11). The signature is
Ed25519 over `statement("notekpr/v1/sig/member-log", [vault_id,
seq:u64be, prev, signer, signed_at:i64be, body])`, where `body` is a
length-prefixed encoding of the fields (never JSON). An entry's hash is
BLAKE3 (derive-key mode, context `notekpr v1 member-log entry`) over its
fields and signature. A key commitment is BLAKE3 (context `notekpr v1
vault key commitment`) over `statement("key", [vault_id, epoch:u32be, key])`.

The server appends the entry that comes with a membership or key change in
the same transaction, after checking that it continues the log, is signed by
the caller with their stored account key, and says exactly what the request
does (`400` otherwise, `409` if the log moved on meanwhile). Once a vault has
a log, changes without an entry are refused with `409` (leaving excepted).
The owner's removal carries two entries, signed one after the other: the
`remove` (`RemoveMemberRequest.log_entry`) and, continuing it, the new key's
`rotate` (in its `rotation`). Clients:

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

### 5.3 Quick notes

Each person may have one vault of the kind `quick_notes`: their quick notes,
which apps show as such, never as a vault (they are flat and keep no trash —
both up to the apps, as the server sees no paths). The server makes sure
they stay the person's own and few:

* Nobody else is ever added: members (people or agents) and single-note
  shares are refused (`400`), and so is starting to share a note's key
  (`PUT …/objects/{o}/key` other than `private`). No comments (`403`); their
  name and settings stay as made (`PATCH /vaults/{v}`: `400`).
* They hold at most `note_limit` notes — the owner's `quick_note_allowance`,
  else the instance's `quick_notes_per_user` (default 20, §9). Notes are the
  objects other than the index and comments that aren't deleted for good;
  the first write of a new one (its own key, `PUT …/objects/{o}/key`, or a
  pushed update) beyond the limit is refused: `403 limit_reached` for the key,
  `quick_notes_full` for an update in a push. Clients keep the note and try
  again once there's room (a note purged, a larger limit, a new connection).
  Lowering the limit takes nothing away.
* They count against no vault limit (`max_vaults`, `vaults_per_user`), and
  the admin API's counts leave them out.

A note leaves by moving to a vault — a client-side copy: the new note (and
the attachments it links to) is encrypted for that vault, and the original
is deleted for good (`DELETE …/objects/{o}`) only after the server
acknowledged the copy — or by being deleted for good.

### 5.4 Keys someone who left still knows

When the owner removes someone, the new keys come in the same request
(§5, §5.1). Everyone else who loses access can't renew a key — only the
owner can — so the server marks the key, and the owner's apps rotate it as
soon as they see the mark:

| What happens | Marked |
|---|---|
| A member who had the vault key leaves | the vault key |
| An agent is deleted | the key of every vault and note it held |
| Someone removes a note shared with them from their list | that note's key |
| A shared note is deleted for good (its shares end) | that note's key, listed once the note is brought back |

The owner sees the marks in `GET /vaults` / `GET /vaults/{v}`:
`rotation_pending` and `notes_rotation_pending`. Their apps look when they
open a vault, on `members_changed`, and on a note's `shares_changed` (which
the server also sends when a marked note is brought back), and rotate:
`POST …/keys/rotate` for the vault key (every note's key moves with it), or
`PUT …/objects/{o}/key` (a rotation) for a note. Any new epoch of that key
clears its mark. Until then, writes go on under the marked key: a removed
person could read them only if the server handed them the ciphertext.

## 6. The encrypted log

Each vault has one monotonically increasing sequence counter. Every accepted
update, every purge and every undone purge consumes one sequence number. The server writes and
publishes while holding a single write lock, so **subscribers always observe a
vault's events in strictly increasing order without gaps.**

| Endpoint | Notes |
|---|---|
| `POST /vaults/{v}/updates {updates: [{object_id, client_update_id, payload, signature?}]}` → `{results, head_seq, rejected?}` | editor/owner. Idempotent per `client_update_id`: a retry returns the original `seq`. Objects are created on first write. An update that can't be stored on its own merits is refused alone and listed in `rejected: [{client_update_id, object_id, code, message}]` with `code` `too_large` (over `max_update_bytes`), `purged` (the object was permanently deleted), `stale_key` (encrypted with an outdated key: reload keys, re-encrypt, send again), `unsigned` (the vault requires signed content, §5.2), `bad_signature` (not signed with the author's account key) or `quick_notes_full` (a new note in quick notes holding as many as they may, §5.3: kept by the client and sent again once there's room); the rest of the batch is stored. Malformed requests, a batch over 500 updates and missing write access still fail the whole push. (Servers before this answered `413` / `409` / `412` for the whole batch; clients handle both.) |
| `GET /vaults/{v}/changes?since=S&limit=N&snapshots=true` | `ChangesResponse {updates, snapshots, purged, restored?, head_seq, has_more, next_since}`. `purged` / `restored`: `{object_id, seq}` of objects deleted for good, and of those whose deletion was undone (below), in `(S, page end]`; apply them in `seq` order. For any object whose latest snapshot covers part of `(S, head]`, the snapshot is returned **instead of** the updates it covers. Request further pages with `since=next_since&snapshots=false`. Every page also holds the snapshots *carrying* changes (below) stored while the head was within it, so each catch-up passes them once. |
| `GET /vaults/{v}/objects/{o}` | `ObjectState`: latest snapshot + updates after it |
| `POST /vaults/{v}/objects/{o}/snapshots {snapshot_id, covers_seq, payload, signature?, carries?}` | editor/owner (`409` unsigned where signatures are required, `400` for a bad signature). `404` if the object doesn't exist yet or was purged (a snapshot never creates an object), `409` for a `snapshot_id` already used, `413` over `max_snapshot_bytes`. Store a compaction (`payload` = encrypted full state). Updates covered by the *previous* snapshot are pruned — one generation is always kept so a bad snapshot is recoverable. One snapshot per person and note per `--snapshot-cooldown-secs` (default 30 s; `429` otherwise). `carries: true`: the state holds changes the log doesn't have — an update too large to push (`too_large`), which a client sends this way instead; the server sends the snapshot to everyone subscribed (`snapshot`) and with catch-ups (above), and its retention keeps the newest such snapshot of each note. |
| `GET /vaults/{v}/objects/{o}/snapshots[/{id}]` | revision history (newest first); default retention 100 per object, never fewer than 2 |
| `DELETE /vaults/{v}/objects/{o}` | owner and admins (editors get `403`: they move things to the trash, in the index): delete for good (emptying the trash), with its comments. Not allowed for `index`. It is gone for everyone at once (`purged`: nothing of it is served, pushes to it are refused as `purged`, the people it was shared with lose it), but the server keeps its data for `--purge-undo-days` (default 30), so it can be undone; repeating the call meanwhile changes nothing (`204`). After that the data is deleted, and the call answers `404`. |
| `POST /vaults/{v}/objects/{o}/restore` | owner and admins: undo deleting it for good while the server keeps it (`404` otherwise). It is back for everyone with its history and comments, though shared with nobody: subscribers get `restored`, catch-ups list it in `restored`. Clients that dropped it fetch its changes again from the start (`GET …/objects/{o}/changes?since=0`, every page) and don't move their `vault_seq` past the undo before they have (§6, client rules). While it was kept, its document key moved on with every rotation of the vault key (§5.1), so no key a removed member knew comes back with it. |
| `GET /vaults/{v}/purged` | owner and admins: what was deleted for good and can still be undone, newest first: `[{id, attachment, purged_at, kept_until}]` (ids only; clients name them from the entries they keep in the index, marked `purged`, until the server let go of the data). |
| `PUT/GET/DELETE /vaults/{v}/attachments/{id}` | raw encrypted bytes (`application/octet-stream`). Upload: editor/owner; immutable: `201` when stored, `200` for a retry (same id and size), `409` for the same id with another size. Download: vault members only, never people a single note is shared with. Delete (for good, emptying the trash): owner and admins; kept for the undo period like notes. |
| `POST /vaults/{v}/attachments/{id}/restore` | owner and admins: undo deleting an attachment for good, while it is kept (`404` otherwise). |

**Client rules**

* Sign every update and snapshot with your account key (§5.2): updates over
  `statement("notekpr/v1/sig/update", [vault_id, object_id, payload])`,
  snapshots over `statement("notekpr/v1/sig/snapshot", [vault_id,
  object_id, covers_seq:i64be, payload])`. `Update` and `Snapshot` carry the
  `signature` (null for content from before signing).
* Encrypt every update with purpose `Update` (snapshots: `Snapshot`) and context `(vault_id, object_id)`, always using the **current** epoch. The server refuses older epochs as `stale_key` (in a push, that update is listed in `rejected`; a snapshot gets `412`) and unknown newer ones with `400`: reload keys, re-encrypt pending changes and send them again.
* Pages of `/changes` are bounded by row count (`limit`, at most 5000) and by size (16 MiB of update payloads); keep requesting while `has_more`.
* Apply updates idempotently (CRDT). Track `vault_seq` = highest sequence received (updates, acks, purges, undone purges, `synced.head_seq`).
* After `restored` (live or in a catch-up), fetch the object's changes from the start and apply them; until every page is in, keep `vault_seq` before the undo's `seq`, so that a reload or reconnect delivers it again and the fetch is retried. Don't snapshot the object meanwhile.
* Deleting for good keeps the index entry, marked `purged` (with `trashed`), while the server keeps the data: it shows only in the trash's list of what can be undone. Undoing removes the mark (the entry is back in the trash). Once an entry's undo period is over and `GET …/purged` no longer lists it, the owner's or an admin's client removes it from the index.
* A snapshot's `covers_seq` must be ≥ 1 and ≤ the client's `vault_seq` at the time it encoded the state, and the client must have applied every update of that object up to there. REST push responses do not advance `vault_seq` (they can overtake other clients' updates); a REST-only client therefore catches up via `/changes` before creating a snapshot.
* Compact when an object has accumulated many updates (clients use ~200) or after an editing session ends; add random jitter.
* An update larger than `max_update_bytes` that can't be split goes out as a snapshot of the object with `carries: true` (if the state fits `max_snapshot_bytes`); once stored, the update is done. Updates after it then reach everyone, as their CRDT dependencies arrive with it.
* Persist unacknowledged updates and retry them with the same `client_update_id` after reconnecting.
* If the server reports a `head_seq` lower than one seen before, warn the user (possible rollback/restore) and re-push local state; CRDT merging restores anything missing.

### 6.1 Comments

A note's comments (docs/comments.md) are the object **`c_<note id>`** (note
ids never contain `_`). It is not a CRDT: each update is one **record**,
appended, never changed or removed; edits, deletions and resolving are new
records. The plaintext of an update is `{"record": b64, "signature": b64}`:

* `record`: JSON `{id, author, created, kind, …, mentions?}` with `kind`
  one of `comment {body, anchor: {start: b64, end: b64, quote}}` (the yrs v1
  sticky indices of the first and last commented characters, and the text as
  it was, ≤ 300 characters), `reply {thread, body}`, `edit {target, body}`,
  `delete {target}`, `resolve {thread}`, `reopen {thread}`; bodies ≤ 10 000
  characters. `mentions` (comment, reply and edit only; absent when empty):
  the user ids of the people the body mentions (`@Name`), at most 50, each
  once — an edit's replace the comment's. Clients notice mentions of their
  own user in the records they sync; the server can't see them;
* `signature`: Ed25519 by the author's account key over
  `statement("notekpr/v1/sig/comment", [vault_id, note_id, record])` —
  over the content, so a record still verifies after it is encrypted again
  under another key.

It is encrypted like the note's content: the note's document key (§5.1), else
the vault key; the server checks the epoch against the note's key
(`stale_key` as usual). Clients fold the records in log order: an `edit`
counts only from the target's author, a `delete` from its author or a vault
owner, and only records whose signature verifies with a key they know for
the author (members: the membership log, which lists every member's keys and
the people notes were shared with; others: pinned keys) change anything.

The server lets **anyone who can read the note** write its comments object —
vault members of any role and people the note is shared with — except
viewers while the vault's `viewers_comment` is off (`403`), and only for a
live note (`404`). It refuses snapshots of it (`400`), records over 64 KiB
(`too_large`), deleting it on its own (`400`) and sharing it on its own
(`400`); purging the note purges its comments. Reads follow the note:
`/objects/{note}/changes` and per-note subscriptions include `c_<note>`, and
for people the note is shared with only records under a key from
`share_from` on. So when a note is shared, its earlier comments stay the
members'; the sharer's client may encrypt them again under the note's new
key (they keep their authors' signatures), which `share_from` in
`VaultKeysResponse` lets it tell apart.

## 7. WebSocket `/api/v1/ws`

JSON text frames, tagged with `"type"`.

Client → server: `auth {token, app?}` (`app` as in the `Notekpr-App`
header, which browsers can't set on a WebSocket; must be first, within
10 s; a session the server doesn't know gets `error {code: "unauthorized"}` and the connection
closes — the only way a browser learns why, so clients sign out on it),
`subscribe {vault_id, since?, object_id?}`, `unsubscribe {vault_id, object_id?}`,
`push {vault_id, updates}`, `awareness {vault_id, object_id, payload}`,
`ping {nonce}`.

Server → client: `ready {user_id, session_id, conn_id}`,
`mode {read_only?}` (right after `ready`, and whenever it changes: `manual` or `storage_full` while the server refuses changes, absent when it accepts them),
`changes {vault_id, changes}` (catch-up pages after `subscribe` with `since`, and after falling behind: below),
`synced {vault_id, head_seq, object_id?}`, `update {vault_id, update}`,
`ack {vault_id, results, rejected?}` (your own pushes, delivered in log order; `rejected` as for the REST push),
`snapshot {vault_id, snapshot}` (a snapshot stored with `carries: true`, §6),
`snapshot_created {vault_id, object_id, snapshot_id, covers_seq}` (every snapshot stored),
`purged {vault_id, object_id, seq}`, `restored {vault_id, object_id, seq}` (deleting it for good was undone, §6),
`awareness {vault_id, object_id, from_user, from_conn, payload}` (empty payload = that connection left),
`vault_event {vault_id, event}` (`members_changed`, `key_rotated`, `meta_changed`, `deleted`, `access_revoked`, `role_changed`),
`object_event {vault_id, object_id, event}` (`shared`, `key_rotated`, `meta_changed`, `shares_changed`, `access_revoked`, `deleted`),
`user_event {event}` (`device_pending`, `devices_changed`, `vaults_changed`, `shares_changed`, `session_revoked`),
`error {code, message, vault_id?}`, `pong {nonce}`.

Only `update` and `awareness` skip the connection they came from (your own
pushes come back as `ack`); `snapshot` and `snapshot_created` reach every
subscriber, the author's own connection included. A subscriber that falls too
far behind live events gets the gap replayed from the log as `changes`, then
`synced` again; awareness, `snapshot_created`, `vault_event` and
`object_event` messages missed in the gap are not replayed.

While the server is read-only (`mode` with `read_only`), pushes get `error
{code: "read_only", vault_id}`; clients don't push then, keep their edits
(stored as unsent changes), and send everything as soon as a `mode` without
`read_only` arrives — no polling or backoff. A push refused just as read-only
began may arrive before that `mode`: clients send it again with the next
`mode` without `read_only`, or after a while if none comes. Reading,
subscribing and presence keep working.

When the server shuts down to be started again (an update), it closes every
connection with code **1012** ("service restart", `CLOSE_RESTARTING`) before
it exits. Clients then reconnect soon — about every second, jittered, for up
to a minute — rather than backing off as after a failure, and may show the
connection as briefly *connecting* rather than *offline*. Any other close, or
a connection that just drops, is a failure: reconnect with backoff.

A client must subscribe to a vault before pushing to it over the socket.
Awareness payloads are encrypted by clients (purpose `Awareness`); the server
only relays them. Limits per connection:

* pushes and (un)subscriptions share a budget (20 per second, bursts of
  400); beyond it the server answers `error {code: "rate_limited"}` and drops
  the message, so clients keep the changes pending and retry;
* at most 200 subscriptions; another one gets `error {code: "bad_request"}`
  ("too many subscriptions");
* awareness: 30 per second, bursts of 60, payloads ≤ 16 KiB, `object_id` ≤ 64
  characters, and only where the connection is subscribed; anything else is
  dropped without an answer;
* messages and frames up to 2 × `max_update_bytes` + 1 MiB (a larger one
  ends the connection); binary frames get an `error`.

The server pings every 30 s and checks the session again every 60 s; once it
is no longer valid, the server sends `user_event {event: "session_revoked"}`
and closes the connection.

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
| `GET /agents`, `POST /agents {name}` | owner's agents; creation returns a client id + secret **once**; at most `agents_per_user` per owner (default 50) and `max_agents` on the server (§9; `403 limit_reached` beyond) |
| `DELETE /agents/{id}` | deletes the agent, its memberships and sessions |
| `POST /agents/{id}/credentials`, `DELETE /agents/{id}/credentials/{client_id}` | rotate / revoke secrets (revocation ends sessions) |

An agent creates its own account key (`PUT /me/account-key` with only `key`).
Its owner shares vaults with it like with any person, choosing viewer or editor.
Agents cannot create vaults, manage members, devices or agents.

## 9. Administration

Administrators are people with the admin role (`--admin`, the first person
to sign in, or made one by another administrator), with their session, and
**admin clients**: scripts and services such as a billing system, each with
a name, scopes and one or more bearer tokens (`Authorization: Bearer
notekpr_a_…`). An admin client isn't an account: it has no session, keys or
memberships, and only the routes below accept its tokens — every other route
(vaults, notes, keys, people, the WebSocket) answers `401`. Neither kind of
administrator can read anything in a vault.

Scopes (`AdminScope`): `read` (every admin client has it), `settings`,
`accounts` (disable and enable accounts, their allowances), `admins` (the
admin role), `clients` (admin clients). A missing scope gets `403`. People
with the admin role have every scope. On an admin-less instance
(`--adminless`) nobody signing in is an administrator (`is_admin` is
`false` everywhere, every route below answers people `403`, and the admin
role can't be given: `400`): only admin clients administer it.

| Endpoint | Notes |
|---|---|
| `GET /admin/users?email=&identity=` | `read`: accounts with kind, e-mail, owner (agents), admin and disabled flags, last sign-in, vault (member of; quick notes not counted) and device counts, `owned_vault_count`, `agent_count`, `quick_note_count`, `vault_allowance`, `agent_allowance`, `quick_note_allowance` — no content. `email` (any case) or `identity` (`issuer\|subject`, or `dev:<name>` in development mode) finds one person |
| `PATCH /admin/users/{id} {disabled?, is_admin?, vault_allowance?, agent_allowance?, quick_note_allowance?}` | `accounts` (`admins` for `is_admin`): disable (ends the person's sessions and their agents') or enable (counts against `max_users`), (un)make administrator, set a person's own limits: `"default"` (the instance's), `"unlimited"` or a number. Agents have no allowance (`400`). A new quick-note allowance is announced to the person's apps (`vaults_changed`) |
| `GET /admin/stats` | `read`: the server's `version` (as in `GET /info`), counts of users, agents, vaults, objects, updates, snapshots and attachments, and the database's size (`storage_bytes`: pages in use) |
| `GET /admin/settings` | `read`: `{settings, read_only?, storage_bytes}`: the instance settings below, whether (and why) the server refuses changes now, and the database's size |
| `PATCH /admin/settings {…}` | `settings`: change any of the settings; the rest stay. Answers like `GET`. Takes effect at once (no restart) and is kept in the database |
| `GET /admin/clients` | the admin clients: `id`, `name`, `scopes`, `created_at`, `managed`, `tokens` (`id`, `hint` — the last characters —, `created_at`, `last_used_at`; never the tokens) |
| `POST /admin/clients {name, scopes}` | `clients`: a new client (name: 1–64 of `a-z 0-9 . _ -`, unique, else `409`) and its first token: `CreatedAdminToken {client, token}`, the only time the token is shown |
| `PATCH /admin/clients/{id} {scopes?}` | `clients`: change its scopes |
| `DELETE /admin/clients/{id}` | `clients`: delete it and its tokens |
| `POST /admin/clients/{id}/tokens` | `clients`: another token (`CreatedAdminToken`) — rotate by adding one, then revoking the old |
| `DELETE /admin/clients/{id}/tokens/{token_id}` | `clients`: revoke a token |

Managing an admin client also takes every
scope it has, and no one gives scopes they don't have themselves, so
delegating never grants more. Admin clients are also made from the server's
command line (`notekpr-server admin-client create` / `ensure`,
[operations.md](operations.md#admin-clients-scripts-and-services));
`managed` ones are kept by `ensure`, which puts back their scopes and tokens
whenever it runs.

**Instance settings** (`InstanceSettings`). A limit is a number or
`"unlimited"` (the default), and `0` means none at all:

| Setting | Meaning |
|---|---|
| `max_users` | accounts of people that aren't disabled; a new person's first sign-in (or enabling an account) beyond it is refused |
| `max_agents` | agents on the server |
| `max_vaults` | vaults on the server (quick notes don't count) |
| `vaults_per_user` | vaults a person may own unless they have their own `vault_allowance`; vaults shared with them and their quick notes don't count |
| `agents_per_user` | agents a person may have unless they have their own `agent_allowance` (default 50) |
| `quick_notes_per_user` | quick notes a person may keep unless they have their own `quick_note_allowance` (default 20, §5.3); a change is announced to everyone's apps (`vaults_changed`) |
| `read_only` | `true`: the server refuses changes (below) |
| `max_storage_mb` | at this database size (MiB, pages in use) the server turns read-only by itself (`storage_full`), until it is below it again — e.g. the limit was raised. Measured every 10 s and after every settings change |

Limits are checked when something new would be created, in the transaction
that creates it; lowering one takes nothing away.

**Read-only**: everything that changes something is refused with `503
read_only` — pushes (REST and WebSocket), snapshots, attachments, keys,
vaults, members, shares, agents. What keeps working: reading and syncing
down, signing in and out (but no new accounts), devices (register, approve,
rename, remove), revoking sessions and agents, account key and recovery
setup, preferences, and administration (which switches it off). Manual
read-only (`read_only: true`) and `storage_full` are separate: lifting one
doesn't lift the other; `GET /info` and `mode` report `manual` first.

## 10. Operations endpoints

`/healthz` (liveness), `/readyz` (database reachable, else `503`), `/metrics`
(Prometheus; optional bearer token) live at the root, not under `/api/v1`.
`/metrics` answers `404` when metrics are turned off and `401` without the
right token. All responses carry a strict CSP and security headers.

## 11. Older apps

Installed apps keep running for months, so servers and newer apps send them
values of fixed lists they don't know yet. Apps keep such a value and give
it the least it could mean, rather than fail on the message that carries it:

* **Roles** (§5): kept as written — the membership log signs a role's name
  (its bytes), so entries naming one still verify (§5.2) — and allowed
  nothing an app would have to allow: not writing, the vault's tasks or
  managing it (as `viewer`; the server enforces what the role really may).
  Content signed by someone whose roles were all unknown is refused, as a
  viewer's would be.
* **Read-only reasons** (`mode`, `GET /info`, §9): an unknown one is
  read-only all the same ("the server is read-only for now").
* **Events** (`vault_event`, `object_event`, `user_event`, §7): an unknown
  kind makes an app reload what it could be about — the vault (its
  information, keys and membership log), the note's keys, or the person's
  lists (vaults, shared notes, devices).
* **WebSocket messages** of a `type` an app doesn't know are skipped.
* **Items in the vault index** (`kind`: `note`, `attachment`, `folder`): an
  unknown kind isn't shown, but its folder holds it — it moves when the
  folder is renamed or moved and goes to the trash with it (in the CRDT,
  nothing of it is lost). A folder holding one isn't moved to another vault
  (the app couldn't copy it): the app says to update.
* **Comment records** (§6.1) of an unknown `kind`: kept in the local copy as
  signed, so the app reads them once it knows them, and change nothing in
  the threads meanwhile.
* **Membership log entries** (§5.2) of an unknown `kind` can't be verified
  (what is signed depends on their fields), so the log stops being
  trustworthy there: the app doesn't open the vault and says to update —
  plainly, not as a security warning — and keeps its local copy. Servers
  refuse kinds they don't know (`400`), and serve stored entries as written
  (`GET /vaults/{v}/log`), so a server taken back to an older release
  passes on what a newer one stored.
