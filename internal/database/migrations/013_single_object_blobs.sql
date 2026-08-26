-- New files are opaque single S3 objects. FastCDC manifests remain readable
-- during the rolling background migration, but are no longer an upload format.

DELETE FROM uploads WHERE status = 'pending';
DELETE FROM files
WHERE kind = 'file' AND status = 'pending' AND object_key IS NULL
  AND NOT EXISTS (SELECT 1 FROM audio_media WHERE audio_media.file_id = files.id);

ALTER TABLE uploads RENAME TO uploads_fastcdc_old;

CREATE TABLE uploads (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL,
    mode TEXT NOT NULL CHECK(mode IN ('single', 'multipart')),
    object_key TEXT NOT NULL,
    s3_upload_id TEXT,
    part_size INTEGER NOT NULL CHECK(part_size > 0),
    expected_size INTEGER NOT NULL CHECK(expected_size >= 0),
    mime_type TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending', 'completed', 'aborted', 'failed')),
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE,
    CHECK((mode = 'single' AND s3_upload_id IS NULL) OR
          (mode = 'multipart' AND s3_upload_id IS NOT NULL))
);

DROP TABLE uploads_fastcdc_old;
CREATE INDEX uploads_pending_idx ON uploads(status, expires_at);

-- Probe results are independent of presentation and survive server restarts.
-- chapters_json uses the same start_ms/end_ms/title representation as
-- audio_media so ordinary uploaded M4A/ALAC files can expose their chapters.
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
    FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
);
