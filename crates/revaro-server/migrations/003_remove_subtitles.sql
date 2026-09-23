-- Subtitle conversion and playback were removed. Keep existing media rows.
ALTER TABLE media_metadata DROP COLUMN subtitles_json;
