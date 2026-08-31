package server

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/storage"
)

type retainedTorrentEngine struct {
	mu      sync.Mutex
	paused  int
	deleted int
	imports int
}

func (e *retainedTorrentEngine) AddTorrent(context.Context, string, string, []int, bool) (storage.TorrentAddResult, error) {
	return storage.TorrentAddResult{}, nil
}
func (e *retainedTorrentEngine) TorrentDetails(context.Context, int) (storage.TorrentDetails, error) {
	return storage.TorrentDetails{}, nil
}
func (e *retainedTorrentEngine) TorrentStats(context.Context, int) (storage.TorrentStats, error) {
	return storage.TorrentStats{}, nil
}
func (e *retainedTorrentEngine) SelectTorrentFiles(context.Context, int, []int) error { return nil }
func (e *retainedTorrentEngine) StartTorrent(context.Context, int) error              { return nil }
func (e *retainedTorrentEngine) PauseTorrent(context.Context, int) error {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.paused++
	return nil
}
func (e *retainedTorrentEngine) ImportTorrent(context.Context, int, []storage.TorrentImportFile) ([]storage.TorrentImportedFile, error) {
	e.mu.Lock()
	e.imports++
	e.mu.Unlock()
	return nil, errors.New("fixture ingest failure")
}
func (e *retainedTorrentEngine) DeleteTorrent(context.Context, int) error {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.deleted++
	return nil
}
func (e *retainedTorrentEngine) StreamTorrent(context.Context, int, int, int64, int64) (io.ReadCloser, error) {
	return io.NopCloser(bytes.NewReader(nil)), nil
}

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

func TestPublicDownloadIPRejectsSpecialUseNetworks(t *testing.T) {
	for _, value := range []string{"127.0.0.1", "10.0.0.1", "100.64.0.1", "169.254.169.254", "192.0.2.1", "198.18.0.1", "203.0.113.1", "::1", "2001:db8::1"} {
		if isPublicDownloadIP(net.ParseIP(value)) {
			t.Errorf("special-use IP accepted: %s", value)
		}
	}
	for _, value := range []string{"1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"} {
		if !isPublicDownloadIP(net.ParseIP(value)) {
			t.Errorf("public IP rejected: %s", value)
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

func TestFailedTorrentStagingIsRetainedUntilExplicitRemoval(t *testing.T) {
	app := newTestApp(t)
	engine := &retainedTorrentEngine{}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	manager := &downloadManager{
		server: app.srv, bt: engine, ctx: ctx, cancel: cancel,
		jobs: make(map[string]*downloadRuntime), urlJobs: make(map[string]*urlDownloadRuntime),
	}
	app.srv.downloads = manager
	now := time.Now().UTC().Format(time.RFC3339Nano)
	const jobID = "retained-failed-torrent"
	if _, err := app.db.Exec(`INSERT INTO download_jobs(id,parent_id,source_type,source,status,ingest_state,selected_size,completed_size,created_at,updated_at) VALUES(?,?,'magnet','magnet:?xt=urn:btih:retained','importing','processing',100,100,?,?)`, jobID, RootID, now, now); err != nil {
		t.Fatal(err)
	}
	runtimeCtx, runtimeCancel := context.WithCancel(ctx)
	runtime := &downloadRuntime{jobID: jobID, torrentID: 42, ctx: runtimeCtx, cancel: runtimeCancel}
	manager.jobs[jobID] = runtime

	manager.fail(jobID, errors.New("mux failed"))
	engine.mu.Lock()
	paused, deleted := engine.paused, engine.deleted
	engine.mu.Unlock()
	if paused != 1 || deleted != 0 {
		t.Fatalf("after failure paused=%d deleted=%d", paused, deleted)
	}
	if runtimeCtx.Err() != nil || manager.jobs[jobID] != runtime {
		t.Fatal("failed torrent runtime was cancelled or detached")
	}
	var status, ingestState string
	if err := app.db.QueryRow(`SELECT status,ingest_state FROM download_jobs WHERE id=?`, jobID).Scan(&status, &ingestState); err != nil {
		t.Fatal(err)
	}
	if status != "failed" || ingestState != "failed" {
		t.Fatalf("status=%q ingest_state=%q", status, ingestState)
	}
	if err := manager.resume(context.Background(), jobID); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(time.Second)
	for {
		engine.mu.Lock()
		imports, retryDeletes := engine.imports, engine.deleted
		engine.mu.Unlock()
		runtime.mu.Lock()
		importing := runtime.importing
		runtime.mu.Unlock()
		var retryStatus string
		_ = app.db.QueryRow(`SELECT status FROM download_jobs WHERE id=?`, jobID).Scan(&retryStatus)
		if imports == 1 && retryStatus == "failed" && !importing {
			if retryDeletes != 0 {
				t.Fatalf("retry failure deleted=%d torrents", retryDeletes)
			}
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("retry did not invoke torrent import")
		}
		time.Sleep(time.Millisecond)
	}

	if err := manager.remove(context.Background(), jobID); err != nil {
		t.Fatal(err)
	}
	engine.mu.Lock()
	deleted = engine.deleted
	engine.mu.Unlock()
	if deleted != 1 {
		t.Fatalf("explicit removal deleted=%d torrents", deleted)
	}
}
