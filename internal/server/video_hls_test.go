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
	if segments != 2 || duration != 8 {
		t.Fatalf("playlist state=(%d,%v), want (2,8)", segments, duration)
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
	response, ok = app.srv.reusableVideoHLSResponse("stale-browser-token", session.FileID, 108)
	if !ok || response.SessionID != session.ID {
		t.Fatalf("file cache was not searched after stale token: response=%+v reusable=%v", response, ok)
	}
}

func TestVideoHLSTranscodeReleaseRetainsSegmentsUntilCacheDestroy(t *testing.T) {
	dir := t.TempDir()
	segment := filepath.Join(dir, "segment-000000.ts")
	if err := os.WriteFile(segment, []byte("segment"), 0o600); err != nil {
		t.Fatal(err)
	}
	done := make(chan struct{})
	session := &videoHLSSession{Dir: dir, doneCh: done, cancel: func() { close(done) }}
	session.cancelTranscode()
	if _, err := os.Stat(segment); err != nil {
		t.Fatalf("release removed cached segment: %v", err)
	}
	session.destroy()
	if _, err := os.Stat(dir); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("destroy did not remove cache directory: %v", err)
	}
}

func TestVideoHLSCompletedCacheRequiresForwardBuffer(t *testing.T) {
	if videoHLSTargetReady(116, 112, 300) {
		t.Fatal("completed cache with only four seconds ahead was accepted")
	}
	if !videoHLSTargetReady(120, 112, 300) {
		t.Fatal("completed cache with eight seconds ahead was rejected")
	}
	if !videoHLSTargetReady(300, 296, 300) {
		t.Fatal("final short range of a completed video was rejected")
	}
}

func TestVideoHLSPausesOnlyPresentedSession(t *testing.T) {
	app := newTestApp(t)
	done := make(chan struct{})
	session := &videoHLSSession{ID: "presented", FileID: "video", cancel: func() { close(done) }, doneCh: done}
	app.srv.videoHLSMu.Lock()
	app.srv.videoHLSSessions[session.ID] = session
	app.srv.videoHLSMu.Unlock()
	app.srv.pauseVideoHLSTranscoder("another-tab", "video")
	select {
	case <-done:
		t.Fatal("another tab token stopped the presented session")
	default:
	}
	app.srv.pauseVideoHLSTranscoder(session.ID, session.FileID)
	select {
	case <-done:
	default:
		t.Fatal("presented session was not stopped")
	}
}
