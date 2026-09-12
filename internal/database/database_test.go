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
	var retiredTables int
	if err := db.QueryRow(`SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('audio_media','download_jobs','download_files','download_pieces','url_download_jobs','web_media_ingests','web_media_playback','web_media_subtitles')`).Scan(&retiredTables); err != nil {
		t.Fatal(err)
	}
	if retiredTables != 0 {
		t.Fatalf("retired processing tables created: %d", retiredTables)
	}
	var manifestTables int
	if err := db.QueryRow(`SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('storage_manifests','storage_manifest_blocks')`).Scan(&manifestTables); err != nil {
		t.Fatal(err)
	}
	if manifestTables != 0 {
		t.Fatalf("legacy manifest index tables remain: %d", manifestTables)
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
	if versions != 2 {
		t.Fatalf("expected initial schema and cleanup migration, got %d", versions)
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
