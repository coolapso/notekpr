-- The vault owner's signature on each note key given to someone a note is
-- shared with (R3): recipients use only keys the owner signed giving them.
ALTER TABLE object_keys ADD COLUMN signature BLOB;
