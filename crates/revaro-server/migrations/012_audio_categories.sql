-- Preserve previously created audio groups as ordinary classifications.
INSERT INTO library_collections(id,name,kind,created_at)
 SELECT id,name,'audio',created_at FROM book_stacks WHERE kind='audio';
INSERT INTO library_collection_items(collection_id,file_id,position)
 SELECT si.stack_id,si.file_id,si.position FROM book_stack_items si
 JOIN book_stacks s ON s.id=si.stack_id WHERE s.kind='audio';
DELETE FROM book_stacks WHERE kind='audio';
-- Keep the legacy discriminator for compatibility with an older process
-- during an upgrade. The application only exposes book stacks now.
