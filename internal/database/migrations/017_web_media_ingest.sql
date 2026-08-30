ALTER TABLE download_jobs ADD COLUMN ingest_state TEXT NOT NULL DEFAULT 'queued'
    CHECK(ingest_state IN ('queued','probing','processing','uploading','completed','unsupported','failed','cancelled'));
CREATE TABLE web_media_ingests (
 download_job_id TEXT NOT NULL, file_index INTEGER NOT NULL, file_id TEXT UNIQUE,
 state TEXT NOT NULL CHECK(state IN ('queued','probing','processing','uploading','completed','unsupported','failed','cancelled')),
 video_codec TEXT NOT NULL DEFAULT '', audio_codec TEXT NOT NULL DEFAULT '', error TEXT NOT NULL DEFAULT '',
 created_at TEXT NOT NULL, updated_at TEXT NOT NULL, PRIMARY KEY(download_job_id,file_index),
 FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE, FOREIGN KEY(download_job_id) REFERENCES download_jobs(id) ON DELETE CASCADE);
CREATE TABLE web_media_playback (
 file_id TEXT PRIMARY KEY, object_key TEXT NOT NULL UNIQUE, size INTEGER NOT NULL CHECK(size>0), etag TEXT NOT NULL,
 mime_type TEXT NOT NULL DEFAULT 'video/mp4', duration_ms INTEGER NOT NULL DEFAULT 0 CHECK(duration_ms>=0),
 video_codec TEXT NOT NULL, audio_codec TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL,
 FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE);
CREATE TABLE web_media_subtitles (
 file_id TEXT NOT NULL, track_index INTEGER NOT NULL, object_key TEXT NOT NULL UNIQUE, size INTEGER NOT NULL CHECK(size>0), etag TEXT NOT NULL,
 language TEXT NOT NULL DEFAULT '', title TEXT NOT NULL DEFAULT '', is_default INTEGER NOT NULL DEFAULT 0, is_forced INTEGER NOT NULL DEFAULT 0,
 PRIMARY KEY(file_id,track_index), FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE);
