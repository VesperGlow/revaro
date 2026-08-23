-- Persistent built-in BitTorrent tasks. Verified torrent pieces are staged in
-- object storage and indexed here so downloads survive process restarts
-- without requiring enough local disk for the complete payload.

CREATE TABLE download_jobs (
    id TEXT PRIMARY KEY,
    parent_id TEXT NOT NULL,
    source_type TEXT NOT NULL CHECK(source_type IN ('magnet', 'torrent')),
    source TEXT NOT NULL,
    metainfo BLOB,
    info_hash TEXT,
    name TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL CHECK(status IN (
        'metadata', 'waiting', 'queued', 'downloading', 'paused',
        'importing', 'done', 'failed', 'cancelled'
    )),
    selected_size INTEGER NOT NULL DEFAULT 0 CHECK(selected_size >= 0),
    completed_size INTEGER NOT NULL DEFAULT 0 CHECK(completed_size >= 0),
    download_speed INTEGER NOT NULL DEFAULT 0 CHECK(download_speed >= 0),
    peers INTEGER NOT NULL DEFAULT 0 CHECK(peers >= 0),
    error TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE UNIQUE INDEX download_jobs_retained_hash
ON download_jobs(info_hash)
WHERE info_hash IS NOT NULL AND status NOT IN ('failed', 'cancelled');

CREATE INDEX download_jobs_status_idx ON download_jobs(status, updated_at);

CREATE TABLE download_files (
    job_id TEXT NOT NULL,
    file_index INTEGER NOT NULL CHECK(file_index >= 0),
    path TEXT NOT NULL,
    size INTEGER NOT NULL CHECK(size >= 0),
    selected INTEGER NOT NULL DEFAULT 0 CHECK(selected IN (0, 1)),
    PRIMARY KEY(job_id, file_index),
    FOREIGN KEY(job_id) REFERENCES download_jobs(id) ON DELETE CASCADE
);

CREATE TABLE download_pieces (
    info_hash TEXT NOT NULL,
    piece_index INTEGER NOT NULL CHECK(piece_index >= 0),
    size INTEGER NOT NULL CHECK(size > 0),
    object_key TEXT NOT NULL,
    completed_at TEXT NOT NULL,
    PRIMARY KEY(info_hash, piece_index)
);

CREATE INDEX download_pieces_hash_idx ON download_pieces(info_hash);
