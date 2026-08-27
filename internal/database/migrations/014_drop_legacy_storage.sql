-- FastCDC manifests and blocks were retired after all supported releases
-- switched to one opaque blobs/<UUID> object per logical file.
DROP INDEX IF EXISTS storage_manifest_blocks_block_id_idx;
DROP TABLE IF EXISTS storage_manifest_blocks;
DROP TABLE IF EXISTS storage_manifests;
