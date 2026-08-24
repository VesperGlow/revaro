CREATE TABLE media_progress (
    file_id TEXT PRIMARY KEY,
    position_ms INTEGER NOT NULL CHECK(position_ms >= 0),
    duration_ms INTEGER NOT NULL DEFAULT 0 CHECK(duration_ms >= 0),
    updated_at TEXT NOT NULL,
    FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
);

