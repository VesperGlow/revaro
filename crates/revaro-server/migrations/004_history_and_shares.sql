ALTER TABLE shares ADD COLUMN expires_at TEXT;
CREATE INDEX shares_expiry ON shares(expires_at);
CREATE TABLE document_versions (
 id TEXT PRIMARY KEY,
 file_id TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
 object_key TEXT NOT NULL,
 size INTEGER NOT NULL CHECK(size >= 0),
 etag TEXT NOT NULL,
 content_hash TEXT NOT NULL,
 mime_type TEXT NOT NULL,
 created_at TEXT NOT NULL
);
CREATE INDEX document_versions_file ON document_versions(file_id,created_at DESC,id DESC);
CREATE INDEX document_versions_object ON document_versions(object_key);
CREATE TRIGGER versions_delete_cleanup AFTER DELETE ON document_versions
BEGIN
 INSERT INTO object_cleanup(object_key,reason,created_at,updated_at)
 VALUES(OLD.object_key,'version removed',strftime('%Y-%m-%dT%H:%M:%fZ','now'),strftime('%Y-%m-%dT%H:%M:%fZ','now'))
 ON CONFLICT(object_key) DO UPDATE SET reason=excluded.reason,updated_at=excluded.updated_at,generation=object_cleanup.generation+1;
END;
CREATE INDEX files_listing ON files(parent_id,deleted_at,kind,name COLLATE NOCASE,id);
