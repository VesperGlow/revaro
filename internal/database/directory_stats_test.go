package database

import (
	"testing"
	"time"
)

func TestDirectoryStatsTrackDeepMoveTrashRestoreAndDelete(t *testing.T) {
	db, err := Open(t.TempDir() + "/stats.db")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	now := time.Now().UTC().Format(time.RFC3339Nano)
	const root = "00000000-0000-0000-0000-000000000000"
	for _, row := range []struct {
		id, parent, kind string
		size             int64
	}{
		{"a", root, "directory", 0}, {"b", root, "directory", 0}, {"deep", "a", "directory", 0}, {"file", "deep", "file", 42},
	} {
		key := any(nil)
		if row.kind == "file" {
			key = "blobs/file"
		}
		if _, err := db.Exec(`INSERT INTO files(id,parent_id,name,kind,object_key,size,status,created_at,updated_at) VALUES(?,?,?,?,?,?,'ready',?,?)`, row.id, row.parent, row.id, row.kind, key, row.size, now, now); err != nil {
			t.Fatal(err)
		}
	}
	check := func(id string, count, bytes int64) {
		t.Helper()
		var gotCount, gotBytes int64
		if err := db.QueryRow(`SELECT file_count,total_bytes FROM directory_stats WHERE directory_id=?`, id).Scan(&gotCount, &gotBytes); err != nil {
			t.Fatal(err)
		}
		if gotCount != count || gotBytes != bytes {
			t.Fatalf("stats %s=(%d,%d), want (%d,%d)", id, gotCount, gotBytes, count, bytes)
		}
	}
	check(root, 1, 42)
	check("a", 1, 42)
	check("deep", 1, 42)
	check("b", 0, 0)
	if _, err := db.Exec(`UPDATE files SET parent_id=? WHERE id='deep'`, "b"); err != nil {
		t.Fatal(err)
	}
	check("a", 0, 0)
	check("b", 1, 42)
	check(root, 1, 42)
	if _, err := db.Exec(`UPDATE files SET deleted_at=?,trash_root_id='deep' WHERE id IN ('deep','file')`, now); err != nil {
		t.Fatal(err)
	}
	check(root, 0, 0)
	check("b", 0, 0)
	if _, err := db.Exec(`UPDATE files SET deleted_at=NULL,trash_root_id=NULL WHERE id IN ('deep','file')`); err != nil {
		t.Fatal(err)
	}
	check(root, 1, 42)
	check("b", 1, 42)
	if _, err := db.Exec(`DELETE FROM files WHERE id='file'`); err != nil {
		t.Fatal(err)
	}
	check(root, 0, 0)
	check("deep", 0, 0)
}
