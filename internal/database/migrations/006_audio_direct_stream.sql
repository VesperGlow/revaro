-- Existing installs may have an AAC playback companion from migration 005.
-- Point playback metadata back to the downloadable master; the old companion
-- becomes unreachable and is reclaimed later by the normal storage GC.

UPDATE audio_media
SET stream_object_key = (SELECT files.object_key FROM files WHERE files.id = audio_media.file_id),
    stream_size = (SELECT files.size FROM files WHERE files.id = audio_media.file_id),
    stream_etag = (SELECT files.etag FROM files WHERE files.id = audio_media.file_id),
    updated_at = (SELECT files.updated_at FROM files WHERE files.id = audio_media.file_id)
WHERE EXISTS (
    SELECT 1 FROM files
    WHERE files.id = audio_media.file_id
      AND files.object_key IS NOT NULL
      AND files.object_key <> ''
);
