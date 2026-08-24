-- Keep the object-storage import phase observable. Torrent bytes may already
-- be complete while FastCDC is still hashing, deduplicating, and uploading
-- blocks, so this phase needs its own counters instead of reusing download
-- progress (which is already at 100%).

ALTER TABLE download_jobs ADD COLUMN imported_size INTEGER NOT NULL DEFAULT 0 CHECK(imported_size >= 0);
ALTER TABLE download_jobs ADD COLUMN import_speed INTEGER NOT NULL DEFAULT 0 CHECK(import_speed >= 0);
ALTER TABLE download_jobs ADD COLUMN current_file TEXT NOT NULL DEFAULT '';
