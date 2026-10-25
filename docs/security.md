# Security model and threat analysis

## Goals

1. Only members of a vault can read its content — not the server operator,
   not the identity provider, not other users of the same server. People a
   single note is shared with can read that note and nothing else.
2. Only the people who may write a vault (owner, admins, editors) can change
   it, and only its owner and admins delete anything for good; changes are
   attributable.
3. Losing a device must not mean losing notes; losing *all* devices is
   recoverable with the recovery key, and only with it.
4. No edit is silently lost because of concurrency, offline work or a
   misbehaving client.

## What is protected, and from whom

| Asset | Protected by | Server can see |
|---|---|---|
| Note text, titles, file and folder names, icons and order, tags, links | XChaCha20-Poly1305 with per-vault keys, client-side | no |
| Attachments (bytes, names, types) | same | size, uploader and upload time |
| Vault names, descriptions, icons and pictures | same (`encrypted_meta`) | no; the size of `encrypted_meta` roughly shows whether a vault has a picture |
| Revision history | encrypted snapshots | count, size, time, author |
| Comments (text, quotes, what they are about, replies, whom they @mention) | encrypted with the note's key; each signed by its author | that a note has comments, and the number, sizes, times and authors of the records; each vault's *viewers can comment* setting. Not who is mentioned: people's own devices find their mentions |
| Presence (which note, cursor) | encrypted awareness messages | that a user is connected to a vault; on individually shared notes also which of them someone has open, and when (clients announce themselves on the note too, under its id) |
| Favorites, recent notes, key pins, synced settings (note widths, which mentions were read) | account-key encryption | no |
| Quick notes (text, names, attachments) | a vault of their own, encrypted like any other | which vault holds someone's quick notes, how many notes are in it, and — from a new note in a vault at about the time one leaves the quick notes — roughly when one moved where |
| Vault keys | HPKE to each member's account key | no |
| Document keys (every note has its own) | wrapped under the vault key for members, HPKE to each person a note is shared with | no; it learns which notes are shared, with whom, and when sharing started |
| Account secrets | HPKE to each device, and to the recovery key | no |
| Identities, memberships, roles, devices, sessions | server-enforced authorization | yes (needed to operate) |

**Metadata the server necessarily learns:** user accounts (display name,
e-mail from the IdP, OIDC issuer and subject), which users share which vault and
with what role, how many objects a vault contains, sizes and timestamps of
ciphertexts, which user and device wrote each update, and when people use it.
This is the minimum needed to authenticate, authorize, route and store data;
it is documented here so operators and users can judge it. It also sees each
connection's IP address, as any server does, but neither stores nor logs it
(below).

### Where the metadata is, and who can see it

"The server sees" means: whoever can read the server's database file
(`notekpr.db`) or a backup of it — the operator, a hosting provider, anyone
with the disk. It is an ordinary SQLite file, readable with any SQLite tool.
Nothing in the app or the logs shows it:

* **Administrators** (Settings → Administration, and admin clients over
  `/admin/*`) see each account's name, e-mail, kind (person or agent),
  role, whether it is disabled, when it was created and last signed in, how
  many vaults it is in and owns, how many devices, agents and quick notes it
  has, and its own limits; plus totals for the instance (accounts, vaults, objects,
  updates, storage) and the instance settings. Not who is in which vault,
  not who shares with whom, not activity — and never content.
* **Logs** have one line per request — method, path with ids replaced by
  `:id`, status, duration — plus start-up and shut-down events. No user or
  vault ids, IP addresses, tokens, query strings, bodies or names.

What the database holds, besides ciphertext and public keys:

| Table | What it tells |
|---|---|
| `users`, `identities` | accounts: display name, e-mail, OIDC issuer and subject, administrator, disabled, created and last sign-in; an agent's owner |
| `devices`, `sessions`, `agent_credentials` | each account's devices and their names (the browser and system, or `notekpr CLI on <host name>`), sessions, and when each was created, last used (to the minute; only the latest time is kept), revoked |
| `vaults`, `vault_members`, `member_log` | who owns each vault, who is in it with which role, who added them and when; which vault is someone's quick notes; the membership log's entries (user ids, roles, public keys, key commitments, times) |
| `objects`, `updates`, `snapshots` | how many notes and other objects a vault has, when each was created and changed, and for every change (notes, the file tree, comments): who wrote it, from which device, when, and its size. Comment threads are objects named after their note (`c_<note id>`), so it also tells which notes have comments |
| `attachments` | each attachment's size, who uploaded it, when |
| `object_shares`, `object_keys` | which notes are shared individually, with whom, with which role, by whom and since when |
| `user_prefs` | when someone's encrypted preferences last changed, and their size |
| `instance_settings` | the instance's limits and read-only switch, and when they last changed |
| `audit_log` | account, device, membership, sharing and key events: who did what to whom, when (ids, roles and device names; never names of notes or vaults) |

Kept only in memory, never written: IP addresses (used for rate limits), who
is connected right now, and presence (which vault — and, for individually
shared notes, which note — someone has open).

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
* **Adds a member on its own, or changes someone's role?** Every membership
  and key change is an entry in the vault's owner-signed, hash-chained
  membership log (protocol.md §5.2). The owner's client gives vault keys only
  to members the verified log lists with that role and key, members see
  anyone the log doesn't list flagged in the member list, and rotation stops
  with a security warning if the server lists an unknown member with keys.
* **Shares someone's quick notes?** The server refuses members and note
  shares there, and as with every vault it can't add anyone on its own:
  their owner's apps never sign anyone into them. Nor can it make a vault
  someone else owns or shares pass for one's quick notes: apps take for them
  only a vault of that kind the person owns alone.
* **Hands a member a vault key it made itself** (so it could read what they
  write next)? The owner commits to every epoch's key in the log; clients
  check each key they receive against the commitment and refuse a vault
  whose key doesn't match or was never announced. People a single note is
  shared with use only a note key the owner signed giving them (checked
  against the owner's pinned key).
* **Rolls back or rewrites the membership log?** Clients remember the last
  entry they verified; a log that no longer contains it is a security error.
  Vaults from before the log are trusted as their owner's client saw them
  when it signed the first checkpoint.
* **Enrolls a rogue device?** Requires an account signature over the device key,
  which only a holder of the account secret can produce, and the user compares
  a verification code shown on both devices.
* **Gives a new device its own account secret?** Anyone can encrypt to a
  device's public key, so the server could send a pending device a secret it
  made up, and then read what that device writes. The verification code also
  covers the account key: the new device computes it from the account key it
  expects (and pins it), the approving device from the real one, and the new
  device refuses a secret whose public key differs from the pinned one —
  also on every later unlock.
* **Tampers with, reorders or replays updates?** AEAD rejects tampering;
  object/vault/epoch binding rejects moved ciphertexts; CRDT updates are
  idempotent so replays are harmless.
* **Replays an older recovery update** (for a recovery key since replaced
  because it leaked)? The signed update covers a version that the server
  only lets go up; once a versioned update is stored, no older or
  unversioned one is accepted.
* **Hands out older preferences** (say, from before someone's key was
  pinned, so a substituted key would be trusted on "first" use)? Preferences
  carry a counter inside their encryption, and each browser keeps its own
  sealed copy; an older copy from the server is not used (a browser that
  never saw the newer one can't tell).
* **Withholds updates or rolls back state?** Clients track the highest sequence
  ever seen and warn on regression, then re-push their state (CRDT merge
  restores lost content). A server can still delay or drop *future* deliveries
  (availability is not protected).
* **Injects content as a member?** It has no vault key, so it cannot produce
  valid ciphertext. Nor can it help someone who has the key but may not
  write (a viewer, or someone removed): every update and snapshot is signed
  by its author, and clients accept content only if the vault's log lists
  that author with that key as someone who could write it. Content by an
  author a client's copy of the log doesn't know yet waits until the log is
  reloaded; it is never applied unchecked.
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
* **Stolen admin client token** (`notekpr_a_…`): gives what that client's
  scopes allow — reading accounts and their metadata above, and with more
  scopes changing limits and read-only mode, disabling accounts, the admin
  role, admin clients — and nothing in any vault. An admin client is no
  account: no route but `/admin/*` accepts its tokens, it has no keys or
  memberships, and it can't sign in to the web app or the WebSocket. Revoke
  the token (Settings → Administration → Admin clients, the API, or
  `notekpr-server admin-client`) without a restart; give each consumer its
  own client with only the scopes it needs, so one leak is one revocation.
  Tokens are stored as hashes; a client with the `clients` scope can only
  hand out scopes it has.

### Malicious collaborators

* **Viewers** cannot write notes: the server rejects their pushes (but for
  comments), snapshots, attachment uploads and deletes. They do hold the vault
  key, but every update and snapshot is signed by its author and clients
  accept only authors the log lists as writers, so a viewer can't forge
  content even with the server's help (for what remains, see
  [residual risks](#residual-risks-and-planned-hardening), 2).
* **Comments** (by anyone who can read a note, unless the owner turns it off
  for viewers) are records that are only ever added. Each is signed by its
  author over its content, and clients count an edit only from the comment's
  author and a deletion from its author or a vault owner, so nobody can
  change or remove someone else's comment, or comment in someone else's name
  — not even with the server's help. A comment by someone whose key a reader
  doesn't know (yet) is shown marked as unverified and can't change anything.
  The server could leave out records it was sent, as it could any update.
  A comment can mention only people the writer's client offers (the vault's
  members; on a note shared with someone, its owner and its commenters);
  a crafted one naming anyone else notifies nobody who can't already read
  it, and mentions are never looked for in notes shared with someone.
* **Editors** can change content and move it to the trash, but not delete
  anything for good — the server refuses them emptying the trash, purging a
  note and deleting an attachment — nor manage members or keys. Their writes
  are attributed (author and device are recorded per update) and history
  allows restoring earlier versions. Because the server can't check what a
  snapshot contains, a malicious editor can still damage the *server's* copy
  of history (e.g. snapshots that drop content; a per-person cooldown keeps
  this slow).
  Members' local replicas, Markdown exports and server backups are the
  recovery path; owner-recoverable deletes are planned.
* **Admins** are editors the owner trusts with the vault's tasks: emptying
  the trash (permanent deletes, recorded in the vault's activity log) and
  reading that log. They don't manage members, keys or the vault's settings.
  Their role is in the owner-signed membership log like every other, so a
  server can't present someone as an admin the owner didn't make — though,
  holding the data, it could delete it without anyone's help. Agents can't
  be admins.
* **Removed members** lose server access immediately and the vault key is
  rotated, so content written afterwards is unreadable to them even if they
  obtain ciphertext. Data they downloaded before removal cannot be revoked —
  the UI says so. Every note's document key is renewed in the same request
  (members could unwrap them all).
* **Recipients of a single note** hold only that note's document keys from
  the moment sharing started. Sharing gives the note a fresh key that has no
  chain link to its earlier ones, so even with all the ciphertext a recipient
  cannot decrypt the note's history from before it was shared; the server
  also hands out only content from that epoch on (`share_from`). Nor do they
  get the vault index (paths, other titles), other notes, attachments, the
  member list or the vault key. They learn the owner's identity and see the presence of people
  working on that note. Of its comments they get those written since it was
  shared, and earlier ones only if the sharer chose to include them (then
  everyone the note is shared with gets them). They cannot share it further or change its key. When
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
| Browser | Device secret in `localStorage`, unsent updates (already encrypted with vault or note keys) and new notes' keys not registered yet (wrapped under the vault key) in `localStorage`, and a local copy of each vault in IndexedDB, encrypted with the vault's (or shared note's) keys like the server's copy. No plaintext notes are persisted; content is decrypted into memory. The local copy is deleted when access to a vault ends and by "Forget this browser". Protection of the browser profile relies on the OS account and full-disk encryption. |
| Browser, "Keep this browser usable offline" (opt-in) | Additionally the session token, this device's wrapped account key, the vault list with the wrapped vault keys and the encrypted preferences (`localStorage`), and a service worker with a copy of the app's own files. With the device secret next to them, a copy of the browser profile can then be decrypted **without the server — and removing the device no longer cuts it off**. Off by default; the setting explains this. Turning it off, "Forget this browser", a dead session or a device the server reports as removed delete the stored data. |
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
  (`notekpr export-encrypted`, `GET /me/export`, or
  `notekpr-server export-user` from any server backup; it also lists the
  names of the people in it, which the server knows anyway, so recovered
  comments say who wrote them), everything a
  person could read decrypts offline into Markdown files with
  `notekpr recover`, without the server ([recovery.md](recovery.md)).

### Web application threats

* **XSS**: a note is content from other people (vault members, whoever shared
  it), shown in the page that holds the decrypted keys. The reading view
  renders Markdown with only `http(s)`/`mailto`/in-vault link targets and
  escaped attributes; the editor inserts only escaped text. Raw HTML in notes
  goes through an allow list (`crates/editor/src/html.rs`): it is tokenized as
  a browser tokenizes it (html5ever) and written out again from the tokens,
  keeping formatting elements (`details`, `kbd`, `sub`, tables, …) and a few
  attributes with checked values (`align`, `colspan`, `width`, …). Scripts and
  styles with their content, frames, objects, forms, SVG, MathML, comments,
  event handlers and the `style`, `class` and `id` attributes are dropped —
  nothing a note can draw over the app (a fake "re-enter your recovery key"),
  no DOM clobbering, nothing posing as the app's own markup. The one thing
  read from them is a text or background color on a `span`, `font` or `mark`:
  it becomes the app's class for the nearest color of a fixed palette
  (`crates/editor/src/color.rs`), so no CSS from a note reaches the page.
  Links and images in HTML follow the Markdown rules. The editor's preview of
  an HTML block is parsed into its own element, so no stray tag reaches the
  editor's lines. A strict Content-Security-Policy (`script-src 'self'
  'wasm-unsafe-eval'`, no inline scripts or event handlers, `frame-src
  'none'`, `frame-ancestors 'none'`, `object-src 'none'`) is a second layer.
  External images are not loaded (also prevents tracking pixels), and there
  are no embeds (`iframe`, video) for the same reason: whoever writes a note
  would choose what your browser loads.
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
  addresses the provider marks verified are used to find people, unless
  `--oidc-allow-unverified-email`.
* Identities are keyed by `iss`+`sub`, never e-mail. Authentication grants no
  vault access.
* Administration grants no vault access either. Admin clients are kept out
  of everything else by construction: the routes that serve vaults, notes,
  keys, people or the WebSocket only accept a session (`Principal`), which
  they never have; only the administration routes know their tokens. An
  admin-less server (`--adminless`) has no administrators among its people
  at all — people see only their own account and vaults — and is
  administered only by admin clients set up from the server's command line.
* Read-only mode (switched on, or the database at `max_storage_mb`) is
  checked under the write lock for pushes, and before anything else for
  every other change, with a short list of what stays writable (signing in
  and out, devices, revoking access, administration); a new route is
  refused until it is added there.
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
  bodies, names, user ids or IP addresses. No telemetry.
* Development mode (`--dev-mode`) allows password-less login and is announced in
  logs and on every page. The server refuses to start in dev mode on a
  non-loopback address, with an https public URL or with OIDC configured unless
  explicitly overridden; without dev mode, sessions of dev-mode accounts are
  ended and those accounts never become the first administrator.

## Residual risks and planned hardening

1. First-contact key substitution by a malicious server (mitigated only by
   fingerprint comparison).
2. Content authenticity with a colluding server: membership and content are
   signed (protocol §5.2), but someone who *could* write once (an editor
   since demoted or removed) still holds a valid signing key and could, with
   the server's help, add backdated content under an old key epoch. Content
   from before a vault required signatures is accepted unsigned. Recipients
   of single notes don't see the vault's log, so their clients don't check
   authors (their note keys are signed by the owner, so the keys themselves
   can't be swapped).
3. Metadata visible to the server (sizes, timing, social graph).
4. Browser key storage relies on the browser profile's security.
5. Post-quantum: key wrapping uses X25519 today; the format is versioned for a
   move to hybrid ML-KEM (X-Wing).
