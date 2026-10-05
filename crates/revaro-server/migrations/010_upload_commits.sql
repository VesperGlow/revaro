-- Keep the public pending/completed lifecycle compatible with existing clients.
-- A durable commit phase freezes uploaded bytes before filesystem publication.
ALTER TABLE uploads ADD COLUMN commit_state TEXT NOT NULL DEFAULT 'receiving'
    CHECK(commit_state IN ('receiving','assembling','staged'));
ALTER TABLE uploads ADD COLUMN staged_etag TEXT;
ALTER TABLE uploads ADD COLUMN idempotency_key TEXT;
ALTER TABLE uploads ADD COLUMN request_fingerprint TEXT;
ALTER TABLE uploads ADD COLUMN staging_cleaned INTEGER NOT NULL DEFAULT 0
    CHECK(staging_cleaned IN (0,1));
-- Rotate recovery batches so persistent failures cannot starve later uploads.
ALTER TABLE uploads ADD COLUMN commit_attempted_at TEXT;
CREATE UNIQUE INDEX uploads_idempotency_key ON uploads(idempotency_key)
    WHERE idempotency_key IS NOT NULL;
CREATE INDEX uploads_commit_recovery ON uploads(status,commit_state);
