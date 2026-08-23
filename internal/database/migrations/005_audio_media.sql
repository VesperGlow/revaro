-- Audiobook-style metadata for merged audio. The downloadable master stays
-- in files; stream_object_key points to a browser-compatible AAC companion
-- (or to the master itself when the selected output is already AAC).

CREATE TABLE audio_media (
    file_id TEXT PRIMARY KEY,
    duration_ms INTEGER NOT NULL CHECK(duration_ms > 0),
    chapters_json TEXT NOT NULL,
    stream_object_key TEXT NOT NULL,
    stream_size INTEGER NOT NULL CHECK(stream_size > 0),
    stream_etag TEXT NOT NULL,
    has_cover INTEGER NOT NULL DEFAULT 0 CHECK(has_cover IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
);
