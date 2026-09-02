package server

import (
	"context"
	"net/http"
	"net/url"
	"testing"
	"time"
)

func TestTrashRestoresTreeAndProtectsContentFromGC(t *testing.T) {
	a := newTestApp(t)
	parentRR := a.request("POST", "/api/directories", map[string]any{"parent_id": RootID, "name": "Archive"}, true)
	parent := decode[File](t, parentRR)
	childRR := a.request("POST", "/api/directories", map[string]any{"parent_id": parent.ID, "name": "Nested"}, true)
	child := decode[File](t, childRR)
	f := a.readyFile(t, "kept.txt", []byte("recover me"))
	if rr := a.request("PATCH", "/api/files/"+f.ID, map[string]any{"parent_id": child.ID}, true); rr.Code != http.StatusOK {
		t.Fatalf("move file=%d: %s", rr.Code, rr.Body.String())
	}
	shareRR := a.request("POST", "/api/files/"+f.ID+"/share", nil, true)
	share := decode[struct {
		URL string `json:"url"`
	}](t, shareRR)
	shareURL, _ := url.Parse(share.URL)

	if rr := a.request("DELETE", "/api/files/"+parent.ID, nil, true); rr.Code != http.StatusNoContent {
		t.Fatalf("trash tree=%d: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("GET", "/api/files/"+f.ID, nil, true); rr.Code != http.StatusNotFound {
		t.Fatalf("trashed descendant remains readable: %d", rr.Code)
	}
	if rr := a.request("GET", shareURL.Path, nil, false); rr.Code != http.StatusNotFound {
		t.Fatalf("trashed share remains readable: %d", rr.Code)
	}
	trashRR := a.request("GET", "/api/trash", nil, true)
	trash := decode[struct {
		Items      []File `json:"items"`
		TotalBytes int64  `json:"total_bytes"`
		FileCount  int64  `json:"file_count"`
	}](t, trashRR)
	if len(trash.Items) != 1 || trash.Items[0].ID != parent.ID || trash.Items[0].DeletedAt == "" {
		t.Fatalf("trash roots=%+v", trash.Items)
	}
	if trash.TotalBytes != int64(len("recover me")) || trash.FileCount != 1 {
		t.Fatalf("trash recursive stats=%+v", trash)
	}
	if rr := a.request("POST", "/api/trash/"+parent.ID+"/restore", nil, true); rr.Code != http.StatusNoContent {
		t.Fatalf("restore=%d: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("GET", "/api/files/"+f.ID, nil, true); rr.Code != http.StatusOK {
		t.Fatalf("restored descendant unavailable: %d", rr.Code)
	}
	if rr := a.request("GET", shareURL.Path, nil, false); rr.Code != http.StatusOK {
		t.Fatalf("restored share unavailable: %d", rr.Code)
	}

	if rr := a.request("DELETE", "/api/files/"+parent.ID, nil, true); rr.Code != http.StatusNoContent {
		t.Fatalf("trash again=%d", rr.Code)
	}
	if rr := a.request("POST", "/api/directories", map[string]any{"parent_id": RootID, "name": "Archive"}, true); rr.Code != http.StatusCreated {
		t.Fatalf("reuse trashed name=%d: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("POST", "/api/trash/"+parent.ID+"/restore", nil, true); rr.Code != http.StatusConflict {
		t.Fatalf("restore conflict=%d: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("DELETE", "/api/trash/"+parent.ID, nil, true); rr.Code != http.StatusNoContent {
		t.Fatalf("purge tree=%d: %s", rr.Code, rr.Body.String())
	}
	var remaining int
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM files WHERE id IN (?,?,?)`, parent.ID, child.ID, f.ID).Scan(&remaining); err != nil || remaining != 0 {
		t.Fatalf("purged tree remains: count=%d err=%v", remaining, err)
	}
}

func TestTrashFilesRemainReadableUntilPurged(t *testing.T) {
	a := newTestApp(t)
	photo := a.readyFile(t, "photo.png", realPNG(t, 320, 180))
	video := a.readyFile(t, "clip.mp4", []byte("video preview"))
	audio := a.readyFile(t, "song.mp3", []byte("audio preview"))
	book := a.readyFile(t, "novel.txt", []byte("第一章\n回收站里的正文"))
	document := a.readyFile(t, "notes.md", []byte("# 仍可查看"))

	for _, f := range []File{photo, video, audio, book, document} {
		if rr := a.request("DELETE", "/api/files/"+f.ID, nil, true); rr.Code != http.StatusNoContent {
			t.Fatalf("trash %s=%d: %s", f.Name, rr.Code, rr.Body.String())
		}
	}
	for _, f := range []File{photo, video, audio} {
		if rr := a.request("GET", "/api/files/"+f.ID+"/preview", nil, true); rr.Code != http.StatusOK {
			t.Fatalf("trashed preview %s=%d: %s", f.Name, rr.Code, rr.Body.String())
		}
	}
	if rr := a.request("GET", "/api/files/"+photo.ID+"/thumbnail", nil, true); rr.Code != http.StatusOK {
		t.Fatalf("trashed thumbnail=%d: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("GET", "/api/files/"+audio.ID+"/download", nil, true); rr.Code != http.StatusOK {
		t.Fatalf("trashed download=%d: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("GET", "/api/files/"+book.ID+"/book", nil, true); rr.Code != http.StatusOK {
		t.Fatalf("trashed book=%d: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("GET", "/api/files/"+document.ID+"/content", nil, true); rr.Code != http.StatusOK {
		t.Fatalf("trashed document=%d: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("DELETE", "/api/trash/"+photo.ID, nil, true); rr.Code != http.StatusNoContent {
		t.Fatalf("purge photo=%d: %s", rr.Code, rr.Body.String())
	}
	if rr := a.request("GET", "/api/files/"+photo.ID+"/preview", nil, true); rr.Code != http.StatusNotFound {
		t.Fatalf("purged preview remains readable: %d", rr.Code)
	}
}

func TestEmptyTrashRemovesEveryDeletedTree(t *testing.T) {
	a := newTestApp(t)
	first := a.readyFile(t, "first.txt", []byte("first"))
	second := a.readyFile(t, "second.txt", []byte("second"))
	for _, f := range []File{first, second} {
		if rr := a.request("DELETE", "/api/files/"+f.ID, nil, true); rr.Code != http.StatusNoContent {
			t.Fatalf("trash %s=%d", f.Name, rr.Code)
		}
	}
	if rr := a.request("DELETE", "/api/trash", nil, true); rr.Code != http.StatusNoContent {
		t.Fatalf("empty trash=%d: %s", rr.Code, rr.Body.String())
	}
	trash := a.request("GET", "/api/trash", nil, true)
	items := decode[struct {
		Items []File `json:"items"`
	}](t, trash)
	if len(items.Items) != 0 {
		t.Fatalf("trash is not empty: %+v", items.Items)
	}
}

func TestExpiredTrashCleanupRemovesOnlyExpiredTrees(t *testing.T) {
	a := newTestApp(t)
	parentRR := a.request("POST", "/api/directories", map[string]any{"parent_id": RootID, "name": "Expired"}, true)
	parent := decode[File](t, parentRR)
	child := a.readyFile(t, "child.txt", []byte("expired content"))
	if rr := a.request("PATCH", "/api/files/"+child.ID, map[string]any{"parent_id": parent.ID}, true); rr.Code != http.StatusOK {
		t.Fatalf("move child=%d: %s", rr.Code, rr.Body.String())
	}
	recent := a.readyFile(t, "recent.txt", []byte("recent content"))
	for _, id := range []string{parent.ID, recent.ID} {
		if rr := a.request("DELETE", "/api/files/"+id, nil, true); rr.Code != http.StatusNoContent {
			t.Fatalf("trash %s=%d: %s", id, rr.Code, rr.Body.String())
		}
	}
	old := time.Now().UTC().Add(-31 * 24 * time.Hour).Format(time.RFC3339Nano)
	if _, err := a.db.Exec(`UPDATE files SET deleted_at=? WHERE trash_root_id=?`, old, parent.ID); err != nil {
		t.Fatal(err)
	}
	if roots := a.srv.CleanupExpiredTrash(context.Background()); roots != 1 {
		t.Fatalf("expired roots=%d, want 1", roots)
	}
	var expiredItems int
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM files WHERE id IN (?,?)`, parent.ID, child.ID).Scan(&expiredItems); err != nil || expiredItems != 0 {
		t.Fatalf("expired tree remains: count=%d err=%v", expiredItems, err)
	}
	var recentItems int
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM files WHERE id=? AND deleted_at IS NOT NULL`, recent.ID).Scan(&recentItems); err != nil || recentItems != 1 {
		t.Fatalf("recent trash was deleted: count=%d err=%v", recentItems, err)
	}
	a.srv.CollectGarbage(context.Background())
	if _, ok := a.store.manifests[child.objectKey]; ok {
		t.Fatal("content of expired trash was not reclaimed")
	}
	if roots := a.srv.CleanupExpiredTrash(context.Background()); roots != 0 {
		t.Fatalf("second cleanup removed %d roots", roots)
	}
}
