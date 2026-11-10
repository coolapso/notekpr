-- People a single note is shared with see where they are in that note's own
-- log, never the vault's sequence numbers: those would tell them how much
-- happens in the rest of the vault (docs/protocol.md §5.1).
--
-- A note's updates and its comments' updates are numbered together, one by
-- one, in log order (`updates.pos`). The note's row keeps the last number
-- given (`objects.pos_head`) and the number reached when sharing last
-- started (`objects.pos_base`): the people it is shared with count from
-- there, so they don't learn how much the note changed before. A snapshot
-- keeps how far it covers in these numbers (`covers_pos`) and where they
-- stood when it was stored (`created_pos`, for one carrying changes).
ALTER TABLE objects ADD COLUMN pos_head INTEGER NOT NULL DEFAULT 0;
ALTER TABLE objects ADD COLUMN pos_base INTEGER NOT NULL DEFAULT 0;
ALTER TABLE updates ADD COLUMN pos INTEGER NOT NULL DEFAULT 0;
ALTER TABLE snapshots ADD COLUMN covers_pos INTEGER NOT NULL DEFAULT 0;
ALTER TABLE snapshots ADD COLUMN created_pos INTEGER NOT NULL DEFAULT 0;

-- What the log holds now, numbered per note (a comments object `c_<note>`
-- counts with its note), in log order. Updates already pruned get no
-- number: nobody is served them any more.
UPDATE updates SET pos = numbered.n
FROM (
    SELECT vault_id, seq, ROW_NUMBER() OVER (
        PARTITION BY vault_id,
            CASE WHEN substr(object_id, 1, 2) = 'c_' AND length(object_id) > 2
                      AND substr(object_id, 3) <> 'index' AND substr(object_id, 3, 2) <> 'c_'
                 THEN substr(object_id, 3) ELSE object_id END
        ORDER BY seq) AS n
    FROM updates
) AS numbered
WHERE numbered.vault_id = updates.vault_id AND numbered.seq = updates.seq;

UPDATE objects SET pos_head = COALESCE((
    SELECT MAX(u.pos) FROM updates u
    WHERE u.vault_id = objects.vault_id
      AND (u.object_id = objects.id OR u.object_id = 'c_' || objects.id)), 0)
WHERE substr(id, 1, 2) <> 'c_';

-- Shared notes: everything under a key from before sharing started comes
-- before everything after it in the log.
UPDATE objects SET pos_base = COALESCE((
    SELECT MAX(u.pos) FROM updates u
    WHERE u.vault_id = objects.vault_id
      AND (u.object_id = objects.id OR u.object_id = 'c_' || objects.id)
      AND substr(u.payload, 2, 4) < objects.share_from), 0)
WHERE share_from IS NOT NULL;

UPDATE snapshots SET
    covers_pos = COALESCE((
        SELECT MAX(u.pos) FROM updates u
        WHERE u.vault_id = snapshots.vault_id
          AND (u.object_id = snapshots.object_id OR u.object_id = 'c_' || snapshots.object_id)
          AND u.seq <= snapshots.covers_seq), 0),
    created_pos = COALESCE((
        SELECT MAX(u.pos) FROM updates u
        WHERE u.vault_id = snapshots.vault_id
          AND (u.object_id = snapshots.object_id OR u.object_id = 'c_' || snapshots.object_id)
          AND u.seq <= snapshots.created_head), 0);

CREATE INDEX updates_pos ON updates(vault_id, object_id, pos);
