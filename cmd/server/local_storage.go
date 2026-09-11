package main

import (
	"context"
	"database/sql"
	"fmt"

	"github.com/VesperGlow/revaro/internal/storage"
)

// Refuse to start cleanup or serve an old S3 database before its blobs have
// been copied. A missing mount must not be mistaken for an empty installation.
func validateLocalFiles(ctx context.Context, db *sql.DB, store storage.Storage) error {
	rows, err := db.QueryContext(ctx, `SELECT object_key,size FROM files WHERE kind='file' AND status='ready' AND object_key IS NOT NULL AND object_key<>'' UNION SELECT stream_object_key,stream_size FROM audio_media WHERE stream_object_key<>''`)
	if err != nil {
		return err
	}
	defer rows.Close()
	for rows.Next() {
		var key string
		var size int64
		if err := rows.Scan(&key, &size); err != nil {
			return err
		}
		info, err := store.HeadObject(ctx, key)
		if err != nil {
			return fmt.Errorf("local file %q is missing: copy existing S3 objects to APP_DATA_DIR/objects before starting (see docs/local-storage-migration.md): %w", key, err)
		}
		if info.Size != size {
			return fmt.Errorf("local file %q has %d bytes, database expects %d", key, info.Size, size)
		}
	}
	return rows.Err()
}
