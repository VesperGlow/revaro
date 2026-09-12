package server

import (
	"context"
	"errors"
	"net/http"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/storage"
)

func uploadLocalFile(t *testing.T, a *testApp, name string) File {
	t.Helper()
	u := a.createUpload(t, name, 7)
	if r := a.requestRaw("PUT", u.URL, []byte("content"), true); r.Code != 204 {
		t.Fatalf("upload: %d %s", r.Code, r.Body.String())
	}
	if r := a.request("POST", "/api/uploads/"+u.UploadID+"/complete", map[string]any{}, true); r.Code != 200 {
		t.Fatalf("complete: %d %s", r.Code, r.Body.String())
	}
	f, err := a.srv.file(context.Background(), u.FileID)
	if err != nil {
		t.Fatal(err)
	}
	return f
}

func TestLocalTrashCleanupPreservesCopiesAndReclaimsFreshContent(t *testing.T) {
	a, local := localTestApp(t)
	a.srv.cleanup.Close()
	ctx := context.Background()
	f := uploadLocalFile(t, a, "original.txt")
	copied := a.request("POST", "/api/files/"+f.ID+"/copy", map[string]any{"parent_id": RootID}, true)
	if copied.Code != 201 {
		t.Fatal(copied.Body.String())
	}
	copy := decode[File](t, copied)
	derived := []string{imageThumbnailKey(f.objectKey), audioThumbnailKey(f.objectKey), videoThumbnailKey(f.objectKey), "flows/" + f.objectKey + "/f1/manifest.json", "flows/" + f.objectKey + "/f2/chunks/0.html"}
	for _, key := range derived {
		if _, err := local.PutObject(ctx, key, "application/octet-stream", []byte("cache")); err != nil {
			t.Fatal(err)
		}
	}
	if r := a.request("DELETE", "/api/files/"+f.ID, nil, true); r.Code != 204 {
		t.Fatal(r.Body.String())
	}
	if err := a.srv.CleanupObjects(ctx); err != nil {
		t.Fatal(err)
	}
	if _, err := local.HeadObject(ctx, f.objectKey); err != nil {
		t.Fatal("trash lost content", err)
	}
	if r := a.request("POST", "/api/trash/"+f.ID+"/restore", nil, true); r.Code != 204 {
		t.Fatal(r.Body.String())
	}
	a.request("DELETE", "/api/files/"+f.ID, nil, true)
	if r := a.request("DELETE", "/api/trash/"+f.ID, nil, true); r.Code != 204 {
		t.Fatal(r.Body.String())
	}
	if err := a.srv.CleanupObjects(ctx); err != nil {
		t.Fatal(err)
	}
	for _, key := range append(derived, f.objectKey) {
		if _, err := local.HeadObject(ctx, key); err != nil {
			t.Fatalf("shared object %s deleted: %v", key, err)
		}
	}
	var queued int
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM object_cleanup`).Scan(&queued); err != nil || queued != 0 {
		t.Fatalf("shared queue not cleared: %d %v", queued, err)
	}
	if r := a.request("GET", "/api/files/"+copy.ID+"/download", nil, true); r.Code != 200 || r.Body.String() != "content" {
		t.Fatal("copy unreadable")
	}
	a.request("DELETE", "/api/files/"+copy.ID, nil, true)
	if r := a.request("DELETE", "/api/trash", nil, true); r.Code != 204 {
		t.Fatal(r.Body.String())
	}
	// No object aging: explicit permanent deletion bypasses orphan-upload grace.
	if err := a.srv.CleanupObjects(ctx); err != nil {
		t.Fatal(err)
	}
	for _, key := range append(derived, f.objectKey) {
		if _, err := local.HeadObject(ctx, key); !storage.IsNotFound(err) {
			t.Fatalf("purged object %s retained: %v", key, err)
		}
	}
}

type deletionFailureStore struct {
	storage.Storage
	fail bool
}

func (s *deletionFailureStore) DeleteObject(ctx context.Context, key string) error {
	if s.fail {
		return errors.New("disk temporarily unavailable")
	}
	return s.Storage.DeleteObject(ctx, key)
}

func TestLocalDeletionFailureIsDurableAndRetries(t *testing.T) {
	a, local := localTestApp(t)
	a.srv.cleanup.Close()
	f := uploadLocalFile(t, a, "retry.txt")
	a.request("DELETE", "/api/files/"+f.ID, nil, true)
	a.request("DELETE", "/api/trash", nil, true)
	broken := &deletionFailureStore{Storage: local, fail: true}
	a.srv.objects.store = broken
	if err := a.srv.CleanupObjects(context.Background()); err == nil {
		t.Fatal("disk failure was swallowed")
	}
	var count int
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM object_cleanup WHERE object_key=?`, f.objectKey).Scan(&count); err != nil || count != 1 {
		t.Fatalf("cleanup intent lost: %d %v", count, err)
	}
	broken.fail = false
	if err := a.srv.CleanupObjects(context.Background()); err != nil {
		t.Fatal(err)
	}
	if _, err := local.HeadObject(context.Background(), f.objectKey); !storage.IsNotFound(err) {
		t.Fatal("retry retained content", err)
	}
}

func TestEmptyTrashWakesDiskCleanupWithPeriodicGCDisabled(t *testing.T) {
	a, local := localTestApp(t)
	if a.srv.cfg.GCInterval != 0 {
		t.Fatal("test requires disabled periodic GC")
	}
	f := uploadLocalFile(t, a, "wake.txt")
	a.request("DELETE", "/api/files/"+f.ID, nil, true)
	if r := a.request("DELETE", "/api/trash", nil, true); r.Code != http.StatusNoContent {
		t.Fatal(r.Body.String())
	}
	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		if _, err := local.HeadObject(context.Background(), f.objectKey); storage.IsNotFound(err) {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatal("permanent deletion did not wake background disk cleanup")
}

func TestLocalCleanupQueueSurvivesServerRestart(t *testing.T) {
	a, local := localTestApp(t)
	a.srv.cleanup.Close()
	f := uploadLocalFile(t, a, "restart.txt")
	a.request("DELETE", "/api/files/"+f.ID, nil, true)
	a.request("DELETE", "/api/trash", nil, true)
	a.srv.Close()
	a.srv = New(a.db, local, a.srv.auth, a.srv.cfg, nil)
	a.handler = a.srv.Handler()
	deadline := time.Now().Add(3 * time.Second)
	for time.Now().Before(deadline) {
		if _, err := local.HeadObject(context.Background(), f.objectKey); storage.IsNotFound(err) {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatal("restart did not drain durable cleanup queue")
}

func TestLocalOrphanGCPreservesFreshAndPendingBlobs(t *testing.T) {
	a, local := localTestApp(t)
	a.srv.cleanup.Close()
	ctx := context.Background()
	u := a.createUpload(t, "pending.txt", 7)
	if r := a.requestRaw("PUT", u.URL, []byte("content"), true); r.Code != 204 {
		t.Fatal(r.Body.String())
	}
	var pendingKey string
	if err := a.db.QueryRow(`SELECT object_key FROM files WHERE id=?`, u.FileID).Scan(&pendingKey); err != nil {
		t.Fatal(err)
	}
	for _, key := range []string{"blobs/old-orphan", "blobs/fresh-orphan"} {
		if _, err := local.PutObject(ctx, key, "", []byte("orphan")); err != nil {
			t.Fatal(err)
		}
	}
	old := time.Now().Add(-48 * time.Hour)
	for _, key := range []string{"blobs/old-orphan", pendingKey} {
		if err := os.Chtimes(filepath.Join(a.srv.cfg.DataDir, "objects", key), old, old); err != nil {
			t.Fatal(err)
		}
	}
	a.srv.CollectGarbage(ctx)
	if _, err := local.HeadObject(ctx, "blobs/old-orphan"); !storage.IsNotFound(err) {
		t.Fatal("old orphan survived", err)
	}
	for _, key := range []string{"blobs/fresh-orphan", pendingKey} {
		if _, err := local.HeadObject(ctx, key); err != nil {
			t.Fatal("protected object removed", key, err)
		}
	}
}

func TestFileDeletionQueueRollsBackWithMetadata(t *testing.T) {
	a, local := localTestApp(t)
	a.srv.cleanup.Close()
	f := uploadLocalFile(t, a, "rollback.txt")
	tx, err := a.db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	if _, err = tx.Exec(`DELETE FROM files WHERE id=?`, f.ID); err != nil {
		t.Fatal(err)
	}
	tx.Rollback()
	if err := a.srv.CleanupObjects(context.Background()); err != nil {
		t.Fatal(err)
	}
	if _, err := local.HeadObject(context.Background(), f.objectKey); err != nil {
		t.Fatal("rollback lost blob", err)
	}
}
