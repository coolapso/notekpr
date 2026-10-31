-- Attachments may live in a file store outside the database (a folder or
-- S3-compatible storage, `--file-store`; `blobs.rs`). The rows keep
-- everything else (size, author, times) either way.
--
-- The file's name in the store; NULL: the bytes are in `data` (left empty
-- for a file in the store).
ALTER TABLE attachments ADD COLUMN file_key TEXT;
CREATE UNIQUE INDEX attachments_file_key ON attachments(file_key) WHERE file_key IS NOT NULL;

-- Files the store is to delete. An upload is recorded before its file is
-- written (`upload` = 1) and claimed by its row; one never claimed (a crash
-- in between, a refused upload) is deleted after a while. A row's deletion
-- records its file (the trigger below, also when its vault or account goes),
-- kept in the store for a grace period so a recent backup of the database
-- still finds it.
CREATE TABLE file_deletions (
    file_key TEXT PRIMARY KEY,
    since    INTEGER NOT NULL,
    upload   INTEGER NOT NULL DEFAULT 0
);

CREATE TRIGGER attachments_file_deleted AFTER DELETE ON attachments
WHEN OLD.file_key IS NOT NULL
BEGIN
    INSERT OR REPLACE INTO file_deletions (file_key, since, upload)
    VALUES (OLD.file_key, CAST(strftime('%s', 'now') AS INTEGER) * 1000, 0);
END;
