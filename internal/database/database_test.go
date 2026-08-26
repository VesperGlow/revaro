package database

import (
	"os"
	"path/filepath"
	"testing"
)

func TestOpenCreatesSchemaAndMigrations(t *testing.T) {
	dir := t.TempDir()
	db, err := Open(filepath.Join(dir, "revaro.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	var tables int
	if err := db.QueryRow(`SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('files','uploads','sessions','settings','shares','schema_migrations')`).Scan(&tables); err != nil {
		t.Fatal(err)
	}
	if tables != 6 {
		t.Fatalf("expected 6 core tables, got %d", tables)
	}
	var manifestTables int
	if err := db.QueryRow(`SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('storage_manifests','storage_manifest_blocks')`).Scan(&manifestTables); err != nil {
		t.Fatal(err)
	}
	if manifestTables != 2 {
		t.Fatalf("expected persistent manifest index tables, got %d", manifestTables)
	}
	var blobTables int
	if err := db.QueryRow(`SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='media_metadata'`).Scan(&blobTables); err != nil {
		t.Fatal(err)
	}
	if blobTables != 1 {
		t.Fatalf("expected media metadata table, got %d", blobTables)
	}
	// 迁移记录已写入
	var versions int
	if err := db.QueryRow(`SELECT COUNT(*) FROM schema_migrations`).Scan(&versions); err != nil {
		t.Fatal(err)
	}
	if versions < 4 {
		t.Fatalf("expected at least 4 migrations, got %d", versions)
	}
	// root 行存在
	var roots int
	if err := db.QueryRow(`SELECT COUNT(*) FROM files WHERE id='00000000-0000-0000-0000-000000000000'`).Scan(&roots); err != nil {
		t.Fatal(err)
	}
	if roots != 1 {
		t.Fatalf("root row count=%d", roots)
	}
	// 外键与 WAL 生效
	var fk int
	var journal string
	if err := db.QueryRow(`PRAGMA foreign_keys`).Scan(&fk); err != nil {
		t.Fatal(err)
	}
	if err := db.QueryRow(`PRAGMA journal_mode`).Scan(&journal); err != nil {
		t.Fatal(err)
	}
	if fk != 1 {
		t.Fatal("foreign_keys pragma not enabled")
	}
	if journal != "wal" {
		t.Fatalf("journal_mode=%q, want wal", journal)
	}
}

func TestOpenIsIdempotent(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "revaro.db")
	db1, err := Open(path)
	if err != nil {
		t.Fatal(err)
	}
	var initialVersions int
	if err := db1.QueryRow(`SELECT COUNT(*) FROM schema_migrations`).Scan(&initialVersions); err != nil {
		db1.Close()
		t.Fatal(err)
	}
	db1.Close()
	// 重复打开（模拟重启）：迁移不重复应用
	db2, err := Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer db2.Close()
	var versions int
	if err := db2.QueryRow(`SELECT COUNT(*) FROM schema_migrations`).Scan(&versions); err != nil {
		t.Fatal(err)
	}
	if versions != initialVersions {
		t.Fatalf("migrations reapplied on reopen: before=%d after=%d", initialVersions, versions)
	}
}

func TestOpenCreatesDataDirectory(t *testing.T) {
	dir := t.TempDir()
	nested := filepath.Join(dir, "a", "b")
	db, err := Open(filepath.Join(nested, "revaro.db"))
	if err != nil {
		t.Fatal(err)
	}
	db.Close()
	if _, err := os.Stat(filepath.Join(nested, "revaro.db")); err != nil {
		t.Fatal("nested data directory was not created")
	}
}

func TestAudioDirectStreamMigrationRepointsExistingCompanion(t *testing.T) {
	db, err := Open(filepath.Join(t.TempDir(), "revaro.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	const fileID = "11111111-1111-1111-1111-111111111111"
	if _, err := db.Exec(`INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)`,
		fileID, "00000000-0000-0000-0000-000000000000", "lossless.m4a", "file", "manifests/master.json", 1234, "audio/mp4", "master-etag", "ready", "2026-08-23T00:00:00Z", "2026-08-23T00:00:01Z"); err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec(`INSERT INTO audio_media(file_id,duration_ms,chapters_json,stream_object_key,stream_size,stream_etag,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?)`,
		fileID, 1000, `[]`, "manifests/aac-companion.json", 456, "aac-etag", "2026-08-23T00:00:00Z", "2026-08-23T00:00:00Z"); err != nil {
		t.Fatal(err)
	}
	body, err := migrations.ReadFile("migrations/006_audio_direct_stream.sql")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := db.Exec(string(body)); err != nil {
		t.Fatal(err)
	}
	var key, etag string
	var size int64
	if err := db.QueryRow(`SELECT stream_object_key,stream_size,stream_etag FROM audio_media WHERE file_id=?`, fileID).Scan(&key, &size, &etag); err != nil {
		t.Fatal(err)
	}
	if key != "manifests/master.json" || size != 1234 || etag != "master-etag" {
		t.Fatalf("direct stream=(%q,%d,%q)", key, size, etag)
	}
}
