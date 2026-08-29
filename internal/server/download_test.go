package server

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

func TestParseByteRangeHTTPForms(t *testing.T) {
	for _, tc := range []struct {
		header     string
		size       int64
		start, end int64
		ok         bool
	}{
		{"", 100, 0, 99, true},
		{"bytes=10-19", 100, 10, 19, true},
		{"bytes=90-", 100, 90, 99, true},
		{"bytes=-10", 100, 90, 99, true},
		{"bytes=90-200", 100, 90, 99, true},
		{"bytes=100-", 100, 0, 0, false},
		{"bytes=20-10", 100, 0, 0, false},
		{"bytes=0-1,4-5", 100, 0, 0, false},
		{"items=0-1", 100, 0, 0, false},
	} {
		start, end, ok := parseByteRange(tc.header, tc.size)
		if start != tc.start || end != tc.end || ok != tc.ok {
			t.Errorf("parseByteRange(%q,%d)=(%d,%d,%t), want (%d,%d,%t)", tc.header, tc.size, start, end, ok, tc.start, tc.end, tc.ok)
		}
	}
}

func TestURLProgressReaderEnforcesLimit(t *testing.T) {
	reader := &urlProgressReader{reader: bytes.NewBufferString("abcdef"), limit: 5}
	got, err := io.ReadAll(reader)
	if !errors.Is(err, errURLDownloadTooLarge) {
		t.Fatalf("error=%v, want too large", err)
	}
	if string(got) != "abcde" || reader.read != 5 {
		t.Fatalf("got=%q read=%d", got, reader.read)
	}
}

func TestValidateURLDownload(t *testing.T) {
	for _, value := range []string{"https://example.com/video.mkv", "http://downloads.example.com:8080/file.zip?token=abc"} {
		if _, err := validateURLDownload(value); err != nil {
			t.Errorf("valid URL %q rejected: %v", value, err)
		}
	}
	for _, value := range []string{"", "/relative/file", "ftp://example.com/file", "https://user:pass@example.com/file"} {
		if _, err := validateURLDownload(value); err == nil {
			t.Errorf("invalid URL %q accepted", value)
		}
	}
}

func TestURLDownloadStreamsIntoDrive(t *testing.T) {
	payload := bytes.Repeat([]byte("direct-download-"), 2048)
	origin := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Disposition", `attachment; filename="renamed-video.mkv"`)
		w.Header().Set("Content-Type", "video/x-matroska")
		_, _ = w.Write(payload)
	}))
	defer origin.Close()
	app := newTestApp(t)
	app.srv.cfg.BTMaxTotalSize = 1 << 20
	ctx, cancel := context.WithCancel(context.Background())
	manager := &downloadManager{
		server: app.srv, http: origin.Client(), ctx: ctx, cancel: cancel,
		jobs: make(map[string]*downloadRuntime), urlJobs: make(map[string]*urlDownloadRuntime), urlSlots: make(chan struct{}, 1),
	}
	app.srv.downloads = manager
	job, err := manager.createURL(context.Background(), RootID, origin.URL+"/ignored-name.bin")
	if err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(5 * time.Second)
	for {
		job, err = manager.getURL(context.Background(), job.ID)
		if err != nil {
			t.Fatal(err)
		}
		if job.Status == "done" || job.Status == "failed" {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("direct download did not finish")
		}
		time.Sleep(10 * time.Millisecond)
	}
	if job.Status != "done" || job.Name != "renamed-video.mkv" || job.CompletedSize != int64(len(payload)) {
		t.Fatalf("job=%+v", job)
	}
	var objectKey string
	if err := app.db.QueryRow(`SELECT object_key FROM files WHERE parent_id=? AND name=?`, RootID, job.Name).Scan(&objectKey); err != nil {
		t.Fatal(err)
	}
	stored, err := app.store.GetObject(context.Background(), objectKey, int64(len(payload)))
	if err != nil || !bytes.Equal(stored, payload) {
		t.Fatalf("stored bytes=%d error=%v", len(stored), err)
	}
}

func TestSafeTorrentPath(t *testing.T) {
	for _, value := range []string{"movie.mkv", "season/episode 01.mkv", "folder\\subtitle.vtt"} {
		if _, err := safeTorrentPath(value); err != nil {
			t.Errorf("safe path %q rejected: %v", value, err)
		}
	}
	for _, value := range []string{"", ".", "../secret", "/etc/passwd", "folder/../../secret", "folder/\x00name"} {
		if _, err := safeTorrentPath(value); err == nil {
			t.Errorf("unsafe path %q accepted", value)
		}
	}
}
