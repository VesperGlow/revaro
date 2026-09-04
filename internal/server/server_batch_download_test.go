package server

import (
	"archive/zip"
	"bytes"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
	"github.com/VesperGlow/revaro/internal/storage"
)

func TestBatchDownloadCreatesZipWithAllFileContents(t *testing.T) {
	a := newTestApp(t)
	first := a.readyFile(t, "first.txt", []byte("first content"))
	second := a.readyFile(t, "second.txt", []byte("second content"))

	rr := batchDownload(t, a, []string{first.ID, second.ID})
	if rr.Code != http.StatusOK {
		t.Fatalf("batch download=%d: %s", rr.Code, rr.Body.String())
	}
	if got := rr.Header().Get("Content-Type"); got != "application/zip" {
		t.Fatalf("content type=%q", got)
	}
	if got := rr.Header().Get("Content-Disposition"); got != `attachment; filename="revaro-download.zip"` {
		t.Fatalf("content disposition=%q", got)
	}

	entries := readBatchZip(t, rr.Body.Bytes())
	if string(entries["first.txt"]) != "first content" || string(entries["second.txt"]) != "second content" {
		t.Fatalf("zip entries=%v", entries)
	}
}

func TestBatchDownloadMakesDuplicateNamesUnique(t *testing.T) {
	a := newTestApp(t)
	directoryRR := a.request("POST", "/api/directories", map[string]any{"parent_id": RootID, "name": "other"}, true)
	if directoryRR.Code != http.StatusCreated {
		t.Fatalf("create directory=%d: %s", directoryRR.Code, directoryRR.Body.String())
	}
	directory := decode[File](t, directoryRR)
	first := a.readyFile(t, "file.txt", []byte("root"))
	second := addBatchReadyFile(t, a, directory.ID, "file.txt", []byte("nested"))

	rr := batchDownload(t, a, []string{first.ID, second.ID})
	if rr.Code != http.StatusOK {
		t.Fatalf("batch download=%d: %s", rr.Code, rr.Body.String())
	}
	entries := readBatchZip(t, rr.Body.Bytes())
	if string(entries["file.txt"]) != "root" || string(entries["file (2).txt"]) != "nested" {
		t.Fatalf("zip entries=%v", entries)
	}
}

func TestBatchDownloadCleansTraversalFromZipEntryNames(t *testing.T) {
	a := newTestApp(t)
	file := a.readyFile(t, "../evil.txt", []byte("safe name"))

	rr := batchDownload(t, a, []string{file.ID})
	if rr.Code != http.StatusOK {
		t.Fatalf("batch download=%d: %s", rr.Code, rr.Body.String())
	}
	entries := readBatchZip(t, rr.Body.Bytes())
	if string(entries["evil.txt"]) != "safe name" {
		t.Fatalf("zip entries=%v", entries)
	}
	for name := range entries {
		if strings.Contains(name, "/") || strings.Contains(name, `\`) || name == "." || name == ".." {
			t.Fatalf("unsafe zip entry name=%q", name)
		}
	}
}

func TestBatchDownloadRejectsInvalidSelections(t *testing.T) {
	a := newTestApp(t)
	file := a.readyFile(t, "ready.txt", []byte("ready"))
	directoryRR := a.request("POST", "/api/directories", map[string]any{"parent_id": RootID, "name": "folder"}, true)
	if directoryRR.Code != http.StatusCreated {
		t.Fatalf("create directory=%d: %s", directoryRR.Code, directoryRR.Body.String())
	}
	directory := decode[File](t, directoryRR)
	pending := a.readyFile(t, "pending.txt", []byte("pending"))
	if _, err := a.db.Exec(`UPDATE files SET status='pending' WHERE id=?`, pending.ID); err != nil {
		t.Fatal(err)
	}

	cases := []struct {
		name string
		ids  []string
		code int
	}{
		{name: "object key is not an id", ids: []string{storage.BlobKey(file.ID)}, code: http.StatusBadRequest},
		{name: "missing id", ids: []string{ids.New()}, code: http.StatusNotFound},
		{name: "directory id", ids: []string{directory.ID}, code: http.StatusBadRequest},
		{name: "non-ready file", ids: []string{pending.ID}, code: http.StatusConflict},
		{name: "duplicate id", ids: []string{file.ID, file.ID}, code: http.StatusBadRequest},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			rr := a.request("POST", "/api/files/batch-download/prepare", map[string]any{"ids": tc.ids}, true)
			if rr.Code != tc.code {
				t.Fatalf("status=%d want=%d body=%s", rr.Code, tc.code, rr.Body.String())
			}
			if rr.Header().Get("Content-Type") == "application/zip" {
				t.Fatal("invalid selection started a ZIP response")
			}
		})
	}
}

func TestBatchDownloadRequiresAuthentication(t *testing.T) {
	a := newTestApp(t)
	rr := a.request("POST", "/api/files/batch-download/prepare", map[string]any{"ids": []string{ids.New()}}, false)
	if rr.Code != http.StatusUnauthorized {
		t.Fatalf("unauthenticated prepare status=%d: %s", rr.Code, rr.Body.String())
	}
	file := a.readyFile(t, "auth.txt", []byte("auth"))
	token := prepareBatchDownload(t, a, []string{file.ID})
	rr = a.request("GET", "/api/files/batch-download/"+token, nil, false)
	if rr.Code != http.StatusUnauthorized {
		t.Fatalf("unauthenticated download status=%d: %s", rr.Code, rr.Body.String())
	}
}

func TestBatchDownloadTokenIsOneTime(t *testing.T) {
	a := newTestApp(t)
	file := a.readyFile(t, "once.txt", []byte("once"))
	token := prepareBatchDownload(t, a, []string{file.ID})
	first := a.request("GET", "/api/files/batch-download/"+token, nil, true)
	if first.Code != http.StatusOK {
		t.Fatalf("first batch download=%d: %s", first.Code, first.Body.String())
	}
	second := a.request("GET", "/api/files/batch-download/"+token, nil, true)
	if second.Code != http.StatusNotFound {
		t.Fatalf("reused batch download=%d: %s", second.Code, second.Body.String())
	}
}

type batchDownloadPrepareResponse struct {
	Token string `json:"token"`
}

func prepareBatchDownload(t *testing.T, a *testApp, ids []string) string {
	t.Helper()
	rr := a.request("POST", "/api/files/batch-download/prepare", map[string]any{"ids": ids}, true)
	if rr.Code != http.StatusOK {
		t.Fatalf("batch download prepare=%d: %s", rr.Code, rr.Body.String())
	}
	response := decode[batchDownloadPrepareResponse](t, rr)
	if response.Token == "" {
		t.Fatal("batch download prepare returned an empty token")
	}
	return response.Token
}

func batchDownload(t *testing.T, a *testApp, ids []string) *httptest.ResponseRecorder {
	t.Helper()
	token := prepareBatchDownload(t, a, ids)
	return a.request("GET", "/api/files/batch-download/"+token, nil, true)
}

func addBatchReadyFile(t *testing.T, a *testApp, parentID, name string, content []byte) File {
	t.Helper()
	id := ids.New()
	key := storage.BlobKey(id)
	a.store.mu.Lock()
	a.store.raw[key] = append([]byte(nil), content...)
	a.store.modified[key] = time.Now().UTC()
	a.store.mu.Unlock()
	etag := sha256hex(content)
	now := time.Now().UTC().Format(time.RFC3339Nano)
	if _, err := a.db.Exec(`INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)`, id, parentID, name, "file", key, len(content), "application/octet-stream", etag, "ready", now, now); err != nil {
		t.Fatal(err)
	}
	return File{ID: id, ParentID: &parentID, Name: name, Kind: "file", Size: int64(len(content)), MimeType: "application/octet-stream", ETag: etag, Status: "ready", CreatedAt: now, UpdatedAt: now, objectKey: key}
}

func readBatchZip(t *testing.T, data []byte) map[string][]byte {
	t.Helper()
	reader, err := zip.NewReader(bytes.NewReader(data), int64(len(data)))
	if err != nil {
		t.Fatalf("read ZIP: %v", err)
	}
	entries := make(map[string][]byte, len(reader.File))
	for _, file := range reader.File {
		body, err := file.Open()
		if err != nil {
			t.Fatalf("open ZIP entry %q: %v", file.Name, err)
		}
		content, readErr := io.ReadAll(body)
		closeErr := body.Close()
		if readErr != nil || closeErr != nil {
			t.Fatalf("read ZIP entry %q: read=%v close=%v", file.Name, readErr, closeErr)
		}
		entries[file.Name] = content
	}
	return entries
}
