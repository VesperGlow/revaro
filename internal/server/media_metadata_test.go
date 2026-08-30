package server

import (
	"context"
	"database/sql"
	"errors"
	"sync/atomic"
	"testing"
	"time"
)

func TestMediaMetadataCacheRequiresCurrentProbeVersion(t *testing.T) {
	app := newTestApp(t)
	file := app.readyFile(t, "legacy.mkv", []byte("not media"))
	_, err := app.srv.db.Exec(`INSERT INTO media_metadata(file_id,duration_ms,container,video_codec,audio_codec,width,height,bitrate,chapters_json,analyzed_at,frame_rate,video_profile,video_level,subtitles_json,source_etag,probe_version) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`, file.ID, 1234, "matroska", "hevc", "aac", 1, 1, 1, "[]", time.Now().UTC().Format(time.RFC3339Nano), "", "", 0, "[]", file.ETag, 0)
	if err != nil {
		t.Fatal(err)
	}
	var duration int64
	err = app.srv.db.QueryRow(`SELECT duration_ms FROM media_metadata WHERE file_id=? AND source_etag=? AND probe_version=?`, file.ID, file.ETag, mediaProbeVersion).Scan(&duration)
	if !errors.Is(err, sql.ErrNoRows) {
		t.Fatalf("legacy probe cache unexpectedly reusable: duration=%d err=%v", duration, err)
	}
}

func TestMediaAnalysisSchedulerLimitsConcurrencyAndDeduplicatesFileIDs(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	queue := newMediaAnalysisScheduler(2)
	started := make(chan string, 4)
	release := make(chan struct{})
	finished := make(chan struct{}, 4)
	var running, maximum atomic.Int32
	work := func(id string) func(context.Context) {
		return func(context.Context) {
			current := running.Add(1)
			for observed := maximum.Load(); current > observed && !maximum.CompareAndSwap(observed, current); observed = maximum.Load() {
			}
			started <- id
			<-release
			running.Add(-1)
			finished <- struct{}{}
		}
	}
	if !queue.schedule(ctx, "file-a", work("file-a")) {
		t.Fatal("first file was not scheduled")
	}
	if queue.schedule(ctx, "file-a", work("duplicate")) {
		t.Fatal("duplicate file ID was scheduled")
	}
	if !queue.schedule(ctx, "file-b", work("file-b")) || !queue.schedule(ctx, "file-c", work("file-c")) {
		t.Fatal("distinct files were not scheduled")
	}
	for range 2 {
		select {
		case <-started:
		case <-time.After(time.Second):
			t.Fatal("two analysis workers did not start")
		}
	}
	select {
	case id := <-started:
		t.Fatalf("third analysis %q started before a slot was released", id)
	case <-time.After(50 * time.Millisecond):
	}
	close(release)
	for range 3 {
		select {
		case <-finished:
		case <-time.After(time.Second):
			t.Fatal("scheduled analysis did not finish")
		}
	}
	if got := maximum.Load(); got != 2 {
		t.Fatalf("maximum concurrent analyses=%d, want 2", got)
	}
}
