package server

import (
	"bytes"
	"context"
	"errors"
	"net/http"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/storage"
)

func localTestApp(t *testing.T) (*testApp, *storage.Local) {
	a := newTestApp(t)
	a.srv.Close()
	local, err := storage.NewLocal(filepath.Join(a.srv.cfg.DataDir, "objects"), nil)
	if err != nil {
		t.Fatal(err)
	}
	a.srv = New(a.db, local, a.srv.auth, a.srv.cfg, nil)
	a.handler = a.srv.Handler()
	// Ensure server workers stop before closing their store.
	t.Cleanup(func() { a.srv.Close(); local.Close() })
	return a, local
}

func TestLocalUploadPlaybackShareAndTrash(t *testing.T) {
	a, local := localTestApp(t)
	a.srv.cleanup.Close()
	u := a.createUpload(t, "local.txt", 6)
	if u.URL != "/api/uploads/"+u.UploadID+"/data" {
		t.Fatalf("URL=%q", u.URL)
	}
	if r := a.requestRaw("PUT", u.URL, []byte("abcdef"), false); r.Code != 401 {
		t.Fatalf("unauthenticated upload=%d", r.Code)
	}
	if r := a.requestRaw("PUT", u.URL, []byte("bad"), true); r.Code != 400 {
		t.Fatalf("short body=%d", r.Code)
	}
	if r := a.requestRaw("PUT", u.URL, []byte("abcdef"), true); r.Code != 204 {
		t.Fatalf("upload=%d %s", r.Code, r.Body.String())
	}
	done := a.request("POST", "/api/uploads/"+u.UploadID+"/complete", map[string]any{"parts": []any{}}, true)
	if done.Code != 200 {
		t.Fatalf("complete=%d %s", done.Code, done.Body.String())
	}
	record, err := a.srv.file(context.Background(), u.FileID)
	if err != nil {
		t.Fatal(err)
	}
	if record.ContentHash != sha256hex([]byte("abcdef")) {
		t.Fatal("missing integrity hash")
	}
	if r := a.requestRaw("PUT", u.URL, []byte("change"), true); r.Code != 404 {
		t.Fatalf("completed upload writable=%d", r.Code)
	}
	r := a.requestH("GET", "/api/files/"+u.FileID+"/download", nil, true, map[string]string{"Range": "bytes=2-4"})
	if r.Code != 206 || r.Body.String() != "cde" || r.Header().Get("Content-Range") != "bytes 2-4/6" {
		t.Fatalf("range=%d %q %v", r.Code, r.Body.String(), r.Header())
	}
	if r := a.requestH("GET", "/api/files/"+u.FileID+"/download", nil, true, map[string]string{"Range": "bytes=99-"}); r.Code != 416 {
		t.Fatalf("invalid range=%d", r.Code)
	}
	shared := a.request("POST", "/api/files/"+u.FileID+"/share", nil, true)
	share := decode[struct {
		URL string `json:"url"`
	}](t, shared)
	r = a.request("GET", share.URL, nil, false)
	if r.Code != 200 || r.Body.String() != "abcdef" {
		t.Fatalf("share=%d %q", r.Code, r.Body.String())
	}
	if r := a.request("DELETE", "/api/files/"+u.FileID, nil, true); r.Code != 204 {
		t.Fatalf("trash=%d", r.Code)
	}
	if _, err := local.HeadObject(context.Background(), record.objectKey); err != nil {
		t.Fatal("trash lost blob", err)
	}
	if r := a.request("POST", "/api/trash/"+u.FileID+"/restore", nil, true); r.Code != 204 {
		t.Fatalf("restore=%d %s", r.Code, r.Body.String())
	}
	a.request("DELETE", "/api/files/"+u.FileID, nil, true)
	a.request("DELETE", "/api/trash/"+u.FileID, nil, true)
	a.srv.CleanupObjects(context.Background())
	// GC has a grace period; age only this test's unreferenced blob.
	old := time.Now().Add(-48 * time.Hour)
	path := filepath.Join(a.srv.cfg.DataDir, "objects", record.objectKey)
	if err := os.Chtimes(path, old, old); err != nil && !errors.Is(err, os.ErrNotExist) {
		t.Fatal(err)
	}
	a.srv.CollectGarbage(context.Background())
	if _, err := local.HeadObject(context.Background(), record.objectKey); !storage.IsNotFound(err) {
		t.Fatalf("purged blob retained: %v", err)
	}
}

func TestLocalMultipartHTTPResume(t *testing.T) {
	a, _ := localTestApp(t)
	size := multipartUploadThreshold + 5
	u := a.createUpload(t, "multi.bin", size)
	payload := bytes.Repeat([]byte{0x51}, int(u.PartSize))
	first := a.requestRaw(http.MethodPut, "/api/uploads/"+u.UploadID+"/data/1", payload, true)
	if first.Code != 204 {
		t.Fatalf("part one=%d %s", first.Code, first.Body.String())
	}
	ack := a.request("PUT", "/api/uploads/"+u.UploadID+"/parts/1", map[string]any{"etag": first.Header().Get("ETag"), "size": len(payload)}, true)
	if ack.Code != 204 {
		t.Fatalf("ack=%d", ack.Code)
	}
	saved := a.request("GET", "/api/uploads/"+u.UploadID, nil, true)
	resume := decode[struct {
		Parts []storage.CompletedPart `json:"parts"`
	}](t, saved)
	if len(resume.Parts) != 1 {
		t.Fatalf("resume=%s", saved.Body.String())
	}
	second := a.requestRaw("PUT", "/api/uploads/"+u.UploadID+"/data/2", []byte("tail!"), true)
	if second.Code != 204 {
		t.Fatalf("part two=%d %s", second.Code, second.Body.String())
	}
	parts := append(resume.Parts, storage.CompletedPart{PartNumber: 2, ETag: second.Header().Get("ETag")})
	done := a.request("POST", "/api/uploads/"+u.UploadID+"/complete", map[string]any{"parts": parts}, true)
	if done.Code != 200 {
		t.Fatalf("complete=%d %s", done.Code, done.Body.String())
	}
	r := a.requestH("GET", "/api/files/"+u.FileID+"/download", nil, true, map[string]string{"Range": "bytes=-5"})
	if r.Code != 206 || r.Body.String() != "tail!" {
		t.Fatalf("tail range=%d %q", r.Code, r.Body.String())
	}
}

func TestDatabaseBackupsUseSeparateStore(t *testing.T) {
	a, local := localTestApp(t)
	backup := newMockStorage(0)
	a.srv.backup = backup
	a.srv.cfg.BackupRetention = 2
	if err := a.srv.createDatabaseBackup(context.Background()); err != nil {
		t.Fatal(err)
	}
	refs, err := local.ListPrefix(context.Background(), backupObjectPrefix)
	if err != nil || len(refs) != 0 {
		t.Fatalf("database backup leaked into local blobs: %v %v", refs, err)
	}
	remote, err := backup.ListDatabases(context.Background())
	if err != nil || len(remote) != 1 {
		t.Fatalf("remote snapshots=%v %v", remote, err)
	}
}
