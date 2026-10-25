-- Allow the production Galgame share table to reference the SiglusEngine player.
ALTER TABLE shares RENAME TO shares_legacy_siglus;
DROP INDEX IF EXISTS shares_token_idx;
DROP INDEX IF EXISTS shares_expiry;
DROP INDEX IF EXISTS shares_custom_slug_idx;

CREATE TABLE shares (
    file_id TEXT PRIMARY KEY,
    token TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    expires_at TEXT,
    custom_slug TEXT UNIQUE,
    share_type TEXT NOT NULL DEFAULT 'file' CHECK(share_type IN ('file','galgame')),
    engine TEXT CHECK(engine IS NULL OR engine IN ('kirikiri2','siglus')),
    entry TEXT,
    FOREIGN KEY(file_id) REFERENCES files(id) ON DELETE CASCADE
);

INSERT INTO shares(file_id,token,created_at,expires_at,custom_slug,share_type,engine,entry)
SELECT file_id,token,created_at,expires_at,custom_slug,share_type,engine,entry
FROM shares_legacy_siglus;

DROP TABLE shares_legacy_siglus;
CREATE UNIQUE INDEX shares_token_idx ON shares(token);
CREATE INDEX shares_expiry ON shares(expires_at);
CREATE UNIQUE INDEX shares_custom_slug_idx ON shares(custom_slug);
