package server

import (
	"net/http"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
	"github.com/VesperGlow/revaro/internal/storage"
)

// insertLibraryFile 直接写入一行 ready 文件，用于验证跨目录聚合。
func insertLibraryFile(t *testing.T, a *testApp, parent, name, mimeType string) File {
	t.Helper()
	id := ids.New()
	key := storage.BlobKey(id)
	content := []byte(name)
	a.store.mu.Lock()
	a.store.raw[key] = append([]byte(nil), content...)
	a.store.modified[key] = time.Now().UTC()
	a.store.mu.Unlock()
	now := time.Now().UTC().Format(time.RFC3339Nano)
	if _, err := a.db.Exec(`INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)`, id, parent, name, "file", key, len(content), mimeType, sha256hex(content), "ready", now, now); err != nil {
		t.Fatal(err)
	}
	parentCopy := parent
	return File{ID: id, ParentID: &parentCopy, Name: name, Kind: "file", Size: int64(len(content)), MimeType: mimeType, ETag: sha256hex(content), Status: "ready", CreatedAt: now, UpdatedAt: now, objectKey: key}
}

func createLibraryFolder(t *testing.T, a *testApp, parent, name string) File {
	t.Helper()
	rr := a.request("POST", "/api/directories", map[string]any{"parent_id": parent, "name": name}, true)
	if rr.Code != http.StatusCreated {
		t.Fatalf("create directory status=%d: %s", rr.Code, rr.Body.String())
	}
	return decode[File](t, rr)
}

func TestLibraryAggregatesByTypeWithFolderPaths(t *testing.T) {
	a := newTestApp(t)
	photos := createLibraryFolder(t, a, RootID, "Photos")
	trips := createLibraryFolder(t, a, photos.ID, "Trips")
	music := createLibraryFolder(t, a, RootID, "Music")

	insertLibraryFile(t, a, trips.ID, "sunset.jpg", "image/jpeg")
	insertLibraryFile(t, a, trips.ID, "clip.mp4", "video/mp4")
	insertLibraryFile(t, a, music.ID, "song.mp3", "audio/mpeg")
	insertLibraryFile(t, a, RootID, "novel.epub", "application/epub+zip")
	insertLibraryFile(t, a, RootID, "notes.txt", "text/plain")
	insertLibraryFile(t, a, RootID, "report.pdf", "application/pdf")

	countsRR := a.request("GET", "/api/library/counts", nil, true)
	if countsRR.Code != http.StatusOK {
		t.Fatalf("counts status=%d: %s", countsRR.Code, countsRR.Body.String())
	}
	counts := decode[libraryCounts](t, countsRR)
	if counts != (libraryCounts{Book: 2, Image: 1, Video: 1, Audio: 1, File: 6}) {
		t.Fatalf("counts=%+v", counts)
	}

	imagesRR := a.request("GET", "/api/library?type=image", nil, true)
	if imagesRR.Code != http.StatusOK {
		t.Fatalf("image library status=%d: %s", imagesRR.Code, imagesRR.Body.String())
	}
	images := decode[struct {
		Type   string        `json:"type"`
		Items  []libraryItem `json:"items"`
		Counts libraryCounts `json:"counts"`
	}](t, imagesRR)
	if images.Type != "image" || len(images.Items) != 1 || images.Items[0].Name != "sunset.jpg" {
		t.Fatalf("images=%+v", images)
	}
	if len(images.Items[0].FolderPath) != 2 || images.Items[0].FolderPath[0].Name != "Photos" || images.Items[0].FolderPath[1].Name != "Trips" || images.Items[0].FolderPath[1].ID != trips.ID {
		t.Fatalf("folder path=%+v", images.Items[0].FolderPath)
	}

	books := decode[struct {
		Items []libraryItem `json:"items"`
	}](t, a.request("GET", "/api/library?type=book", nil, true))
	if len(books.Items) != 2 {
		t.Fatalf("books=%+v", books.Items)
	}

	if rr := a.request("GET", "/api/library?type=unknown", nil, true); rr.Code != http.StatusBadRequest {
		t.Fatalf("unknown type status=%d", rr.Code)
	}

	allRR := a.request("GET", "/api/library/all", nil, true)
	if allRR.Code != http.StatusOK {
		t.Fatalf("all status=%d: %s", allRR.Code, allRR.Body.String())
	}
	all := decode[struct {
		Items  map[string][]libraryItem `json:"items"`
		Counts libraryCounts            `json:"counts"`
	}](t, allRR)
	if len(all.Items["book"]) != 2 || len(all.Items["image"]) != 1 || len(all.Items["video"]) != 1 || len(all.Items["audio"]) != 1 {
		t.Fatalf("all items=%+v", all.Items)
	}
	if all.Counts.File != 6 {
		t.Fatalf("all counts=%+v", all.Counts)
	}
}

func TestLibraryExcludesTrashedFiles(t *testing.T) {
	a := newTestApp(t)
	keep := insertLibraryFile(t, a, RootID, "keep.png", "image/png")
	gone := insertLibraryFile(t, a, RootID, "gone.png", "image/png")
	if rr := a.request("DELETE", "/api/files/"+gone.ID, nil, true); rr.Code != http.StatusNoContent && rr.Code != http.StatusOK {
		t.Fatalf("delete status=%d: %s", rr.Code, rr.Body.String())
	}
	rr := a.request("GET", "/api/library?type=image", nil, true)
	items := decode[struct {
		Items []libraryItem `json:"items"`
	}](t, rr)
	if len(items.Items) != 1 || items.Items[0].ID != keep.ID {
		t.Fatalf("items=%+v", items.Items)
	}
}
