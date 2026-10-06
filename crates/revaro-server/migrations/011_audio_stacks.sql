-- Reuse manual stack membership/order; existing stacks remain books.
ALTER TABLE book_stacks ADD COLUMN kind TEXT NOT NULL DEFAULT 'book'
 CHECK(kind IN ('book','audio'));
