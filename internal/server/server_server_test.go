package server

import (
	"net/http"
	"strings"
	"testing"
)

func TestLegacyTaskQueryEndpointsAreRemoved(t *testing.T) {
	a := newTestApp(t)
	for _, path := range []string{"/api/archive-jobs", "/api/archive-jobs/legacy", "/api/audio-merges", "/api/audio-merges/legacy", "/api/downloads"} {
		rr := a.request(http.MethodGet, path, nil, true)
		if rr.Code != http.StatusNotFound && rr.Code != http.StatusMethodNotAllowed {
			t.Fatalf("legacy query endpoint %s returned %d: %s", path, rr.Code, rr.Body.String())
		}
	}
}

func TestDeletePendingFile(t *testing.T) {
	a := newTestApp(t)
	created := a.createUpload(t, "pending.bin", 100)
	// 上传失败/中断遗留的 pending 行可以直接删除，uploads 记录级联清理。
	if rr := a.request("DELETE", "/api/files/"+created.FileID, nil, true); rr.Code != http.StatusNoContent {
		t.Fatalf("delete pending=%d: %s", rr.Code, rr.Body.String())
	}
	var n int
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM files WHERE id=?`, created.FileID).Scan(&n); err != nil {
		t.Fatal(err)
	}
	if n != 0 {
		t.Fatal("pending file row not deleted")
	}
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM uploads WHERE id=?`, created.UploadID).Scan(&n); err != nil {
		t.Fatal(err)
	}
	if n != 0 {
		t.Fatal("uploads row not cascaded")
	}
}

func TestUnknownAPIEndpointReturnsJSON404(t *testing.T) {
	a := newTestApp(t)
	rr := a.request("GET", "/api/does-not-exist", nil, true)
	if rr.Code != http.StatusNotFound {
		t.Fatalf("unknown api=%d, want 404", rr.Code)
	}
	if ct := rr.Header().Get("Content-Type"); !strings.HasPrefix(ct, "application/json") {
		t.Fatalf("content-type=%q, want JSON", ct)
	}
}
