-- Existing S3 multipart sessions cannot be resumed against local disk.
-- Completed files retain their object keys for an exact-key migration.
UPDATE tasks SET status='cancelled',phase='storage_migrated',finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'),updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE source_type='upload' AND source_id IN (SELECT id FROM uploads WHERE status='pending');
DELETE FROM files WHERE status='pending' AND id IN (SELECT file_id FROM uploads WHERE status='pending');
ALTER TABLE uploads RENAME COLUMN s3_upload_id TO multipart_id;
UPDATE tasks SET status='cancelled',phase='feature_removed',finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now'),updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE type IN ('audio_merge','bt','url_download','video_hls','audio_hls','video_fmp4') AND status NOT IN ('completed','failed','cancelled');
