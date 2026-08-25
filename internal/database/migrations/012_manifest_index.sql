-- Persistent online index for content-addressed manifests. files.object_key
-- points at manifest_key, so this pair of tables provides the local
-- file -> ordered blocks mapping without changing the S3 manifest format.
-- Existing rows are filled lazily (and by the normal startup GC pass) from
-- the immutable Wasabi manifest objects.

CREATE TABLE storage_manifests (
    manifest_key TEXT PRIMARY KEY,
    version INTEGER NOT NULL CHECK(version = 1),
    size INTEGER NOT NULL CHECK(size >= 0),
    updated_at TEXT NOT NULL
);

CREATE TABLE storage_manifest_blocks (
    manifest_key TEXT NOT NULL,
    seq INTEGER NOT NULL CHECK(seq >= 0),
    block_id TEXT NOT NULL,
    size INTEGER NOT NULL CHECK(size > 0),
    block_offset INTEGER NOT NULL CHECK(block_offset >= 0),
    PRIMARY KEY(manifest_key, seq),
    FOREIGN KEY(manifest_key) REFERENCES storage_manifests(manifest_key) ON DELETE CASCADE
);

CREATE INDEX storage_manifest_blocks_block_id_idx
ON storage_manifest_blocks(block_id);
