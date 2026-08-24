package server

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"testing"
	"time"
)

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
	if segments != 4 || duration != 16 {
		t.Fatalf("playlist state=(%d,%v), want (4,16)", segments, duration)
	}
}

func TestVideoHLSSessionIsReusedForGeneratedRange(t *testing.T) {
	app := newTestApp(t)
	dir := t.TempDir()
	playlist := filepath.Join(dir, "index.m3u8")
	writeHLSPlaylist(t, playlist, 4)
	done := make(chan struct{})
	close(done)
	session := &videoHLSSession{
		ID: "session", FileID: "video", Start: 100, Duration: 300, Dir: dir, Playlist: playlist,
		lastAccess: time.Now(), videoCodec: "hevc", audioCodec: "aac", transcoding: true,
		cancel: func() {}, doneCh: done,
	}
	app.srv.videoHLSMu.Lock()
	app.srv.videoHLSSessions[session.ID] = session
	app.srv.videoHLSMu.Unlock()

	response, ok := app.srv.reusableVideoHLSResponse(session.ID, session.FileID, 112)
	if !ok || response.SessionID != session.ID || response.Start != 100 {
		t.Fatalf("response=%+v reusable=%v", response, ok)
	}
	if _, ok := app.srv.reusableVideoHLSResponse(session.ID, session.FileID, 140); ok {
		t.Fatal("target outside generated playlist must not reuse session")
	}
}
