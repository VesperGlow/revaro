package storage

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"time"
)

// manifestIndex is the online copy of S3 manifests. files.object_key points
// at manifest_key, so the two migration tables form the persistent
// file-to-block mapping while the immutable JSON object remains the recovery
// source of truth in S3.
type manifestIndex struct {
	db *sql.DB
}

func newManifestIndex(db *sql.DB) *manifestIndex {
	if db == nil {
		return nil
	}
	return &manifestIndex{db: db}
}

func (i *manifestIndex) get(ctx context.Context, key string) (Manifest, bool, error) {
	if i == nil {
		return Manifest{}, false, nil
	}
	tx, err := i.db.BeginTx(ctx, &sql.TxOptions{ReadOnly: true})
	if err != nil {
		return Manifest{}, false, err
	}
	defer tx.Rollback()
	var m Manifest
	if err := tx.QueryRowContext(ctx, `SELECT version,size FROM storage_manifests WHERE manifest_key=?`, key).Scan(&m.Version, &m.Size); err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return Manifest{}, false, nil
		}
		return Manifest{}, false, err
	}
	rows, err := tx.QueryContext(ctx, `SELECT block_id,size,block_offset FROM storage_manifest_blocks WHERE manifest_key=? ORDER BY seq`, key)
	if err != nil {
		return Manifest{}, false, err
	}
	defer rows.Close()
	var expectedOffset int64
	for rows.Next() {
		var block Block
		var offset int64
		if err := rows.Scan(&block.ID, &block.Size, &offset); err != nil {
			return Manifest{}, false, err
		}
		if offset != expectedOffset {
			return Manifest{}, false, fmt.Errorf("manifest %s local block offset %d, want %d", key, offset, expectedOffset)
		}
		expectedOffset += block.Size
		m.Blocks = append(m.Blocks, block)
	}
	if err := rows.Err(); err != nil {
		return Manifest{}, false, err
	}
	if err := validateManifest(m); err != nil || key != m.Key() {
		if err == nil {
			err = errors.New("content hash does not match key")
		}
		return Manifest{}, false, fmt.Errorf("manifest %s local index is invalid: %w", key, err)
	}
	if err := tx.Commit(); err != nil {
		return Manifest{}, false, err
	}
	return m, true, nil
}

func (i *manifestIndex) put(ctx context.Context, key string, m Manifest) error {
	if i == nil {
		return nil
	}
	if err := validateManifest(m); err != nil {
		return err
	}
	if key != m.Key() {
		return fmt.Errorf("manifest %s content hash does not match its key", key)
	}
	tx, err := i.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if _, err := tx.ExecContext(ctx, `INSERT INTO storage_manifests(manifest_key,version,size,updated_at) VALUES(?,?,?,?) ON CONFLICT(manifest_key) DO UPDATE SET version=excluded.version,size=excluded.size,updated_at=excluded.updated_at`, key, m.Version, m.Size, time.Now().UTC().Format(time.RFC3339Nano)); err != nil {
		return err
	}
	if _, err := tx.ExecContext(ctx, `DELETE FROM storage_manifest_blocks WHERE manifest_key=?`, key); err != nil {
		return err
	}
	insertBlock, err := tx.PrepareContext(ctx, `INSERT INTO storage_manifest_blocks(manifest_key,seq,block_id,size,block_offset) VALUES(?,?,?,?,?)`)
	if err != nil {
		return err
	}
	defer insertBlock.Close()
	var offset int64
	for seq, block := range m.Blocks {
		if _, err := insertBlock.ExecContext(ctx, key, seq, block.ID, block.Size, offset); err != nil {
			return err
		}
		offset += block.Size
	}
	return tx.Commit()
}

func (i *manifestIndex) delete(ctx context.Context, key string) error {
	if i == nil || !IsManifestKey(key) {
		return nil
	}
	_, err := i.db.ExecContext(ctx, `DELETE FROM storage_manifests WHERE manifest_key=?`, key)
	return err
}
