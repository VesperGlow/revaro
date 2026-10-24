ALTER TABLE shares ADD COLUMN custom_slug TEXT;
CREATE UNIQUE INDEX shares_custom_slug_idx ON shares(custom_slug);
