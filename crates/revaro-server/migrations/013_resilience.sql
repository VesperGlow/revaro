-- Retry unreadable books without holding up other indexing work.
CREATE TABLE book_metadata_retries (
    file_id TEXT PRIMARY KEY REFERENCES library_items(file_id) ON DELETE CASCADE,
    source_etag TEXT NOT NULL,
    retry_at TEXT NOT NULL
);
CREATE INDEX files_content_order ON files(created_at DESC,id)
WHERE kind='file' AND status='ready' AND deleted_at IS NULL;
CREATE INDEX library_opened ON library_state(last_opened,file_id)
WHERE last_opened IS NOT NULL;
