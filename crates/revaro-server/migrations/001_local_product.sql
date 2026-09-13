-- Initial schema for the local-only product.
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE shares (
    file_id TEXT PRIMARY KEY,
    token TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
);

CREATE TABLE "files" (
    id TEXT PRIMARY KEY,
    parent_id TEXT,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('file', 'directory')),
    object_key TEXT,
    size INTEGER NOT NULL DEFAULT 0 CHECK(size >= 0),
    mime_type TEXT,
    etag TEXT,
    status TEXT NOT NULL DEFAULT 'ready' CHECK(status IN ('pending', 'ready', 'deleting', 'failed')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    deleted_at TEXT,
    restore_parent_id TEXT,
    trash_root_id TEXT,
    content_hash TEXT,
    hash_algorithm TEXT,
    FOREIGN KEY(parent_id) REFERENCES "files"(id),
    CHECK((kind = 'directory' AND object_key IS NULL) OR kind = 'file')
);

CREATE TABLE media_progress (
    file_id TEXT PRIMARY KEY,
    position_ms INTEGER NOT NULL CHECK(position_ms >= 0),
    duration_ms INTEGER NOT NULL DEFAULT 0 CHECK(duration_ms >= 0),
    updated_at TEXT NOT NULL,
    FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
);

CREATE TABLE uploads (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL,
    mode TEXT NOT NULL CHECK(mode IN ('single', 'multipart')),
    object_key TEXT NOT NULL,
    multipart_id TEXT,
    part_size INTEGER NOT NULL CHECK(part_size > 0),
    expected_size INTEGER NOT NULL CHECK(expected_size >= 0),
    mime_type TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending', 'completed', 'aborted', 'failed')),
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    content_hash TEXT,
    completed_at TEXT,
    FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE,
    CHECK((mode = 'single' AND multipart_id IS NULL) OR
          (mode = 'multipart' AND multipart_id IS NOT NULL))
);

CREATE TABLE media_metadata (
    file_id TEXT PRIMARY KEY,
    duration_ms INTEGER NOT NULL DEFAULT 0 CHECK(duration_ms >= 0),
    container TEXT NOT NULL DEFAULT '',
    video_codec TEXT NOT NULL DEFAULT '',
    audio_codec TEXT NOT NULL DEFAULT '',
    width INTEGER NOT NULL DEFAULT 0 CHECK(width >= 0),
    height INTEGER NOT NULL DEFAULT 0 CHECK(height >= 0),
    bitrate INTEGER NOT NULL DEFAULT 0 CHECK(bitrate >= 0),
    chapters_json TEXT NOT NULL DEFAULT '[]',
    analyzed_at TEXT NOT NULL,
    frame_rate TEXT NOT NULL DEFAULT '',
    video_profile TEXT NOT NULL DEFAULT '',
    video_level INTEGER NOT NULL DEFAULT 0,
    subtitles_json TEXT NOT NULL DEFAULT '[]',
    source_etag TEXT NOT NULL DEFAULT '',
    probe_version INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
);

CREATE TABLE directory_stats (
    directory_id TEXT PRIMARY KEY,
    file_count INTEGER NOT NULL DEFAULT 0 CHECK(file_count >= 0),
    total_bytes INTEGER NOT NULL DEFAULT 0 CHECK(total_bytes >= 0),
    FOREIGN KEY(directory_id) REFERENCES files(id) ON DELETE CASCADE
);

CREATE TABLE upload_parts (
    upload_id TEXT NOT NULL,
    part_number INTEGER NOT NULL CHECK(part_number BETWEEN 1 AND 10000),
    size INTEGER NOT NULL CHECK(size > 0),
    etag TEXT NOT NULL,
    content_hash TEXT,
    completed_at TEXT NOT NULL,
    PRIMARY KEY(upload_id, part_number),
    FOREIGN KEY(upload_id) REFERENCES uploads(id) ON DELETE CASCADE
);

CREATE TABLE tasks (
    id TEXT PRIMARY KEY,
    type TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('queued','running','waiting_input','retrying','completed','failed','cancelled')),
    phase TEXT NOT NULL DEFAULT '',
    progress REAL NOT NULL DEFAULT 0 CHECK(progress >= 0 AND progress <= 100),
    speed INTEGER NOT NULL DEFAULT 0 CHECK(speed >= 0),
    eta_seconds INTEGER,
    retry_count INTEGER NOT NULL DEFAULT 0 CHECK(retry_count >= 0),
    max_retries INTEGER NOT NULL DEFAULT 3 CHECK(max_retries >= 0),
    error TEXT NOT NULL DEFAULT '',
    source_type TEXT,
    source_id TEXT,
    payload_json TEXT NOT NULL DEFAULT '{}',
    cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK(cancel_requested IN (0,1)),
    created_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT,
    heartbeat_at TEXT,
    updated_at TEXT NOT NULL
);

CREATE TABLE task_files (
    task_id TEXT NOT NULL,
    file_id TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'output',
    PRIMARY KEY(task_id, file_id, role),
    FOREIGN KEY(task_id) REFERENCES tasks(id) ON DELETE CASCADE,
    FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
);

CREATE TABLE object_cleanup (
    object_key TEXT PRIMARY KEY,
    reason TEXT NOT NULL,
    retry_count INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX sessions_token_idx ON sessions(token_hash);

CREATE INDEX sessions_expiry_idx ON sessions(expires_at);

CREATE UNIQUE INDEX shares_token_idx ON shares(token);

CREATE INDEX files_parent_idx ON files(parent_id);

CREATE UNIQUE INDEX files_unique_name ON files(parent_id, name) WHERE deleted_at IS NULL;

CREATE INDEX files_deleted_idx ON files(deleted_at);

CREATE INDEX files_trash_root_idx ON files(trash_root_id);

CREATE INDEX uploads_pending_idx ON uploads(status, expires_at);

CREATE INDEX files_active_children_idx
ON files(parent_id, kind DESC, name COLLATE NOCASE)
WHERE deleted_at IS NULL;

CREATE INDEX media_metadata_analyzed_idx ON media_metadata(analyzed_at);

CREATE UNIQUE INDEX tasks_source_idx ON tasks(source_type, source_id)
WHERE source_type IS NOT NULL AND source_id IS NOT NULL;

CREATE INDEX tasks_status_idx ON tasks(status, updated_at);

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

INSERT INTO files (id,parent_id,name,kind,status,created_at,updated_at)
VALUES ('00000000-0000-0000-0000-000000000000',NULL,'','directory','ready',CURRENT_TIMESTAMP,CURRENT_TIMESTAMP);
