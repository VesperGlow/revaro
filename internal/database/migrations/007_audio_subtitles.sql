-- Parsed subtitle cues for merged M4A playback. The timed-text stream remains
-- embedded in the M4A master; this JSON lets the web player render it without
-- relying on inconsistent browser support for text tracks in audio-only MP4.

ALTER TABLE audio_media ADD COLUMN subtitles_json TEXT NOT NULL DEFAULT '[]';
