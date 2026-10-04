ALTER TABLE library_items ADD COLUMN series TEXT;
ALTER TABLE library_items ADD COLUMN series_index REAL;
ALTER TABLE library_items ADD COLUMN metadata_etag TEXT;
CREATE INDEX library_book_series ON library_items(series) WHERE kind='book';
CREATE TRIGGER library_book_metadata_update AFTER UPDATE OF name,etag,object_key ON files
BEGIN
 UPDATE library_items SET series=NULL,series_index=NULL,metadata_etag=NULL WHERE file_id=NEW.id AND kind='book';
END;
