-- Hot paths: active directory browsing, job lists and persistent media probes.
CREATE INDEX IF NOT EXISTS files_active_children_idx
ON files(parent_id, kind DESC, name COLLATE NOCASE)
WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS download_jobs_status_updated_idx ON download_jobs(status, updated_at);
CREATE INDEX IF NOT EXISTS url_download_jobs_status_updated_idx ON url_download_jobs(status, updated_at);
CREATE INDEX IF NOT EXISTS media_metadata_analyzed_idx ON media_metadata(analyzed_at);

-- directory_stats is maintained transactionally by triggers for every files
-- mutation, including import paths that intentionally bypass HTTP handlers.
CREATE TABLE directory_stats (
    directory_id TEXT PRIMARY KEY,
    file_count INTEGER NOT NULL DEFAULT 0 CHECK(file_count >= 0),
    total_bytes INTEGER NOT NULL DEFAULT 0 CHECK(total_bytes >= 0),
    FOREIGN KEY(directory_id) REFERENCES files(id) ON DELETE CASCADE
);

INSERT INTO directory_stats(directory_id,file_count,total_bytes)
SELECT d.id,
       COALESCE(SUM(CASE WHEN f.kind='file' AND f.status='ready' AND f.deleted_at IS NULL THEN 1 ELSE 0 END),0),
       COALESCE(SUM(CASE WHEN f.kind='file' AND f.status='ready' AND f.deleted_at IS NULL THEN f.size ELSE 0 END),0)
FROM files d
LEFT JOIN (
    WITH RECURSIVE ancestors(file_id,directory_id) AS (
        SELECT id,parent_id FROM files WHERE kind='file'
        UNION ALL
        SELECT a.file_id,p.parent_id FROM ancestors a JOIN files p ON p.id=a.directory_id
        WHERE a.directory_id IS NOT NULL
    ) SELECT file_id,directory_id FROM ancestors WHERE directory_id IS NOT NULL
) a ON a.directory_id=d.id
LEFT JOIN files f ON f.id=a.file_id
WHERE d.kind='directory'
GROUP BY d.id;

CREATE TRIGGER directory_stats_directory_insert AFTER INSERT ON files
WHEN NEW.kind='directory'
BEGIN
  INSERT OR IGNORE INTO directory_stats(directory_id) VALUES(NEW.id);
END;

CREATE TRIGGER directory_stats_file_insert AFTER INSERT ON files
WHEN NEW.kind='file' AND NEW.status='ready' AND NEW.deleted_at IS NULL
BEGIN
  UPDATE directory_stats SET file_count=file_count+1,total_bytes=total_bytes+NEW.size
  WHERE directory_id IN (WITH RECURSIVE a(id) AS (SELECT NEW.parent_id UNION ALL SELECT f.parent_id FROM files f JOIN a ON f.id=a.id WHERE a.id IS NOT NULL) SELECT id FROM a WHERE id IS NOT NULL);
END;

CREATE TRIGGER directory_stats_file_delete BEFORE DELETE ON files
WHEN OLD.kind='file' AND OLD.status='ready' AND OLD.deleted_at IS NULL
BEGIN
  UPDATE directory_stats SET file_count=file_count-1,total_bytes=total_bytes-OLD.size
  WHERE directory_id IN (WITH RECURSIVE a(id) AS (SELECT OLD.parent_id UNION ALL SELECT f.parent_id FROM files f JOIN a ON f.id=a.id WHERE a.id IS NOT NULL) SELECT id FROM a WHERE id IS NOT NULL);
END;

CREATE TRIGGER directory_stats_file_update AFTER UPDATE OF parent_id,size,status,deleted_at ON files
WHEN OLD.kind='file' OR NEW.kind='file'
BEGIN
  UPDATE directory_stats SET
    file_count=file_count-CASE WHEN OLD.kind='file' AND OLD.status='ready' AND OLD.deleted_at IS NULL THEN 1 ELSE 0 END,
    total_bytes=total_bytes-CASE WHEN OLD.kind='file' AND OLD.status='ready' AND OLD.deleted_at IS NULL THEN OLD.size ELSE 0 END
  WHERE directory_id IN (WITH RECURSIVE a(id) AS (SELECT OLD.parent_id UNION ALL SELECT f.parent_id FROM files f JOIN a ON f.id=a.id WHERE a.id IS NOT NULL) SELECT id FROM a WHERE id IS NOT NULL);
  UPDATE directory_stats SET
    file_count=file_count+CASE WHEN NEW.kind='file' AND NEW.status='ready' AND NEW.deleted_at IS NULL THEN 1 ELSE 0 END,
    total_bytes=total_bytes+CASE WHEN NEW.kind='file' AND NEW.status='ready' AND NEW.deleted_at IS NULL THEN NEW.size ELSE 0 END
  WHERE directory_id IN (WITH RECURSIVE a(id) AS (SELECT NEW.parent_id UNION ALL SELECT f.parent_id FROM files f JOIN a ON f.id=a.id WHERE a.id IS NOT NULL) SELECT id FROM a WHERE id IS NOT NULL);
END;

-- Moving an active directory does not touch its child rows, so transfer its
-- already aggregated subtree once between the two ancestor chains.
CREATE TRIGGER directory_stats_directory_move AFTER UPDATE OF parent_id ON files
WHEN NEW.kind='directory' AND NEW.deleted_at IS NULL AND OLD.deleted_at IS NULL AND OLD.parent_id IS NOT NEW.parent_id
BEGIN
  UPDATE directory_stats SET
    file_count=file_count-(SELECT file_count FROM directory_stats WHERE directory_id=NEW.id),
    total_bytes=total_bytes-(SELECT total_bytes FROM directory_stats WHERE directory_id=NEW.id)
  WHERE directory_id IN (WITH RECURSIVE a(id) AS (SELECT OLD.parent_id UNION ALL SELECT f.parent_id FROM files f JOIN a ON f.id=a.id WHERE a.id IS NOT NULL) SELECT id FROM a WHERE id IS NOT NULL);
  UPDATE directory_stats SET
    file_count=file_count+(SELECT file_count FROM directory_stats WHERE directory_id=NEW.id),
    total_bytes=total_bytes+(SELECT total_bytes FROM directory_stats WHERE directory_id=NEW.id)
  WHERE directory_id IN (WITH RECURSIVE a(id) AS (SELECT NEW.parent_id UNION ALL SELECT f.parent_id FROM files f JOIN a ON f.id=a.id WHERE a.id IS NOT NULL) SELECT id FROM a WHERE id IS NOT NULL);
END;

ALTER TABLE media_metadata ADD COLUMN frame_rate TEXT NOT NULL DEFAULT '';
ALTER TABLE media_metadata ADD COLUMN video_profile TEXT NOT NULL DEFAULT '';
ALTER TABLE media_metadata ADD COLUMN video_level INTEGER NOT NULL DEFAULT 0;
ALTER TABLE media_metadata ADD COLUMN subtitles_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE media_metadata ADD COLUMN source_etag TEXT NOT NULL DEFAULT '';
