-- Probe output evolves independently of the source object. Version cached rows
-- so newly discovered stream metadata is not hidden by an older result.
ALTER TABLE media_metadata ADD COLUMN probe_version INTEGER NOT NULL DEFAULT 0;
