-- Configuration belongs to the shared game, never to the bundled runtime.
ALTER TABLE shares ADD COLUMN key_config TEXT CHECK(key_config IS NULL OR length(key_config) <= 16384);
