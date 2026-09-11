package main

import (
	"context"
	"database/sql"
	"github.com/VesperGlow/revaro/internal/storage"
	"strings"
	"testing"
)

func TestLocalStartupRequiresMigratedFiles(t *testing.T) {
	ctx := context.Background()
	db, err := sql.Open("sqlite", ":memory:")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	for _, query := range []string{
		"CREATE TABLE files (object_key TEXT,size INTEGER,kind TEXT,status TEXT)",
		"CREATE TABLE audio_media (stream_object_key TEXT,stream_size INTEGER)",
		"INSERT INTO files VALUES('blobs/existing',3,'file','ready')",
	} {
		if _, err := db.Exec(query); err != nil {
			t.Fatal(err)
		}
	}
	local, err := storage.NewLocal(t.TempDir(), nil)
	if err != nil {
		t.Fatal(err)
	}
	defer local.Close()
	if err := validateLocalFiles(ctx, db, local); err == nil || !strings.Contains(err.Error(), "blobs/existing") {
		t.Fatalf("missing migration not reported: %v", err)
	}
	if _, err := local.PutObject(ctx, "blobs/existing", "", []byte("abc")); err != nil {
		t.Fatal(err)
	}
	if err := validateLocalFiles(ctx, db, local); err != nil {
		t.Fatalf("migrated file rejected: %v", err)
	}
	if _, err := db.Exec("UPDATE files SET size=4"); err != nil {
		t.Fatal(err)
	}
	if err := validateLocalFiles(ctx, db, local); err == nil {
		t.Fatal("truncated migration accepted")
	}
}
