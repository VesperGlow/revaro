-- Durable upload acknowledgements, content integrity, and the common task plane.
-- Existing files intentionally keep a NULL hash and are filled only when their
-- bytes pass through a new ingest/processing operation.
ALTER TABLE files ADD COLUMN content_hash TEXT;
ALTER TABLE files ADD COLUMN hash_algorithm TEXT;

ALTER TABLE uploads ADD COLUMN content_hash TEXT;
ALTER TABLE uploads ADD COLUMN completed_at TEXT;

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

CREATE UNIQUE INDEX tasks_source_idx ON tasks(source_type, source_id)
WHERE source_type IS NOT NULL AND source_id IS NOT NULL;
CREATE INDEX tasks_status_idx ON tasks(status, updated_at);

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

-- Keep the mature BT/URL runners as execution adapters while making tasks the
-- single queryable lifecycle plane. Triggers make legacy code paths atomic
-- with their task projection and avoid a risky rewrite of the download engine.
INSERT OR IGNORE INTO tasks(id,type,status,phase,progress,speed,error,source_type,source_id,created_at,finished_at,updated_at)
SELECT id,'bt',CASE status WHEN 'done' THEN 'completed' WHEN 'failed' THEN 'failed' WHEN 'cancelled' THEN 'cancelled' WHEN 'waiting' THEN 'waiting_input' WHEN 'paused' THEN 'waiting_input' WHEN 'downloading' THEN 'running' WHEN 'importing' THEN 'running' ELSE 'queued' END,
status,CASE WHEN selected_size>0 THEN MIN(100.0,completed_size*100.0/selected_size) ELSE 0 END,CASE WHEN status='importing' THEN import_speed ELSE download_speed END,error,'download',id,created_at,CASE WHEN status IN ('done','failed','cancelled') THEN updated_at END,updated_at FROM download_jobs;

CREATE TRIGGER download_jobs_task_insert AFTER INSERT ON download_jobs BEGIN
 INSERT OR IGNORE INTO tasks(id,type,status,phase,source_type,source_id,created_at,updated_at) VALUES(NEW.id,'bt',CASE NEW.status WHEN 'waiting' THEN 'waiting_input' ELSE 'queued' END,NEW.status,'download',NEW.id,NEW.created_at,NEW.updated_at);
END;
CREATE TRIGGER download_jobs_task_update AFTER UPDATE ON download_jobs BEGIN
 UPDATE tasks SET status=CASE NEW.status WHEN 'done' THEN 'completed' WHEN 'failed' THEN 'failed' WHEN 'cancelled' THEN 'cancelled' WHEN 'waiting' THEN 'waiting_input' WHEN 'paused' THEN 'waiting_input' WHEN 'downloading' THEN 'running' WHEN 'importing' THEN 'running' ELSE 'queued' END,
 phase=NEW.status,progress=CASE WHEN NEW.selected_size>0 THEN MIN(100.0,NEW.completed_size*100.0/NEW.selected_size) ELSE progress END,speed=CASE WHEN NEW.status='importing' THEN NEW.import_speed ELSE NEW.download_speed END,error=NEW.error,
 started_at=CASE WHEN NEW.status IN ('downloading','importing') THEN COALESCE(started_at,NEW.updated_at) ELSE started_at END,finished_at=CASE WHEN NEW.status IN ('done','failed','cancelled') THEN NEW.updated_at ELSE NULL END,heartbeat_at=CASE WHEN NEW.status IN ('downloading','importing') THEN NEW.updated_at ELSE heartbeat_at END,updated_at=NEW.updated_at WHERE source_type='download' AND source_id=NEW.id;
END;
CREATE TRIGGER download_jobs_task_delete AFTER DELETE ON download_jobs BEGIN DELETE FROM tasks WHERE source_type='download' AND source_id=OLD.id; END;

INSERT OR IGNORE INTO tasks(id,type,status,phase,progress,speed,error,source_type,source_id,created_at,finished_at,updated_at)
SELECT id,'url_download',CASE status WHEN 'done' THEN 'completed' WHEN 'failed' THEN 'failed' WHEN 'cancelled' THEN 'cancelled' WHEN 'paused' THEN 'waiting_input' WHEN 'downloading' THEN 'running' ELSE 'queued' END,status,CASE WHEN selected_size>0 THEN MIN(100.0,completed_size*100.0/selected_size) ELSE 0 END,download_speed,error,'url_download',id,created_at,CASE WHEN status IN ('done','failed','cancelled') THEN updated_at END,updated_at FROM url_download_jobs;
CREATE TRIGGER url_download_jobs_task_insert AFTER INSERT ON url_download_jobs BEGIN
 INSERT OR IGNORE INTO tasks(id,type,status,phase,source_type,source_id,created_at,updated_at) VALUES(NEW.id,'url_download','queued',NEW.status,'url_download',NEW.id,NEW.created_at,NEW.updated_at);
END;
CREATE TRIGGER url_download_jobs_task_update AFTER UPDATE ON url_download_jobs BEGIN
 UPDATE tasks SET status=CASE NEW.status WHEN 'done' THEN 'completed' WHEN 'failed' THEN 'failed' WHEN 'cancelled' THEN 'cancelled' WHEN 'paused' THEN 'waiting_input' WHEN 'downloading' THEN 'running' ELSE 'queued' END,phase=NEW.status,
 progress=CASE WHEN NEW.selected_size>0 THEN MIN(100.0,NEW.completed_size*100.0/NEW.selected_size) ELSE progress END,speed=NEW.download_speed,error=NEW.error,started_at=CASE WHEN NEW.status='downloading' THEN COALESCE(started_at,NEW.updated_at) ELSE started_at END,finished_at=CASE WHEN NEW.status IN ('done','failed','cancelled') THEN NEW.updated_at ELSE NULL END,heartbeat_at=CASE WHEN NEW.status='downloading' THEN NEW.updated_at ELSE heartbeat_at END,updated_at=NEW.updated_at WHERE source_type='url_download' AND source_id=NEW.id;
END;
CREATE TRIGGER url_download_jobs_task_delete AFTER DELETE ON url_download_jobs BEGIN DELETE FROM tasks WHERE source_type='url_download' AND source_id=OLD.id; END;
