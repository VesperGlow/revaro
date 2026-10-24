-- Existing shares retain the ordinary file transport and response shape.
ALTER TABLE shares ADD COLUMN share_type TEXT NOT NULL DEFAULT 'file' CHECK(share_type IN ('file','galgame'));
ALTER TABLE shares ADD COLUMN engine TEXT CHECK(engine IS NULL OR engine = 'kirikiri2');
ALTER TABLE shares ADD COLUMN entry TEXT;
ALTER TABLE shares ADD COLUMN password_hash TEXT;
