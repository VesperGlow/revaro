package server

import (
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/cache"
)

func TestMediaCacheGlobalBudgetKeepsActiveHLSWorkspace(t *testing.T) {
	app := newTestApp(t)
	activeDir := t.TempDir()
	completedDir := t.TempDir()
	if err := os.WriteFile(filepath.Join(activeDir, "segment-000000.ts"), []byte("active"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(completedDir, "segment-000000.ts"), []byte("completed"), 0o600); err != nil {
		t.Fatal(err)
	}
	active := &audioHLSSession{
		ID: "active", Dir: activeDir, lastAccess: time.Now(), doneCh: make(chan struct{}), cancel: func() {},
	}
	completed := &audioHLSSession{
		ID: "completed", Dir: completedDir, lastAccess: time.Now().Add(-time.Hour), done: true,
		doneCh: make(chan struct{}), cancel: func() {},
	}
	close(completed.doneCh)
	app.srv.audioHLSMu.Lock()
	app.srv.audioHLSSessions[active.ID] = active
	app.srv.audioHLSSessions[completed.ID] = completed
	app.srv.audioHLSMu.Unlock()
	app.srv.setMediaCacheSize("audio", active.ID, directoryBytes(activeDir))
	app.srv.setMediaCacheSize("audio", completed.ID, directoryBytes(completedDir))

	stats := app.srv.cache.Stats().Classes[cacheClassMediaHLS]
	if stats.DiskBytes != int64(len("active"))+int64(len("completed")) || stats.DiskEntries != 2 {
		t.Fatalf("HLS stats = %+v", stats)
	}
	app.srv.pruneMediaCache(cache.ExternalBudget{DiskBytes: 1})

	if _, err := os.Stat(activeDir); err != nil {
		t.Fatalf("active workspace was pruned: %v", err)
	}
	if _, err := os.Stat(completedDir); !os.IsNotExist(err) {
		t.Fatalf("completed workspace survived: %v", err)
	}
	active.mu.Lock()
	active.done = true
	active.mu.Unlock()
	close(active.doneCh)
	app.srv.removeAudioHLSSession(active.ID)
}
