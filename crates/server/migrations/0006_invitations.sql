-- Sharing is by invitation (docs/protocol.md §5.6): an owner invites
-- someone to a vault, or to one of its notes, by their account (someone
-- they share with already) or by an e-mail address, which may have no
-- account yet. The person accepts or declines; once they accepted, an
-- owner's app adds them (the keys, the membership log), and the
-- invitation ends.
--
-- The inviting owner signs the invitation (`signature`) and the person
-- invited their acceptance (`acceptance`): the server keeps both, it can
-- make neither.
CREATE TABLE invitations (
    id          TEXT PRIMARY KEY,
    vault_id    TEXT NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    -- A single note; NULL: the vault.
    object_id   TEXT,
    invited_by  TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- Whom: an account, or an address (lowercased) whoever has it sees it
    -- by. The address is forgotten once the invitation ends.
    user_id     TEXT REFERENCES users(id) ON DELETE CASCADE,
    email       TEXT,
    role        TEXT NOT NULL CHECK (role IN ('admin', 'editor', 'viewer')),
    -- As signed by the inviter's app (ms).
    created_at  INTEGER NOT NULL,
    signature   BLOB NOT NULL,
    -- When the server got it, for the daily limit, and until when it waits
    -- (to be answered, then, once accepted, to be done).
    sent_at     INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    -- pending: not answered; declined: looks pending to the owners, gone
    -- for the person; accepted: an owner's app adds them now; ended:
    -- cancelled or done, kept only to count towards the daily limit.
    state       TEXT NOT NULL DEFAULT 'pending'
                CHECK (state IN ('pending', 'declined', 'accepted', 'ended')),
    accepted_by TEXT REFERENCES users(id) ON DELETE CASCADE,
    accepted_at INTEGER,
    acceptance  BLOB,
    -- A note's comments so far go with it (signed with the rest).
    with_comments INTEGER NOT NULL DEFAULT 0,
    CHECK ((user_id IS NULL) <> (email IS NULL) OR state = 'ended')
);
CREATE INDEX invitations_vault ON invitations(vault_id);
CREATE INDEX invitations_user ON invitations(user_id);
CREATE INDEX invitations_email ON invitations(email);
CREATE INDEX invitations_sent ON invitations(invited_by, sent_at);
CREATE INDEX invitations_expiry ON invitations(expires_at);
