# Getting your notes out, whatever happens

Your notes are yours. Even if the server is gone, the apps stop working or
you simply want to leave, you can turn everything you could read back into
ordinary Markdown files — offline, with nothing but your **recovery key** (or
a device that is still signed in) and an **encrypted export**.

## The pieces

* **Encrypted export** — one JSON file with everything you can read, exactly as
  the server stores it: encrypted notes (the latest snapshot of each and the
  changes made after it — not their older revisions), attachments, and your
  copies of the keys, themselves encrypted. Nobody can read it without your
  keys, so it is safe to keep in a backup.
* **Recovery key** — shown once when you set up your account (11 groups of
  five letters and digits). It unlocks your account key, which unlocks the vault
  keys in the export.
* **`notekpr recover`** — the desktop client command that puts the two
  together. It needs no server and no network.

## Keep an encrypted export (recommended)

From a signed-in desktop client, now and then:

```sh
notekpr export-encrypted --out ~/Backups/notekpr-$(date +%F).json
```

or download it from the server's API as yourself (`GET /api/v1/me/export`).

## Recover

**With your recovery key and an export** (no server needed):

```sh
notekpr recover --from notekpr-2026-10-19.json --out ~/Recovered
# asks for the recovery key; or --recovery-key-file FILE, or the
# NOTEKPR_RECOVERY_KEY environment variable
```

`recover` only needs the `notekpr` binary — no profile, no sign-in and no
network.

**With a device that is still signed in** (downloads the export and decrypts
it with this device's keys):

```sh
notekpr recover --out ~/Recovered
```

If the server can't be reached (or this device is no longer signed in), the
same command recovers from the device's own encrypted copies instead, and says
so; `--local` does that without contacting the server at all. Those copies hold
the vaults this device keeps, as far as it synced them (with its unsent edits),
and the attachments it downloaded; others are listed as not recovered. Notes
shared with you one by one aren't kept on devices: only an export has them.

**From a server backup** (if you run the server, or can get its database
file): first write your encrypted export from the backup with the server tool —
it never decrypts anything — then recover as above.

```sh
notekpr-server export-user --backup notekpr-2026-10-01.db --user you@example.com --out you.json
notekpr recover --from you.json --out ~/Recovered
```

`--user` takes your e-mail address, display name or user id. The backup file
isn't changed (the tool works on a copy).

## What you get

```
~/Recovered/
  Family/                   one folder per vault
    Plans/Trip.md
    assets/photo.jpg
    _trash/Old note.md      what was in the trash
  Shared with me/           notes shared with you individually
    Weekend trip.md
```

`--out` must be a new or empty folder. Each note is written with its current
text (older revisions aren't recovered). Folders are private to you (mode
0700, files 0600) — the files are plain text now, so treat the folder like any
private document. Anything that couldn't be decrypted (for example content
written under a key you never received) is listed at the end; everything else
is still recovered.

## Without any notekpr software

The formats are standard and documented, so an independent tool can do the
same: the export is JSON (`EncryptedExport` in
[`crates/core/src/protocol.rs`](../crates/core/src/protocol.rs)), and the
encryption is described in [protocol.md §2](protocol.md#2-encryption-formats):

1. Recovery key → account secret: decode the key (base32, 32 bytes + 2-byte
   checksum) and open `recovery_envelope` (XChaCha20-Poly1305 symmetric
   envelope, purpose `Recovery`, context = your user id).
2. Account secret → vault keys: open your `keys.envelopes` (HPKE RFC 9180,
   X25519 / HKDF-SHA256 / ChaCha20-Poly1305) for each epoch; older epochs
   follow from `keys.chain`. Shared notes' own keys come from `keys.objects`
   (wrapped under the vault key) or, for notes shared with you, from your
   envelopes in `shared_notes[].keys`.
3. Decrypt each snapshot and update (`Snapshot` / `Update` purposes) and apply
   them as Yjs/yrs v1 updates. The vault index (object `index`, a `Y.Map` named
   `files`) maps entry ids to paths and kinds; each note's text is a `Y.Text`
   named `content`. Attachments are single symmetric envelopes (purpose
   `Attachment`, context = vault id and attachment id).
