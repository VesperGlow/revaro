-- Explicit UI grouping only. No series or shelf data is migrated into stacks.
CREATE TABLE book_stacks (
 id TEXT PRIMARY KEY,
 name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 80),
 created_at TEXT NOT NULL
);
CREATE TABLE book_stack_items (
 stack_id TEXT NOT NULL REFERENCES book_stacks(id) ON DELETE CASCADE,
 file_id TEXT NOT NULL UNIQUE REFERENCES files(id) ON DELETE CASCADE,
 position INTEGER NOT NULL CHECK(position >= 0),
 PRIMARY KEY(stack_id,file_id)
);
CREATE INDEX book_stack_order ON book_stack_items(stack_id,position,file_id);
