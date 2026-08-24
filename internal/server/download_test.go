package server

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/btstore"
	"github.com/anacrolix/torrent/metainfo"
)

func TestDownloadImportProgressReader(t *testing.T) {
	var updates []int64
	reader := &downloadImportProgressReader{
		reader:     bytes.NewBufferString("abcdefghij"),
		onProgress: func(read int64) { updates = append(updates, read) },
	}
	got, err := io.ReadAll(reader)
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != "abcdefghij" || reader.read != int64(len(got)) {
		t.Fatalf("got=%q read=%d", got, reader.read)
	}
	if len(updates) == 0 || updates[len(updates)-1] != int64(len(got)) {
		t.Fatalf("progress updates=%v", updates)
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
	stored, err := app.store.ReadFile(context.Background(), objectKey, int64(len(payload)))
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

func TestTorrentPieceStorePersistsVerifiedPieces(t *testing.T) {
	app := newTestApp(t)
	pieceStore, err := btstore.New(app.db, app.store, t.TempDir(), nil)
	if err != nil {
		t.Fatal(err)
	}
	info := &metainfo.Info{Name: "piece.bin", PieceLength: 4, Length: 4, Pieces: make([]byte, metainfo.HashSize)}
	var hash metainfo.Hash
	torrentStore, err := pieceStore.OpenTorrent(context.Background(), info, hash)
	if err != nil {
		t.Fatal(err)
	}
	piece := torrentStore.Piece(info.Piece(0))
	want := []byte("data")
	if n, err := piece.WriteAt(want, 0); err != nil || n != len(want) {
		t.Fatalf("write piece n=%d err=%v", n, err)
	}
	if err := piece.MarkComplete(); err != nil {
		t.Fatal(err)
	}
	if completion := piece.Completion(); !completion.Ok || !completion.Complete || completion.Err != nil {
		t.Fatalf("unexpected completion: %+v", completion)
	}
	got := make([]byte, len(want))
	if n, err := piece.ReadAt(got, 0); err != nil || n != len(got) || !bytes.Equal(got, want) {
		t.Fatalf("read piece got=%q n=%d err=%v", got, n, err)
	}
	var indexed int
	if err := app.db.QueryRow(`SELECT COUNT(*) FROM download_pieces WHERE info_hash=?`, hash.HexString()).Scan(&indexed); err != nil || indexed != 1 {
		t.Fatalf("piece index count=%d err=%v", indexed, err)
	}
	if err := pieceStore.DeleteTorrent(context.Background(), hash.HexString()); err != nil {
		t.Fatal(err)
	}
	if len(app.store.raw) != 0 {
		t.Fatalf("temporary objects not removed: %v", app.store.raw)
	}
}

func TestPrivateAddressBlocklist(t *testing.T) {
	blocklist := privateIPBlocklist()
	for _, value := range []string{"127.0.0.1", "10.1.2.3", "100.64.1.2", "169.254.2.3", "192.168.1.1", "198.18.0.1", "::1", "fc00::1", "fe80::1"} {
		if _, blocked := blocklist.Lookup(net.ParseIP(value)); !blocked {
			t.Errorf("private address %s was not blocked", value)
		}
	}
	for _, value := range []string{"1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"} {
		if _, blocked := blocklist.Lookup(net.ParseIP(value)); blocked {
			t.Errorf("public address %s was blocked", value)
		}
	}
}
