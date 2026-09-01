package server

import (
	"context"
	"fmt"
	"net/http"
	"strings"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
	"github.com/VesperGlow/revaro/internal/storage"
)

func TestSingleObjectUploadLifecycle(t *testing.T) {
	a := newTestApp(t)
	content := []byte("hello, world")
	created := a.createUpload(t, "hello.txt", int64(len(content)))
	if created.Mode != "single" || created.URL == "" || created.PartCount != 0 {
		t.Fatalf("created upload=%+v", created)
	}
	tasksRR := a.request("GET", "/api/tasks", nil, true)
	if tasksRR.Code != http.StatusOK || !strings.Contains(tasksRR.Body.String(), "hello.txt") || !strings.Contains(tasksRR.Body.String(), `"type":"upload"`) {
		t.Fatalf("unified task=%d: %s", tasksRR.Code, tasksRR.Body.String())
	}
	var key string
	if err := a.db.QueryRow(`SELECT object_key FROM files WHERE id=?`, created.FileID).Scan(&key); err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(key, "blobs/") || strings.Contains(key, "hello") {
		t.Fatalf("opaque object key=%q", key)
	}
	if rr := a.request("POST", "/api/uploads/"+created.UploadID+"/complete", map[string]any{"parts": []any{}}, true); rr.Code != http.StatusBadGateway {
		t.Fatalf("complete before PUT=%d: %s", rr.Code, rr.Body.String())
	}
	a.store.raw[key] = append([]byte(nil), content...)
	doneRR := a.request("POST", "/api/uploads/"+created.UploadID+"/complete", map[string]any{"parts": []any{}}, true)
	if doneRR.Code != http.StatusOK {
		t.Fatalf("complete=%d: %s", doneRR.Code, doneRR.Body.String())
	}
	var contentHash, algorithm string
	if err := a.db.QueryRow(`SELECT content_hash,hash_algorithm FROM files WHERE id=?`, created.FileID).Scan(&contentHash, &algorithm); err != nil || algorithm != "sha256" || len(contentHash) != 64 {
		t.Fatalf("integrity metadata hash=%q algorithm=%q err=%v", contentHash, algorithm, err)
	}
	if repeated := a.request("POST", "/api/uploads/"+created.UploadID+"/complete", map[string]any{"parts": []any{}}, true); repeated.Code != http.StatusOK {
		t.Fatalf("idempotent complete=%d: %s", repeated.Code, repeated.Body.String())
	}
	download := a.request("GET", "/api/files/"+created.FileID+"/download", nil, true)
	if download.Code != http.StatusOK || download.Body.String() != string(content) {
		t.Fatalf("proxied download=%d body=%q", download.Code, download.Body.String())
	}
}

func TestMultipartUploadLifecycle(t *testing.T) {
	a := newTestApp(t)
	size := multipartUploadThreshold
	created := a.createUpload(t, "large.bin", size)
	if created.Mode != "multipart" || created.PartCount != 1 || created.PartSize < 5<<20 {
		t.Fatalf("created multipart=%+v", created)
	}
	partsRR := a.request("POST", "/api/uploads/"+created.UploadID+"/parts", map[string]any{"part_numbers": []int{1}}, true)
	if partsRR.Code != http.StatusOK || !strings.Contains(partsRR.Body.String(), "multipart") {
		t.Fatalf("parts=%d: %s", partsRR.Code, partsRR.Body.String())
	}
	if rr := a.request("POST", "/api/uploads/"+created.UploadID+"/parts", map[string]any{"part_numbers": []int{2}}, true); rr.Code != http.StatusBadRequest {
		t.Fatalf("invalid part=%d", rr.Code)
	}
	var key string
	_ = a.db.QueryRow(`SELECT object_key FROM files WHERE id=?`, created.FileID).Scan(&key)
	a.store.raw[key] = make([]byte, size)
	ack := a.request("PUT", "/api/uploads/"+created.UploadID+"/parts/1", map[string]any{"etag": "part-etag", "size": size}, true)
	if ack.Code != http.StatusNoContent {
		t.Fatalf("part ack=%d: %s", ack.Code, ack.Body.String())
	}
	resumed := a.request("GET", "/api/uploads/"+created.UploadID, nil, true)
	if resumed.Code != http.StatusOK || !strings.Contains(resumed.Body.String(), "part-etag") {
		t.Fatalf("resume=%d: %s", resumed.Code, resumed.Body.String())
	}
	done := a.request("POST", "/api/uploads/"+created.UploadID+"/complete", map[string]any{"parts": []map[string]any{}}, true)
	if done.Code != http.StatusOK {
		t.Fatalf("multipart complete=%d: %s", done.Code, done.Body.String())
	}
}

func TestEmptyFileUpload(t *testing.T) {
	a := newTestApp(t)
	created := a.createUpload(t, "empty.bin", 0)
	var key string
	_ = a.db.QueryRow(`SELECT object_key FROM files WHERE id=?`, created.FileID).Scan(&key)
	a.store.raw[key] = []byte{}
	doneRR := a.request("POST", "/api/uploads/"+created.UploadID+"/complete", map[string]any{"parts": []map[string]any{}}, true)
	if doneRR.Code != http.StatusOK {
		t.Fatalf("complete=%d: %s", doneRR.Code, doneRR.Body.String())
	}
	dl := a.request("GET", "/api/files/"+created.FileID+"/download", nil, true)
	if dl.Code != http.StatusOK {
		t.Fatalf("empty download=%d", dl.Code)
	}
}

func TestAbortUpload(t *testing.T) {
	a := newTestApp(t)
	created := a.createUpload(t, "pending.bin", 100)
	if rr := a.request("DELETE", "/api/uploads/"+created.UploadID, nil, true); rr.Code != http.StatusNoContent {
		t.Fatalf("abort=%d: %s", rr.Code, rr.Body.String())
	}
	var n int
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM files WHERE id=?`, created.FileID).Scan(&n); err != nil {
		t.Fatal(err)
	}
	if n != 0 {
		t.Fatal("aborted upload left its file row behind")
	}
}

func TestGarbageCollector(t *testing.T) {
	a := newTestApp(t)
	live := a.readyFile(t, "live.bin", []byte("live"))
	orphan := storage.BlobKey(ids.New())
	a.store.raw[orphan] = []byte("orphan")
	a.store.age(orphan, time.Now().Add(-48*time.Hour))
	a.store.age(live.objectKey, time.Now().Add(-48*time.Hour))
	a.srv.CollectGarbage(context.Background())
	if _, ok := a.store.raw[live.objectKey]; !ok {
		t.Fatal("referenced blob was collected")
	}
	if _, ok := a.store.raw[orphan]; ok {
		t.Fatal("orphaned blob was not collected")
	}
}

func TestGarbageCollectorStreamsPagesAndBoundsDeleteBatches(t *testing.T) {
	a := newTestApp(t)
	old := time.Now().Add(-48 * time.Hour)
	for i := 0; i < 2505; i++ {
		key := fmt.Sprintf("blobs/orphan-%04d", i)
		a.store.raw[key] = []byte("x")
		a.store.age(key, old)
	}
	a.srv.CollectGarbage(context.Background())
	if len(a.store.raw) != 0 {
		t.Fatalf("%d orphan objects survived", len(a.store.raw))
	}
	for _, size := range a.store.deleteBatchSizes {
		if size <= 0 || size > 1000 {
			t.Fatalf("invalid delete batch size %d", size)
		}
	}
	if len(a.store.deleteBatchSizes) < 2 {
		t.Fatalf("expected multiple bounded delete batches, got %v", a.store.deleteBatchSizes)
	}
}

func TestGarbageCollectorKeepsAudioStreamAndCover(t *testing.T) {
	a := newTestAppWithBlockSize(t, 8)
	master := a.readyFile(t, "book.flac", []byte("lossless-master"))
	streamKey := master.objectKey
	coverKey := audioThumbnailKey(master.objectKey)
	a.store.raw[coverKey] = []byte("jpeg-cover")
	a.store.age(coverKey, time.Now().Add(-48*time.Hour))
	now := time.Now().UTC().Format(time.RFC3339Nano)
	if _, err := a.db.Exec(`INSERT INTO audio_media(file_id,duration_ms,chapters_json,stream_object_key,stream_size,stream_etag,has_cover,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?)`,
		master.ID, 1000, `[{"title":"Part 1","start_ms":0,"end_ms":1000}]`, streamKey, master.Size, master.ETag, true, now, now); err != nil {
		t.Fatal(err)
	}
	a.srv.CollectGarbage(context.Background())
	if _, ok := a.store.raw[streamKey]; !ok {
		t.Fatal("referenced audio stream blob was collected")
	}
	if _, ok := a.store.raw[coverKey]; !ok {
		t.Fatal("referenced audio cover was collected")
	}
}

func TestExpiredUploadCannotComplete(t *testing.T) {
	a := newTestApp(t)
	u := a.createUpload(t, "expired.bin", 100)
	// 把 expires_at 改为过去，模拟过期但尚未被定时清理的窗口期
	if _, err := a.db.Exec(`UPDATE uploads SET expires_at=? WHERE id=?`, time.Now().UTC().Add(-time.Minute).Format(time.RFC3339Nano), u.UploadID); err != nil {
		t.Fatal(err)
	}
	body := map[string]any{"parts": []any{}}
	if rr := a.request("POST", "/api/uploads/"+u.UploadID+"/complete", body, true); rr.Code != http.StatusNotFound {
		t.Fatalf("expired complete=%d, want 404: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("POST", "/api/uploads/"+u.UploadID+"/parts", map[string]any{"part_numbers": []int{1}}, true); rr.Code != http.StatusNotFound {
		t.Fatalf("expired parts=%d, want 404", rr.Code)
	}
}

func TestMultipartPartSizeStaysWithinS3Limit(t *testing.T) {
	partSize := multipartPartSize(1 << 40)
	if count, err := storage.ValidMultipartPartCount(1<<40, partSize); err != nil || count > 10000 {
		t.Fatalf("part size=%d count=%d err=%v", partSize, count, err)
	}
}
