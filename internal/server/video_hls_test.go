package server

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestHLSSetupFailureReturnsConcurrencySlot(t *testing.T) {
	for _, tc := range []struct {
		name, fileName, mimeType, endpoint string
	}{
		{"audio", "track.mp3", "audio/mpeg", "/api/files/%s/audio/hls"},
		{"video", "movie.mp4", "video/mp4", "/api/files/%s/video/hls"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			app := newTestApp(t)
			file := app.readyFile(t, tc.fileName, []byte("not needed for setup failure"))
			if _, err := app.db.Exec(`UPDATE files SET mime_type=? WHERE id=?`, tc.mimeType, file.ID); err != nil {
				t.Fatal(err)
			}
			if err := os.RemoveAll(app.srv.cfg.WorkDir); err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(app.srv.cfg.WorkDir, []byte("not a directory"), 0o600); err != nil {
				t.Fatal(err)
			}
			endpoint := fmt.Sprintf(tc.endpoint, file.ID)
			for attempt := 0; attempt < 2; attempt++ {
				response := app.request(http.MethodPost, endpoint, map[string]any{"start": 0}, true)
				if response.Code != http.StatusInternalServerError {
					t.Fatalf("attempt %d status=%d body=%s", attempt+1, response.Code, response.Body.String())
				}
			}
		})
	}
}

func writeHLSPlaylist(t *testing.T, path string, segments int) {
	t.Helper()
	body := "#EXTM3U\n#EXT-X-VERSION:3\n"
	for index := 0; index < segments; index++ {
		body += "#EXTINF:4.000000,\nsegment-00000" + string(rune('0'+index)) + ".ts\n"
	}
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
}

func TestVideoHLSWaitsForStartupBuffer(t *testing.T) {
	dir := t.TempDir()
	playlist := filepath.Join(dir, "index.m3u8")
	writeHLSPlaylist(t, playlist, 1)
	session := &videoHLSSession{Playlist: playlist, doneCh: make(chan struct{})}
	ctx, cancel := context.WithTimeout(context.Background(), 180*time.Millisecond)
	defer cancel()
	if err := waitForVideoHLS(ctx, session); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("one segment should not start playback, got %v", err)
	}
	writeHLSPlaylist(t, playlist, videoHLSStartupChunks)
	if err := waitForVideoHLS(context.Background(), session); err != nil {
		t.Fatal(err)
	}
	segments, duration := videoHLSPlaylistState(playlist)
	if segments != 2 || duration != 8 {
		t.Fatalf("playlist state=(%d,%v), want (2,8)", segments, duration)
	}
}

func TestVideoHLSSeekDestroysOnlyPresentedSession(t *testing.T) {
	app := newTestApp(t)
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "segment-000000.ts"), []byte("segment"), 0o600); err != nil {
		t.Fatal(err)
	}
	done := make(chan struct{})
	session := &videoHLSSession{ID: "presented", FileID: "video", Dir: dir, cancel: func() { close(done) }, doneCh: done}
	app.srv.videoHLSMu.Lock()
	app.srv.videoHLSSessions[session.ID] = session
	app.srv.videoHLSMu.Unlock()
	app.srv.removeVideoHLSSessionForFile("another-tab", "video")
	select {
	case <-done:
		t.Fatal("another tab token stopped the presented session")
	default:
	}
	app.srv.removeVideoHLSSessionForFile(session.ID, session.FileID)
	select {
	case <-done:
	default:
		t.Fatal("presented session was not stopped")
	}
	if _, err := os.Stat(dir); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("seek did not remove old session workspace: %v", err)
	}
}
