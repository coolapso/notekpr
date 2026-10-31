# Who can do what in a vault

Everyone in a vault has one of four roles. The person who creates a vault is
its first owner.

| | Owner | Admin | Editor | Viewer |
|---|:-:|:-:|:-:|:-:|
| Read notes, see their version history, export the vault | ✓ | ✓ | ✓ | ✓ |
| Comment, resolve threads, edit or delete one's own comments | ✓ | ✓ | ✓ | ✓¹ |
| Write, import, move to the trash and restore from it | ✓ | ✓ | ✓ | |
| Empty the trash (delete for good), and undo that for a while | ✓ | ✓ | | |
| See the vault's activity log | ✓ | ✓ | | |
| Delete other people's comments | ✓ | | | |
| Add and remove people, change their roles, make owners | ✓ | | | |
| Share a single note with someone outside the vault | ✓ | | | |
| Rename the vault, set its icon, archive it, *Viewers can comment* | ✓ | | | |
| Rotate the vault's encryption key | ✓ | | | |
| Delete the vault | ✓ | | | |
| Leave the vault | ✓² | ✓ | ✓ | ✓ |

¹ Unless an owner turns *Viewers can comment* off.
² While another owner stays.

## Owners

* **Owners are equal.** Any owner can do everything above, including
  demoting or removing another owner. To make someone an owner, they must
  already be in the vault and have set up encryption.
* **A vault always keeps an owner.** The last owner can't leave. Nor can
  they delete their account while other people use one of their vaults:
  they first make someone else an owner of it, or delete it.
* **Two owners are safer than one.** Only owners can change who is in a
  vault. If its only owner loses every device and their recovery key,
  nobody can add or remove people any more, though everyone keeps what they
  can read and can export it into a vault of their own. A vault other people
  use suggests a second owner; the hint can be hidden.
* **Storage:** a vault counts against the limits of one of its owners,
  its creator at first. When that owner leaves, stops being an owner or
  deletes their account, it moves to the owner who has been in the vault
  longest.

## Admins

An admin is an editor trusted to clean up: emptying the trash and checking
the activity log. They have no say over who is in the vault, its keys or its
settings, so they can't lock anyone out or take the vault over.

## Agents

AI agents and scripts can be editors or viewers, never admins or owners
([agents](agents.md)).

## Single notes

A note can be shared on its own, as editor or viewer, with someone outside
the vault. They get only that note (from when it was shared on), and can
comment on it (as viewer, unless *Viewers can comment* is off). They don't
get the vault, its other notes, its members or its attachments. Only owners
share single notes. Someone who joins the vault no longer needs the note
shared on its own: their vault role covers it.

## Not to be confused with server administrators

A server administrator (an account with the admin flag) runs the instance:
accounts, limits, storage, read-only mode. That gives no access to any
vault. They see vault ids, owners and sizes, never names or content.
