-- Every note gets its own document key (R2). A note's key may be private
-- (only vault members have it) or shared; people a note is shared with get
-- content from `share_from` on: the envelope epoch (4 bytes, big-endian, as
-- in the ciphertext header) of the key created when sharing started. NULL =
-- never shared.
ALTER TABLE objects ADD COLUMN share_from BLOB;

-- Notes that already have a document key got it when they were first shared.
UPDATE objects SET share_from = X'80000001' WHERE key_epoch IS NOT NULL;
