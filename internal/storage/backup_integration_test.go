package storage

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/dataplane"
)

// Exercises the actual Rust AWS SDK against an isolated S3 protocol fixture.
// Enable with REVARO_TEST_DATA_PLANE=/absolute/path/to/revaro-data-plane.
func TestRustDatabaseBackupTransport(t *testing.T) {
	binary := os.Getenv("REVARO_TEST_DATA_PLANE")
	if binary == "" {
		t.Skip("Rust binary not configured")
	}
	key := "revaro-backups/database/revaro-db-20260912T000000Z.sqlite"
	var mu sync.Mutex
	var uploaded []byte
	deleted := false
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		defer mu.Unlock()
		switch {
		case r.Method == "PUT" && r.URL.Path == "/backup-test/"+key:
			uploaded, _ = io.ReadAll(r.Body)
			w.Header().Set("ETag", "etag")
			w.WriteHeader(200)
		case r.Method == "GET" && r.URL.Query().Get("prefix") == "revaro-backups/database/":
			w.Header().Set("Content-Type", "application/xml")
			fmt.Fprintf(w, "<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>%s</Key><Size>32</Size></Contents></ListBucketResult>", key)
		case r.Method == "POST" && r.URL.Query().Has("delete"):
			data, _ := io.ReadAll(r.Body)
			deleted = strings.Contains(string(data), key)
			w.Header().Set("Content-Type", "application/xml")
			fmt.Fprint(w, "<DeleteResult/>")
		default:
			t.Errorf("unexpected S3 request: %s %s", r.Method, r.URL)
			http.Error(w, "unexpected request", 400)
		}
	}))
	defer upstream.Close()
	for k, v := range map[string]string{"APP_DATA_DIR": t.TempDir(), "APP_WORK_DIR": t.TempDir(), "BACKUP_ENABLED": "true", "S3_ENDPOINT": upstream.URL, "S3_REGION": "us-east-1", "S3_BUCKET": "backup-test", "S3_ACCESS_KEY": "test-access", "S3_SECRET_KEY": "test-secret", "S3_PATH_STYLE": "true"} {
		t.Setenv(k, v)
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	addr := listener.Addr().String()
	listener.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	process, err := dataplane.Start(ctx, binary, addr, slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatal(err)
	}
	defer process.Close()
	client := NewDataPlane(process.Addr(), process.Token())
	payload := []byte("SQLite format 3\x00backup-fixture")
	if err := client.UploadDatabase(ctx, key, bytes.NewReader(payload), int64(len(payload))); err != nil {
		t.Fatal(err)
	}
	mu.Lock()
	got := append([]byte(nil), uploaded...)
	mu.Unlock()
	if !bytes.Contains(got, payload) {
		t.Fatalf("uploaded snapshot missing: %q", got)
	}
	refs, err := client.ListDatabases(ctx)
	if err != nil || len(refs) != 1 || refs[0].Key != key {
		t.Fatalf("list=%+v %v", refs, err)
	}
	if err := client.DeleteDatabases(ctx, []string{key}); err != nil {
		t.Fatal(err)
	}
	mu.Lock()
	removed := deleted
	mu.Unlock()
	if !removed {
		t.Fatal("retention did not delete snapshot")
	}
	if err := client.UploadDatabase(ctx, "blobs/forbidden", bytes.NewReader(payload), int64(len(payload))); err == nil {
		t.Fatal("ordinary S3 file write accepted")
	}
	if err := client.DeleteDatabases(ctx, []string{"profile/avatar"}); err == nil {
		t.Fatal("ordinary S3 file deletion accepted")
	}
}
