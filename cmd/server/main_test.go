package main

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/VesperGlow/revaro/internal/auth"
)

func TestWriteCredentialsUsesPrivateFile(t *testing.T) {
	dir := t.TempDir()
	path, err := writeCredentials(dir, auth.InitialCredentials{Username: "admin", Password: "secret-value"})
	if err != nil {
		t.Fatal(err)
	}
	if path != filepath.Join(dir, "initial-admin-credentials") {
		t.Fatalf("path = %q", path)
	}
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm() != 0o600 {
		t.Fatalf("mode = %o", info.Mode().Perm())
	}
}

func TestEnsureWorkDirChecksWrites(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "work")
	if err := ensureWorkDir(dir); err != nil {
		t.Fatal(err)
	}
	entries, err := os.ReadDir(dir)
	if err != nil || len(entries) != 0 {
		t.Fatalf("probe leaked entries: %v, %v", entries, err)
	}
}
