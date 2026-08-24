-- Direct HTTP(S) downloads live beside torrent jobs instead of widening the
-- original download_jobs CHECK constraint. This keeps the existing torrent
-- foreign keys and restart data intact on SQLite upgrades.

CREATE TABLE url_download_jobs (
    id TEXT PRIMARY KEY,
    parent_id TEXT NOT NULL,
    source_url TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL CHECK(status IN (
        'queued', 'downloading', 'paused', 'done', 'failed', 'cancelled'
    )),
    selected_size INTEGER NOT NULL DEFAULT 0 CHECK(selected_size >= 0),
    completed_size INTEGER NOT NULL DEFAULT 0 CHECK(completed_size >= 0),
    download_speed INTEGER NOT NULL DEFAULT 0 CHECK(download_speed >= 0),
    error TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX url_download_jobs_status_idx ON url_download_jobs(status, updated_at);
