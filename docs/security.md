# Security model and threat analysis

## Goals

1. Only members of a vault can read its content — not the server operator,
   not the identity provider, not other users of the same server. People a
   single note is shared with can read that note and nothing else.
2. Only editors and owners can change a vault; changes are attributable.
3. Losing a device must not mean losing notes; losing *all* devices is
   recoverable with the recovery key, and only with it.
4. No edit is silently lost because of concurrency, offline work or a
   misbehaving client.

## What is protected, and from whom

| Asset | Protected by | Server can see |
|---|---|---|
| Note text, titles, file and folder names, tags, links | XChaCha20-Poly1305 with per-vault keys, client-side | no |
| Attachments (bytes, names, types) | same | size only |
| Vault names/descriptions | same (`encrypted_meta`) | no |
| Revision history | encrypted snapshots | count, size, time, author |
| Presence (which note, cursor) | encrypted awareness messages | that a user is connected to a vault |
| Favorites, recent notes, key pins | account-key encryption | no |
| Vault keys | HPKE to each member's account key | no |
| Document keys of individually shared notes | wrapped under the vault key for members, HPKE to each recipient | no |
| Account secrets | HPKE to each device, and to the recovery key | no |
| Identities, memberships, roles, devices, sessions | server-enforced authorization | yes (needed to operate) |

**Metadata the server necessarily learns:** user accounts (display name,
e-mail from the IdP, OIDC issuer and subject), which users share which vault and
with what role, how many objects a vault contains, sizes and timestamps of
ciphertexts, which user and device wrote each update, connection times and IP
addresses. This is the minimum needed to authenticate, authorize, route and
store data; it is documented here so operators and users can judge it.

## Cryptography

All primitives come from maintained, widely used Rust crates (RustCrypto
`chacha20poly1305`, `hkdf`, `sha2`; `hpke`; `ed25519-dalek`; `blake3`). No custom
primitives. Formats:

* **Content**: `version ‖ epoch ‖ 24-byte random nonce ‖ ciphertext ‖ tag`.
  Subkey = HKDF-SHA256(vault key of that epoch, purpose label). AAD binds the
  purpose, header, vault id and object id, so ciphertexts cannot be swapped
  between objects, vaults, purposes (e.g. replaying a snapshot as an update) or
  epochs.
* **Key wrapping**: HPKE base mode (RFC 9180) DHKEM(X25519, HKDF-SHA256),
  HKDF-SHA256, ChaCha20-Poly1305. `info` separates use cases; AAD binds vault id,
  epoch and recipient (or user and device id).
* **Signatures**: Ed25519 with strict verification over length-prefixed,
  domain-separated statements (account key self-signature, device approval,
  recovery-key rotation).
* **Recovery key**: 256 random bits plus a 16-bit checksum (typo detection),
  base32. No password-derived keys exist anywhere, so there is nothing to
  brute-force offline.
* **Fingerprints**: 128-bit BLAKE3 hash of an account's public keys, shown for
  out-of-band comparison; device verification codes are 60 bits.
* **Transport**: TLS terminates at the reverse proxy (HSTS is set when the
  public URL is https). All application-level protections above hold even
  without TLS, but sessions and metadata need it.

## Threats

### Compromised or malicious server (or its backups)

* **Reads content?** No — it never has a key. Backups contain only ciphertext.
* **Substitutes a public key to receive a vault key?** Mitigated: clients verify
  self-signatures, pin keys on first use and refuse (with a clear warning) if a
  pinned key changes; fingerprints can be compared out of band. Residual risk:
  a server that substitutes keys *before* the first share between two people is
  only detected by comparing fingerprints.
* **Enrolls a rogue device?** Requires an account signature over the device key,
  which only a holder of the account secret can produce, and the user compares
  a verification code shown on both devices.
* **Tampers with, reorders or replays updates?** AEAD rejects tampering;
  object/vault/epoch binding rejects moved ciphertexts; CRDT updates are
  idempotent so replays are harmless.
* **Withholds updates or rolls back state?** Clients track the highest sequence
  ever seen and warn on regression, then re-push their state (CRDT merge
  restores lost content). A server can still delay or drop *future* deliveries
  (availability is not protected).
* **Injects content as a member?** It has no vault key, so it cannot produce
  valid ciphertext.
* **Learns metadata** (see above). Mitigation: minimal metadata by design.

### Stolen credentials

* **Stolen OIDC session or IdP account**: gives a *session* but no keys. The
  attacker can register a pending device, which only becomes useful if the user
  approves it after comparing codes. They could also delete or corrupt data the
  account can write; revision history and the one-generation retention of raw
  updates allow recovery, and the owner can remove the account from vaults.
* **Stolen session token** (e.g. from a compromised browser): revocable from
  Settings → Sessions or by removing the device; tokens are stored hashed on the
  server.
* **Stolen recovery key** alone: useless without signing in as the user; with
  both, the attacker can enroll a device. Users can rotate the recovery key.
* **Stolen agent secret**: revoke it in Settings → AI agents; tokens live one
  hour; the agent only ever had the vaults explicitly shared with it.

### Malicious collaborators

* **Viewers** cannot write: the server rejects their pushes, snapshots,
  attachment uploads and purges. (They do hold the vault key, so in collusion
  with a malicious server they could forge content; signing updates with the
  author's key is planned to close this.)
* **Editors** can change or trash content but not manage members or keys.
  Their writes are attributed (author and device are recorded per update) and
  history allows restoring earlier versions. Permanent deletes are recorded in
  the vault audit log. Because the server can't check what a snapshot
  contains, a malicious editor can still damage the *server's* copy of history
  (e.g. snapshots that drop content; a per-person cooldown keeps this slow).
  Members' local replicas, Markdown exports and server backups are the
  recovery path; owner-recoverable deletes are planned.
* **Removed members** lose server access immediately and the vault key is
  rotated, so content written afterwards is unreadable to them even if they
  obtain ciphertext. Data they downloaded before removal cannot be revoked —
  the UI says so. The document keys of individually shared notes are rotated
  too, since members could read them.
* **Recipients of a single note** hold only that note's document key. The
  server gives them only that note's content under that key — not the vault
  index (paths, other titles), other notes, attachments, the member list, the
  vault key, or the note's history from before it was shared (stored under the
  vault key). They learn the owner's identity and see the presence of people
  working on that note. They cannot share it further or change its key. When
  one is removed, the owner's client rotates the document key; as with vault
  members, what they already downloaded stays with them. Images embedded from
  vault attachments are not shown to them (the attachments aren't shared).

### Compromised device

A compromised, unlocked device exposes everything that device can decrypt
(this is inherent to E2EE). Mitigations: remove the device (revokes its
sessions and envelope), rotate keys of shared vaults, rotate the recovery key.

### Local storage

| Client | At rest |
|---|---|
| Browser | Device secret in `localStorage`, unsent updates (already encrypted with vault keys) in `localStorage`, and a local copy of each vault in IndexedDB, encrypted with the vault's (or shared note's) keys like the server's copy. No plaintext notes are persisted; content is decrypted into memory. The local copy is deleted when access to a vault ends and by "Forget this browser". Protection of the browser profile relies on the OS account and full-disk encryption. |
| Desktop, default | Encrypted local replica (vault-key encrypted CRDT state, sealed with a local key file `0600`). The key file sits in the same profile directory, so this protects against other OS users and stray copies of single files, but not against a copy of the whole directory (a backup, a synced folder) or malware running as the same user; use full-disk encryption. Moving the key to the OS keyring is planned. |
| Desktop, plaintext mirror (explicit opt-in) | Ordinary Markdown files readable by any program running as the user — the point of the mode. It must be enabled explicitly and the CLI explains the implications. |
| Agent gateway host | Holds the agent's keys and plaintext of the vaults shared with the agent. Treat it like any server with secrets; the gateway listens on localhost by default and requires a bearer token. |

### Untrusted sync operations

Every operation a client receives is authenticated (AEAD with its context) and
merged by the CRDT. A malformed or undecryptable update is skipped and
reported, never applied partially. Duplicate deliveries are no-ops.

### Loss of encryption keys

* One lost device: approve a new device from another one.
* All devices lost: recovery key.
* All devices *and* the recovery key lost: the notes cannot be recovered — by
  design there is no server-side master key. Export your vaults regularly.
* **No lock-in**: with the recovery key (or a device) and an encrypted export
  (`coolpsonotes export-encrypted`, `GET /me/export`, or
  `coolpsonotes-server export-user` from any server backup), everything a
  person could read decrypts offline into Markdown files with
  `coolpsonotes recover`, without the server ([recovery.md](recovery.md)).

### Web application threats

* **XSS**: the reading view renders Markdown with raw HTML disabled (shown as
  text), only `http(s)`/`mailto`/in-vault link targets, escaped attributes; the
  editor inserts only escaped text. A strict Content-Security-Policy
  (`script-src 'self' 'wasm-unsafe-eval'`, no inline scripts, `frame-ancestors
  'none'`, `object-src 'none'`) is a second layer. External images are not
  loaded (also prevents tracking pixels).
* **CSRF**: the API uses bearer tokens, not cookies. The OIDC login itself is
  bound to the browser that started it (an HttpOnly, SameSite cookie checked at
  the callback), so a login link prepared by someone else can't sign a victim
  into the attacker's account.
* **Open redirects**: login `return_to` must be a same-origin path.
* **Clickjacking**: `X-Frame-Options: DENY`, `frame-ancestors 'none'`.
* **Supply chain**: the browser client contains no third-party JavaScript; all
  dependencies are Rust crates pinned in `Cargo.lock`. Run `cargo audit` /
  `cargo deny` in CI (see development.md).

### Server hardening

* OIDC: authorization code with PKCE, `state` and `nonce`, ID token signature
  verified against the provider's JWKS (refreshed on key rotation), issuer and
  audience checked by `openidconnect`; HTTP redirects disabled and response
  sizes capped for provider calls; the provider's certificate is checked
  against the system trust store (plus an optional CA file). Only e-mail
  addresses the provider marks verified are used to find people.
* Identities are keyed by `iss`+`sub`, never e-mail. Authentication grants no
  vault access.
* Authorization is checked on every request and every WebSocket push (under the
  write lock, so a push can't race a removal); non-members get `404` so vault
  ids cannot be probed.
* Content must be encrypted with the current key: after a key rotation the
  server refuses updates and snapshots under older epochs (`stale_key`), so
  outdated clients can't keep writing content a removed member could read.
* Rate limits per client IP on the API and (stricter) on login, token and
  device-code endpoints; per-connection limits on presence messages and on
  WebSocket pushes and subscriptions; request size limits per route; catch-up
  pages capped in rows and bytes; a per-person cooldown on snapshots.
* Logs contain method, a redacted path and status — never tokens, query strings,
  bodies or names. No telemetry.
* Development mode (`--dev-mode`) allows password-less login and is announced in
  logs and on every page. The server refuses to start in dev mode on a
  non-loopback address, with an https public URL or with OIDC configured unless
  explicitly overridden; without dev mode, sessions of dev-mode accounts are
  ended and those accounts never become the first administrator.

## Residual risks and planned hardening

1. First-contact key substitution by a malicious server (mitigated only by
   fingerprint comparison).
2. Update authenticity among members with a colluding server (planned: signed
   updates and a signed membership log).
3. Metadata visible to the server (sizes, timing, social graph).
4. Browser key storage relies on the browser profile's security.
5. Post-quantum: key wrapping uses X25519 today; the format is versioned for a
   move to hybrid ML-KEM (X-Wing).
