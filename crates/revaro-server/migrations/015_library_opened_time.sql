-- Timestamp normalization preserves mixed legacy formats and nanoseconds.
-- Keep the original string index for rollback to older application binaries.
-- Keep the original timestamp and index for older binaries and SQLite tools.
-- Only migration/repair queries use the application's timestamp function;
-- persistent schema objects need no custom SQLite functions.
ALTER TABLE library_state ADD COLUMN last_opened_sort TEXT;
UPDATE library_state SET last_opened_sort=COALESCE(revaro_timestamp(last_opened),'')
WHERE last_opened IS NOT NULL;
CREATE INDEX library_opened_time ON library_state(last_opened_sort DESC,file_id)
WHERE last_opened IS NOT NULL;
CREATE INDEX library_opened_pending ON library_state(file_id)
WHERE last_opened IS NOT NULL AND last_opened_sort IS NULL;

-- A legacy writer changes only last_opened. Invalidate its derived sort key;
-- queries fall back to normalizing the original timestamps until repaired.
CREATE TRIGGER library_opened_invalidate AFTER UPDATE OF last_opened ON library_state
WHEN NEW.last_opened IS NOT OLD.last_opened
 AND NEW.last_opened_sort IS OLD.last_opened_sort
BEGIN
    UPDATE library_state SET last_opened_sort=NULL WHERE file_id=NEW.file_id;
END;
