-- Point lookups during foreign-key cascades must not scan all shelf members.
CREATE INDEX library_collection_file ON library_collection_items(file_id,collection_id);
-- The mixed directory-first / descending sort needs matching index directions.
CREATE INDEX files_directory_updated ON files(parent_id,kind,updated_at DESC,id)
 WHERE deleted_at IS NULL;
CREATE INDEX files_directory_size ON files(parent_id,kind,size DESC,id)
 WHERE deleted_at IS NULL;
