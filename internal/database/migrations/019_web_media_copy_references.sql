-- Copies own their playback/track associations, while immutable S3 assets may
-- have multiple file references, just like files.object_key and audio_media.
-- Keep file-scoped keys and cascades; an ingest remains source-job provenance.
CREATE TABLE web_media_playback_copy (
 file_id TEXT PRIMARY KEY, object_key TEXT NOT NULL, size INTEGER NOT NULL CHECK(size>0), etag TEXT NOT NULL,
 mime_type TEXT NOT NULL DEFAULT 'video/mp4', duration_ms INTEGER NOT NULL DEFAULT 0 CHECK(duration_ms>=0),
 video_codec TEXT NOT NULL, audio_codec TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL,
 FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE);
INSERT INTO web_media_playback_copy(file_id,object_key,size,etag,mime_type,duration_ms,video_codec,audio_codec,created_at)
 SELECT file_id,object_key,size,etag,mime_type,duration_ms,video_codec,audio_codec,created_at FROM web_media_playback;
DROP TABLE web_media_playback;
ALTER TABLE web_media_playback_copy RENAME TO web_media_playback;

CREATE TABLE web_media_subtitles_copy (
 file_id TEXT NOT NULL, track_index INTEGER NOT NULL, object_key TEXT NOT NULL, size INTEGER NOT NULL CHECK(size>0), etag TEXT NOT NULL,
 language TEXT NOT NULL DEFAULT '', title TEXT NOT NULL DEFAULT '', is_default INTEGER NOT NULL DEFAULT 0, is_forced INTEGER NOT NULL DEFAULT 0,
 PRIMARY KEY(file_id,track_index), FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE);
INSERT INTO web_media_subtitles_copy(file_id,track_index,object_key,size,etag,language,title,is_default,is_forced)
 SELECT file_id,track_index,object_key,size,etag,language,title,is_default,is_forced FROM web_media_subtitles;
DROP TABLE web_media_subtitles;
ALTER TABLE web_media_subtitles_copy RENAME TO web_media_subtitles;
