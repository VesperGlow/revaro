package storage

import (
	"context"
	"path/filepath"
	"strings"
	"testing"

	"github.com/VesperGlow/revaro/internal/database"
)

func TestManifestIndexPersistsOrderedBlocksAndOffsets(t *testing.T) {
	db, err := database.Open(filepath.Join(t.TempDir(), "revaro.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	index := newManifestIndex(db)
	m := Manifest{Version: 1, Size: 7, Blocks: []Block{
		{ID: strings.Repeat("01", 32), Size: 3},
		{ID: strings.Repeat("02", 32), Size: 4},
	}}
	if err := index.put(context.Background(), m.Key(), m); err != nil {
		t.Fatal(err)
	}
	got, ok, err := index.get(context.Background(), m.Key())
	if err != nil || !ok {
		t.Fatalf("get indexed manifest ok=%v err=%v", ok, err)
	}
	if got.ID() != m.ID() || len(got.Blocks) != 2 {
		t.Fatalf("indexed manifest=%+v", got)
	}
	var offset int64
	if err := db.QueryRow(`SELECT block_offset FROM storage_manifest_blocks WHERE manifest_key=? AND seq=1`, m.Key()).Scan(&offset); err != nil {
		t.Fatal(err)
	}
	if offset != 3 {
		t.Fatalf("second block offset=%d, want 3", offset)
	}
	if err := index.delete(context.Background(), m.Key()); err != nil {
		t.Fatal(err)
	}
	if _, ok, err := index.get(context.Background(), m.Key()); err != nil || ok {
		t.Fatalf("deleted manifest remained ok=%v err=%v", ok, err)
	}
}
