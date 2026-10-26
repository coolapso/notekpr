# Comments on notes (design)

> **Built** (2026-10-22; the reading view, the CLI for notes shared with
> you, @mentions and comments in exports 2026-10-23). The wire format is in
> [protocol.md §6.1](protocol.md#61-comments), what the server learns in
> [security.md](security.md); this file explains the design.

Like Notion: select some text in a note, add a comment; others reply below it;
anyone who can comment resolves the thread when it's done. Comments never
change the note's text.

## 1. What people get

* **Comments live on the text.** Select text → *Comment* (a button that
  appears by the selection, the toolbar, or a shortcut) → write. The text
  gets a highlight, and the thread opens right there, next to it — in the
  editor and in the reading view. Clicking a highlight opens its thread again.
  The side panel also lists all threads of the note (open ones first), each
  with the text it's about, to find them in long notes.
* **Threads:** replies, *Resolve* / *Reopen*, edit or delete your own
  comments. Resolved threads lose their highlight and fold away in the panel.
* **The highlight follows the text** through everyone's edits — also edits
  made in your own editor and synced through the Markdown folder (they are
  merged as small changes, not as a new file). If the commented text is
  deleted, the thread stays, showing the text it was about.
* **Who:** everyone who can read the note — owners, editors **and viewers**
  of the vault, and the people a single note is shared with (either role).
  A vault setting, *Viewers can comment* (on by default), lets its owner
  turn it off for viewers of the vault and for people its notes are shared
  with read-only.
* **Deleting:** the author of a comment can always delete it (viewers
  included); vault owners can delete any comment.
* **@mentions:** `@` and a name in a comment mentions that person: typing
  `@` offers the vault's members, and the mention shows highlighted. Only
  the vault's members can be mentioned. People a single note is shared with
  can't be mentioned; they may mention the note's owner and the people who
  commented on it. The mentioned person's own devices notice it (§3,
  *Mentions*): a mentions inbox in the sidebar with the number of new ones, a
  message when one arrives, and — if they turn it on in this browser — a
  system notification while the app is open but not in front.
* **Own editors and the CLI:** comments are not written into the Markdown
  files (they would stop being plain Markdown; Notion's export drops them
  too). `notekpr comments …` lists, adds, replies to and resolves them
  (`notekpr shared comments …` on notes shared with you), and `notekpr
  mentions` lists the comments that mention you, so AI agents can take part
  ([agents.md](agents.md)); the gateway gets the same.
* **Exports** keep them, readable, beside the notes (§6, *Exports*).

Later: reactions, comments on a whole note (no selection).

## 2. Why not inside the note

The simplest place would be the note's own encrypted document, next to its
text — but the server refuses viewers' changes to notes, and must keep doing
so, or a viewer could change the text. Comments by viewers therefore need
their own object, and the server must know it belongs to a note to let
viewers write it — the one new thing it learns (§7).

So comments are stored apart from the note — in the server's database, as
the note's encrypted comment thread, each comment with a reference to the
text it is about — and the apps draw them over the note.

## 3. A thread object per note

Each note's comments are one **comments object**, `c_<note id>` (note ids
never contain `_`, so everyone — server included — knows which note it
belongs to without asking), created by the first comment. It is **not** a
CRDT document like notes: it is an append-only list of **records**, one
record per update in the vault's log:

* every record is encrypted with the note's key (§5) and **signed by its
  author's account key over its content** (the record, the vault and the
  note), inside the encryption. Clients check it against the author's
  pinned key, as for note edits. Being over the content rather than the
  ciphertext, it stays valid when the record is encrypted again under
  another key (§5). The update carrying it is signed as usual by whoever
  pushed it (`sign_update`, over the ciphertext);
* records are never changed or removed: an edit, a deletion or a resolve is a
  new record pointing at an earlier one; clients fold the list into threads;
* no snapshots and no compaction (a snapshot would drop the per-record
  signatures); comments are small, so the log stays small.

Why not a CRDT here: in a merged CRDT anyone allowed to write can delete or
rewrite anyone's entry without a trace. With signed, append-only records a
viewer can add comments but cannot touch other people's — nor pretend to be
someone else.

### Records

```
id        random, unique in the thread object
kind      comment | reply | edit | delete | resolve | reopen
thread    the comment a reply / resolve / reopen belongs to
target    the record an edit / delete changes
body      text (Markdown inline syntax: emphasis, code, links)
anchor    comment only: start and end in the note's text, and the quoted text
mentions  comment, reply, edit: the people the body mentions (user ids)
created   time (ms), as the author's device saw it
```

The author is the signer of the record (the server stores who pushed the
update, but can forge neither signature). Folding rules every client applies
the same way:

* `edit` counts only when signed by the target's author (its `mentions`
  replace the comment's); `delete` when signed by the target's author or
  by a vault owner (moderation);
* `resolve` / `reopen` by anyone who may comment; the latest one wins;
* replies to a comment you can't see (§5) are hidden;
* records that don't decode or don't verify are ignored (and logged).

### Anchors

The start and end are yrs *sticky positions* in the note's text: they name
characters, not offsets, so they keep pointing at the same text whatever is
inserted or deleted around it (the format is the Yjs one, stable). When both
ends meet because the text is gone, the thread shows its quote instead of a
highlight. Restoring an older version of the note rewrites the text: threads
on text that changed show their quote.

### Mentions

A mention is `@Name` in the body plus the person's user id in the record's
`mentions`, inside the encryption: the server never learns who is
mentioned. The name stays readable in the body everywhere (the CLI,
exports, a recovery without a server); the id says whom it notifies, even
after a rename and among people with the same name.

* **Who may be mentioned** is the writer's client's choice, and it offers
  only these: in a vault, its members as the owner-signed membership log
  vouches for them; on a note shared with you, its owner and the people
  who commented on it (you see no members). People a note is shared with
  are never notified: their clients look for mentions in vaults they are a
  member of only. A record naming someone else notifies nobody — they
  can't read it.
* **Notifications** are found by each person's own clients in the comments
  they sync: the comments, replies and edits that mention them, by others,
  that still do (an edit can take a mention back), in notes that aren't in
  the trash. The web app shows them in the sidebar's inbox, says so when
  one arrives (not for those that were there before a vault's first
  catch-up in a session) and, if asked to in this browser, shows a system
  notification while the app is open but not in front; `notekpr mentions`
  lists them.
* **Read or not** is kept in the person's encrypted preferences (which the
  server stores as ciphertext and their devices sync): per vault, read up
  to a place in its log (the log's order, which a device's clock can't
  fake), plus single ones read after it. Opening a mention's thread marks
  it read; "Mark all as read" (`notekpr mentions --mark-read`) moves each
  vault's place to its newest mention.

## 4. Server changes

* **Creating:** like any object, by its first update.
* **Writing:** `write_access` for a comments object = read access to its note
  (any vault member, or anyone the note is shared with), minus viewers when
  the vault's *Viewers can comment* is off. Everything else about pushing
  stays (idempotent `client_update_id`, ordering, acks).
* **The setting:** `vaults.viewers_comment` (default on), set by the owner
  (`PATCH /vaults/{v}`) — plain, since the server enforces it.
* **Keys:** `check_payload_key` compares against the note's key epoch (§5).
* **Refused for comments objects:** snapshots, records over 64 KiB, deleting
  or sharing them on their own.
* **Reading and live events:** whoever can read the note reads its comments;
  a subscription to one note (people it's shared with) also gets its
  comments object's updates.
* **Purging a note** purges its comments; trashing doesn't. Undoing the
  purge brings them back with it.

## 5. Keys and single-note sharing

The comments object uses its note's key (`VaultReplica::ring_for` looks up
the note for a comments object): its own document key, or the vault key for
notes from before notes had their own keys. Rotations need nothing extra.

Sharing a note gives it a fresh key that its earlier keys can't be derived
from, and people it is shared with only decrypt content from that key on
(`share_from`). Comments written before are under earlier keys, so **sharing
asks whether to include them**: *Include existing comments* in the share
dialog, on by default (`--without-comments` in the CLI). Clients keep, for
each record, the newest key epoch they've seen it under, and learn where
sharing started from `share_from` in the vault's keys; that tells them which
records people the note is shared with can't read yet.

* **Included:** the sharer's client pushes the records they can't read again
  under the note's current key. Their authors' signatures still hold
  (§3), so they show as written by their authors. This is one more step of
  the sharing operation, like the note's own content.
* **Not included:** people the note is shared with see the comments written
  from then on; replies to earlier threads are hidden for them.
* **The choice is per note, not per person:** everyone a note is shared with
  uses the same document key, so existing comments included for one person
  are visible to everyone the note is (or will be) shared with. Sharing
  again later can include them then; once included, they can't be taken
  back from those who saw them. The share dialog says so.

## 6. Clients

* **core:** the record format, signing domain and folding (`comments.rs`),
  with unit tests; `VaultReplica` keeps each note's records; the author check
  accepts any reader of the note (today only writers' updates are accepted).
* **sync:** the outbox must not merge comment records the way it merges a
  note's CRDT updates; refusals and acks as usual.
* **web:** the *Comment* button by selected text and `Alt+M`, highlights
  drawn behind the editor's text (`Editor::set_marks`: an overlay, so the
  editor's DOM text stays the Markdown source; they follow edits), the
  thread by its text, the side panel's *Comments* list (a menu on notes
  shared with you), a count on the note. In the reading view the
  highlights are part of the rendered text (`span.cmt`), and every piece
  of text that is the note's source character for character says where it
  is in it (`data-o`), so a selection there is commented on as in the
  editor. `@` in a comment box offers people to mention; the sidebar's `@`
  is the mentions inbox; Settings → Editor → *Mentions* turns system
  notifications on for this browser.
* **CLI and gateway:** `comments list|add|reply|resolve|reopen|edit|delete`
  (`add --on "quoted text"` finds the text to anchor to), the same as
  `shared comments …` for notes shared with you, `mentions [--unread]
  [--mark-read]`, `vault viewers-comment`, `share-note --without-comments`.
  `@Name` in a comment's text mentions whom it names among those who may
  be mentioned; other `@words` are left as text, with a warning.
* **Demo:** threads in several notes, including a viewer's comment (carol in
  *Family*) and a resolved one, mentions, and comments by someone a note
  is shared with.

### Exports

`notekpr export`, the web app's vault export and `notekpr recover` write
each note's threads as a Markdown file of their own, at `_comments/` and
the note's path (`_comments/Projects/Plan.md`; a file of the vault keeps
its path, the comments file gets another): a heading per thread quoting
its text, in the order of the note (threads whose text is gone last), then
its comment and replies with their authors' names and times (UTC),
resolved threads marked. Imports leave `_comments/` out: comments can't
be brought back, as they are signed by their authors.

The encrypted export (`/me/export`) holds the comment records already (the
vaults' logs); for an offline recovery it also carries the names of the
people in it — the members of the person's vaults, and the authors of
what they get of notes shared with them, never other members of those
vaults — and each vault's owner, whose deletions count. A recovery has no
membership log to check signatures against, so it takes the records as
they are, as it takes the notes.

## 7. What the server learns

Nothing about the content: text, quotes, positions and who is replying to
whom are encrypted. New: **which objects are the comment threads of which
note**, when, by whom and how much is commented (the authors, times and
sizes of the records) — as it already knows for note edits — and each
vault's *Viewers can comment* setting. When a note is shared with existing
comments included, it sees that their number of records is pushed again.
Viewers now write to the vault (comments only), so they also count against
the rate limits.

Mentions add nothing: who is mentioned is inside the encrypted records, and
notifications are found by the mentioned person's own devices; which
mentions they read is in their encrypted preferences. An encrypted export
carries people's names and the vaults' owners (§6, *Exports*), which the server
knows anyway. A system notification, when someone turns them on, shows who
mentioned them, the note's title and the start of the comment on that
device's screen, like any app's notifications.

## 8. Decisions (2026-10-21)

1. Viewers can comment (vault viewers and people a note is shared with
   read-only), and highlight text to do so; a vault setting turns it off.
2. Comments are shown on the text they are about; the side panel lists them.
3. Sharing a note asks whether to include its existing comments, on by
   default (§5).
4. Authors can always delete their own comments, vault owners anyone's.

(2026-10-23)

5. Mentions notify in the app, found by the mentioned person's own devices
   (an inbox, a message, and opt-in system notifications from the
   browser); the server learns nothing new, so there are no e-mails.
6. Only a vault's members can be mentioned. People a note is shared with
   can't be; they may mention its owner and those who commented on it.
7. Exports put each note's comments in `_comments/` as Markdown; imports
   leave that folder out.

## 9. Order of work

1. core: records, folding, record signatures, re-encryption for sharing —
   unit tests.
2. server: migrations, creating, access, the vault setting, key and snapshot
   rules — `crates/server/tests/api.rs` (viewers can comment unless turned
   off, can't edit notes; share recipients; refused snapshots; purge).
3. engine and sync: comment objects in `VaultReplica`, author checks, outbox —
   unit and property tests in `crates/sync`.
4. CLI and gateway — `crates/client/tests/`.
5. web: panel and reading view first, then editor highlights — browser tests
   with an owner, an editor and a viewer.
6. docs, feature lists, demo data.
