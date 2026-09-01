package server

import (
	"context"
	"net/http"
	"strings"
	"testing"
	"time"
)

func TestRootProtectionAndNameConflict(t *testing.T) {
	a := newTestApp(t)
	rr := a.request("PATCH", "/api/files/"+RootID, map[string]any{"name": "changed"}, true)
	if rr.Code != 400 {
		t.Fatalf("root rename status=%d", rr.Code)
	}
	first := a.request("POST", "/api/directories", map[string]any{"parent_id": RootID, "name": "Photos"}, true)
	if first.Code != 201 {
		t.Fatalf("create status=%d: %s", first.Code, first.Body.String())
	}
	second := a.request("POST", "/api/directories", map[string]any{"parent_id": RootID, "name": "Photos"}, true)
	if second.Code != 409 {
		t.Fatalf("duplicate status=%d", second.Code)
	}
}

func TestWriteRequestsRequireMatchingOrigin(t *testing.T) {
	a := newTestApp(t)
	body := map[string]any{"parent_id": RootID, "name": "OriginTest"}
	noOrigin := a.requestH("POST", "/api/directories", body, true, map[string]string{"Origin": ""})
	if noOrigin.Code != http.StatusForbidden {
		t.Fatalf("write without Origin status=%d", noOrigin.Code)
	}
	crossOrigin := a.requestH("POST", "/api/directories", body, true, map[string]string{"Origin": "http://evil.example"})
	if crossOrigin.Code != http.StatusForbidden {
		t.Fatalf("cross-origin write status=%d", crossOrigin.Code)
	}
	sameOrigin := a.request("POST", "/api/directories", body, true)
	if sameOrigin.Code != http.StatusCreated {
		t.Fatalf("same-origin write status=%d: %s", sameOrigin.Code, sameOrigin.Body.String())
	}
	// 读请求不受 Origin 限制
	if got := a.request("GET", "/api/storage/stats", nil, true); got.Code != http.StatusOK {
		t.Fatalf("GET with no Origin status=%d", got.Code)
	}
}

func TestDirectoryCannotMoveIntoDescendant(t *testing.T) {
	a := newTestApp(t)
	parentRR := a.request("POST", "/api/directories", map[string]any{"parent_id": RootID, "name": "Parent"}, true)
	parent := decode[File](t, parentRR)
	childRR := a.request("POST", "/api/directories", map[string]any{"parent_id": parent.ID, "name": "Child"}, true)
	child := decode[File](t, childRR)
	rr := a.request("PATCH", "/api/files/"+parent.ID, map[string]any{"parent_id": child.ID}, true)
	if rr.Code != 400 {
		t.Fatalf("cycle status=%d: %s", rr.Code, rr.Body.String())
	}
}

func TestCreateReadAndUpdateDocument(t *testing.T) {
	a := newTestApp(t)
	createdRR := a.request("POST", "/api/documents", map[string]any{"parent_id": RootID, "name": "notes.md", "content": "# First\n"}, true)
	if createdRR.Code != http.StatusCreated {
		t.Fatalf("create document=%d: %s", createdRR.Code, createdRR.Body.String())
	}
	created := decode[File](t, createdRR)
	if created.Status != "ready" || created.MimeType != "text/markdown; charset=utf-8" {
		t.Fatalf("created document=%+v", created)
	}
	var firstKey string
	if err := a.db.QueryRow(`SELECT object_key FROM files WHERE id=?`, created.ID).Scan(&firstKey); err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(firstKey, "blobs/") {
		t.Fatalf("document object key=%q", firstKey)
	}
	readRR := a.request("GET", "/api/files/"+created.ID+"/content", nil, true)
	if readRR.Code != http.StatusOK {
		t.Fatalf("read document=%d: %s", readRR.Code, readRR.Body.String())
	}
	read := decode[struct {
		Content string `json:"content"`
		ETag    string `json:"etag"`
	}](t, readRR)
	if read.Content != "# First\n" || read.ETag == "" {
		t.Fatalf("document content=%q etag=%q", read.Content, read.ETag)
	}
	conflictRR := a.request("PUT", "/api/files/"+created.ID+"/content", map[string]any{"content": "changed", "etag": "wrong"}, true)
	if conflictRR.Code != http.StatusConflict {
		t.Fatalf("stale edit=%d: %s", conflictRR.Code, conflictRR.Body.String())
	}
	updatedRR := a.request("PUT", "/api/files/"+created.ID+"/content", map[string]any{"content": "# Saved\n", "etag": read.ETag}, true)
	if updatedRR.Code != http.StatusOK {
		t.Fatalf("update document=%d: %s", updatedRR.Code, updatedRR.Body.String())
	}
	var secondKey string
	if err := a.db.QueryRow(`SELECT object_key FROM files WHERE id=?`, created.ID).Scan(&secondKey); err != nil {
		t.Fatal(err)
	}
	if secondKey == firstKey {
		t.Fatal("updated document kept the old manifest")
	}
	reread := a.request("GET", "/api/files/"+created.ID+"/content", nil, true)
	got := decode[struct {
		Content string `json:"content"`
	}](t, reread)
	if got.Content != "# Saved\n" {
		t.Fatalf("updated content=%q", got.Content)
	}
}

func TestCopyPreservesAudioMetadata(t *testing.T) {
	a := newTestApp(t)
	audio := a.readyFile(t, "album.m4a", []byte("audio-master"))
	now := time.Now().UTC().Format(time.RFC3339Nano)
	chapters := `[{"title":"第一节","start_ms":0,"end_ms":10000},{"title":"第二节","start_ms":10000,"end_ms":25000}]`
	if _, err := a.db.Exec(`INSERT INTO audio_media(file_id,duration_ms,chapters_json,stream_object_key,stream_size,stream_etag,has_cover,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?)`, audio.ID, 25000, chapters, audio.objectKey, audio.Size, audio.ETag, false, now, now); err != nil {
		t.Fatal(err)
	}

	copyRR := a.request("POST", "/api/files/"+audio.ID+"/copy", map[string]any{"parent_id": RootID}, true)
	if copyRR.Code != http.StatusCreated {
		t.Fatalf("copy=%d: %s", copyRR.Code, copyRR.Body.String())
	}
	copied := decode[File](t, copyRR)
	if copied.Name != "album - 副本.m4a" || copied.Size != audio.Size || copied.ETag != audio.ETag {
		t.Fatalf("copied file=%+v", copied)
	}
	storedCopy, err := a.srv.file(context.Background(), copied.ID)
	if err != nil || storedCopy.objectKey != audio.objectKey {
		t.Fatalf("copy object key=%q err=%v", storedCopy.objectKey, err)
	}
	infoRR := a.request("GET", "/api/files/"+copied.ID+"/audio", nil, true)
	if infoRR.Code != http.StatusOK {
		t.Fatalf("copied audio info=%d: %s", infoRR.Code, infoRR.Body.String())
	}
	info := decode[struct {
		Chapters []audioChapterResponse `json:"chapters"`
	}](t, infoRR)
	if len(info.Chapters) != 2 || info.Chapters[1].Title != "第二节" {
		t.Fatalf("copied chapters=%+v", info.Chapters)
	}
}

func TestCopyRejectsDirectoryAndInvalidTarget(t *testing.T) {
	a := newTestApp(t)
	directoryRR := a.request("POST", "/api/directories", map[string]any{"parent_id": RootID, "name": "folder"}, true)
	directory := decode[File](t, directoryRR)
	if rr := a.request("POST", "/api/files/"+directory.ID+"/copy", map[string]any{"parent_id": RootID}, true); rr.Code != http.StatusNotFound {
		t.Fatalf("directory copy=%d: %s", rr.Code, rr.Body.String())
	}
	file := a.readyFile(t, "note.txt", []byte("hello"))
	if rr := a.request("POST", "/api/files/"+file.ID+"/copy", map[string]any{"parent_id": file.ID}, true); rr.Code != http.StatusBadRequest {
		t.Fatalf("invalid target=%d: %s", rr.Code, rr.Body.String())
	}
}

func TestDocumentSaveConflictOnStaleEtag(t *testing.T) {
	a := newTestApp(t)
	doc := a.readyFile(t, "note.md", []byte("v1"))
	first := a.request("PUT", "/api/files/"+doc.ID+"/content", map[string]any{"content": "v2", "etag": doc.ETag}, true)
	if first.Code != http.StatusOK {
		t.Fatalf("first save=%d: %s", first.Code, first.Body.String())
	}
	// 用过期 etag 再保存：必须 409，而不是静默覆盖
	stale := a.request("PUT", "/api/files/"+doc.ID+"/content", map[string]any{"content": "v3", "etag": doc.ETag}, true)
	if stale.Code != http.StatusConflict {
		t.Fatalf("stale etag save=%d, want 409", stale.Code)
	}
	// 不带 etag 的保存仍然放行（旧客户端兼容）
	noEtag := a.request("PUT", "/api/files/"+doc.ID+"/content", map[string]any{"content": "v4"}, true)
	if noEtag.Code != http.StatusOK {
		t.Fatalf("no-etag save=%d", noEtag.Code)
	}
}
