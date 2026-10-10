ALTER TABLE media_progress ADD COLUMN revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0);
ALTER TABLE media_progress ADD COLUMN writer TEXT;
ALTER TABLE media_progress ADD COLUMN sequence INTEGER NOT NULL DEFAULT 0 CHECK (sequence >= 0);
ALTER TABLE media_progress ADD COLUMN completed INTEGER NOT NULL DEFAULT 0 CHECK (completed IN (0, 1));
