package server

import (
	"context"
	"encoding/json"
	"net/http"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/storage"
)

func TestLegacyMediaMetadataIsReprobedAndEmbeddedSubtitlesBecomePlayable(t *testing.T) {
	app := newTestApp(t)
	file := app.readyFile(t, "legacy.mkv", []byte("unchanged media object"))
	_, err := app.srv.db.Exec(`INSERT INTO media_metadata(file_id,duration_ms,container,video_codec,audio_codec,width,height,bitrate,chapters_json,analyzed_at,frame_rate,video_profile,video_level,subtitles_json,source_etag,probe_version) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`, file.ID, 1234, "matroska", "hevc", "aac", 1920, 1080, 1, "[]", time.Now().Add(-time.Hour).UTC().Format(time.RFC3339Nano), "24/1", "Main", 120, "[]", file.ETag, mediaProbeVersion-1)
	if err != nil {
		t.Fatal(err)
	}
	var probes atomic.Int32
	app.srv.probeMediaSource = func(context.Context, File) (storage.MediaProbe, error) {
		probes.Add(1)
		return storage.MediaProbe{DurationMS: 1234, Container: "matroska", VideoCodec: "hevc", AudioCodec: "aac", Width: 1920, Height: 1080, Subtitles: []storage.MediaSubtitle{
			{Index: 2, Codec: "ass", Language: "chi", Title: "简体", Default: true},
			{Index: 3, Codec: "ass"},
		}}, nil
	}

	response := app.request(http.MethodGet, "/api/files/"+file.ID+"/video", nil, true)
	if response.Code != http.StatusOK {
		t.Fatalf("legacy media info=%d: %s", response.Code, response.Body.String())
	}
	var payload struct {
		Subtitles []videoSubtitleResponse `json:"subtitles"`
	}
	payload = decode[struct {
		Subtitles []videoSubtitleResponse `json:"subtitles"`
	}](t, response)
	if probes.Load() != 1 || len(payload.Subtitles) != 2 || payload.Subtitles[0].ID != "embedded-2" || payload.Subtitles[1].ID != "embedded-3" {
		t.Fatalf("probes=%d subtitles=%+v", probes.Load(), payload.Subtitles)
	}
	var version int
	var persisted string
	if err := app.srv.db.QueryRow(`SELECT probe_version,subtitles_json FROM media_metadata WHERE file_id=?`, file.ID).Scan(&version, &persisted); err != nil {
		t.Fatal(err)
	}
	var tracks []embeddedSubtitle
	if err := json.Unmarshal([]byte(persisted), &tracks); err != nil || version != mediaProbeVersion || len(tracks) != 2 {
		t.Fatalf("updated metadata version=%d tracks=%+v err=%v", version, tracks, err)
	}

	cacheKey := "embedded-v2:" + file.ID + ":" + file.ETag + ":" + file.UpdatedAt + ":3"
	ready := make(chan struct{})
	close(ready)
	app.srv.videoSubtitleCache[cacheKey] = &videoSubtitleCacheEntry{ready: ready, data: []byte("WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nlegacy ASS\n"), completedAt: time.Now()}
	track := app.request(http.MethodGet, payload.Subtitles[1].URL, nil, true)
	if track.Code != http.StatusOK || !strings.Contains(track.Body.String(), "legacy ASS") {
		t.Fatalf("re-probed subtitle is not playable: %d %q", track.Code, track.Body.String())
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
