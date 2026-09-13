-- Queue reclamation in the same transaction that removes the final metadata.
-- The worker rechecks references, so copies and retained trash stay readable.
ALTER TABLE object_cleanup ADD COLUMN generation INTEGER NOT NULL DEFAULT 0;
CREATE TRIGGER files_delete_cleanup AFTER DELETE ON files
WHEN OLD.kind='file' AND OLD.object_key IS NOT NULL AND OLD.object_key<>''
BEGIN
  INSERT INTO object_cleanup(object_key,reason,created_at,updated_at)
  VALUES(OLD.object_key,'file deleted',strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))
  ON CONFLICT(object_key) DO UPDATE SET reason=excluded.reason,updated_at=excluded.updated_at,generation=object_cleanup.generation+1;
END;

CREATE TRIGGER files_replace_cleanup AFTER UPDATE OF object_key ON files
WHEN OLD.kind='file' AND OLD.object_key IS NOT NULL AND OLD.object_key<>'' AND OLD.object_key IS NOT NEW.object_key
BEGIN
  INSERT INTO object_cleanup(object_key,reason,created_at,updated_at)
  VALUES(OLD.object_key,'file replaced',strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))
  ON CONFLICT(object_key) DO UPDATE SET reason=excluded.reason,updated_at=excluded.updated_at,generation=object_cleanup.generation+1;
END;
