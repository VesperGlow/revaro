package server

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"os"
	"reflect"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/database"
	"github.com/VesperGlow/revaro/internal/ids"
	"github.com/VesperGlow/revaro/internal/storage"
)

// Use the BT publication transaction, so the fixture has the same provenance
// and embedded/external track indices as a completed torrent import.
func importedVideoForCopy(t *testing.T, app *testApp, tracks int) (File, storage.WebMediaAsset) {
	t.Helper()
	app.srv.cleanup.Close()
	now := time.Now().UTC().Format(time.RFC3339Nano)
	job := downloadJob{ID: "copy-job", ParentID: RootID, Name: "Movie.mkv"}
	if _, err := app.db.Exec(`INSERT INTO download_jobs(id,parent_id,source_type,source,status,created_at,updated_at) VALUES(?,?,'magnet','magnet:?xt=urn:btih:copy','importing',?,?)`, job.ID, RootID, now, now); err != nil {
		t.Fatal(err)
	}
	asset := storage.WebMediaAsset{State: "completed", Key: "derived/media/copy-job/0/playback.mp4", Size: 5, ETag: "video-etag", DurationMS: 12345, VideoCodec: "hevc", AudioCodec: "aac", Subtitles: []storage.WebMediaSubtitle{}}
	for i := 0; i < tracks; i++ {
		index, language, title := 2, "eng", "English CC"
		if i > 0 {
			index, language, title = 1000000+i, "zh-Hans", "Movie.zh-Hans 外挂字幕"
		}
		asset.Subtitles = append(asset.Subtitles, storage.WebMediaSubtitle{Index: index, Key: fmt.Sprintf("derived/media/copy-job/0/subtitles/%d.vtt", index), Size: 46, ETag: fmt.Sprintf("subtitle-etag-%d", i), Language: language, Title: title, Default: i == 0, Forced: i > 0})
	}
	app.store.mu.Lock()
	app.store.raw[asset.Key] = []byte("video")
	for _, sub := range asset.Subtitles {
		app.store.raw[sub.Key] = []byte("WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nSubtitle\n")
	}
	app.store.mu.Unlock()
	manager := &downloadManager{server: app.srv}
	err := manager.commitImported(context.Background(), job, []importedDownloadFile{{path: job.Name, objectKey: asset.Key, mimeType: "video/mp4", etag: asset.ETag, size: asset.Size, index: 0, web: &asset}}, false)
	if err != nil {
		t.Fatal(err)
	}
	file, err := app.srv.file(context.Background(), "bt-copy-job-0")
	if err != nil {
		t.Fatal(err)
	}
	return file, asset
}

func copyVideoForTest(t *testing.T, app *testApp, source File) File {
	t.Helper()
	rr := app.request("POST", "/api/files/"+source.ID+"/copy", map[string]any{"parent_id": RootID}, true)
	if rr.Code != http.StatusCreated {
		t.Fatalf("copy: %d %s", rr.Code, rr.Body.String())
	}
	copy := decode[File](t, rr)
	if copy.ID == source.ID {
		t.Fatal("copy reused source file ID")
	}
	return copy
}

func assertCopiedVideo(t *testing.T, app *testApp, file File, want storage.WebMediaAsset) {
	t.Helper()
	var got storage.WebMediaAsset
	var mime, created string
	err := app.db.QueryRow(`SELECT object_key,size,etag,mime_type,duration_ms,video_codec,audio_codec,created_at FROM web_media_playback WHERE file_id=?`, file.ID).Scan(&got.Key, &got.Size, &got.ETag, &mime, &got.DurationMS, &got.VideoCodec, &got.AudioCodec, &created)
	if err != nil {
		t.Fatal(err)
	}
	got.State = "completed"
	got.Subtitles = []storage.WebMediaSubtitle{}
	rows, err := app.db.Query(`SELECT track_index,object_key,size,etag,language,title,is_default,is_forced FROM web_media_subtitles WHERE file_id=? ORDER BY track_index`, file.ID)
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()
	for rows.Next() {
		var sub storage.WebMediaSubtitle
		if err := rows.Scan(&sub.Index, &sub.Key, &sub.Size, &sub.ETag, &sub.Language, &sub.Title, &sub.Default, &sub.Forced); err != nil {
			t.Fatal(err)
		}
		got.Subtitles = append(got.Subtitles, sub)
	}
	if err := rows.Err(); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(got, want) || mime != "video/mp4" || created == "" {
		t.Fatalf("metadata = %+v (%s, %s), want %+v", got, mime, created, want)
	}
	rr := app.request("GET", "/api/files/"+file.ID+"/video", nil, true)
	if rr.Code != http.StatusOK {
		t.Fatalf("video API: %d %s", rr.Code, rr.Body.String())
	}
	response := decode[struct {
		Optimized   bool                    `json:"optimized"`
		PlaybackURL string                  `json:"playback_url"`
		Subtitles   []videoSubtitleResponse `json:"subtitles"`
	}](t, rr)
	if !response.Optimized || len(response.Subtitles) != len(want.Subtitles) {
		t.Fatalf("video API: %s", rr.Body.String())
	}
	assertAssetURL := func(url, key string) {
		t.Helper()
		resp, err := http.Get(url)
		if err != nil {
			t.Fatal(err)
		}
		defer resp.Body.Close()
		body, err := io.ReadAll(resp.Body)
		app.store.mu.RLock()
		expected := string(app.store.raw[key])
		app.store.mu.RUnlock()
		if err != nil || resp.StatusCode != http.StatusOK || expected == "" || string(body) != expected {
			t.Fatalf("asset %s: status=%d body=%q err=%v", key, resp.StatusCode, body, err)
		}
	}
	assertAssetURL(response.PlaybackURL, want.Key)
	for i, sub := range response.Subtitles {
		wantSub := want.Subtitles[i]
		if sub.ID != fmt.Sprintf("optimized-%d", wantSub.Index) || sub.Language != wantSub.Language || sub.Label != wantSub.Title || sub.Name != wantSub.Title || sub.Default != wantSub.Default || sub.Forced != wantSub.Forced {
			t.Fatalf("subtitle API = %+v, want %+v", sub, wantSub)
		}
		assertAssetURL(sub.URL, wantSub.Key)
	}
}

func TestBTVideoCopyMetadata(t *testing.T) {
	for _, tracks := range []int{0, 1, 3} {
		t.Run(fmt.Sprintf("tracks_%d", tracks), func(t *testing.T) {
			app := newTestApp(t)
			source, asset := importedVideoForCopy(t, app, tracks)
			first := copyVideoForTest(t, app, source)
			second := copyVideoForTest(t, app, source)
			third := copyVideoForTest(t, app, first)
			for _, file := range []File{source, first, second, third} {
				assertCopiedVideo(t, app, file, asset)
			}
			var count int
			if err := app.db.QueryRow(`SELECT COUNT(*) FROM web_media_ingests`).Scan(&count); err != nil || count != 1 {
				t.Fatalf("copy must not invent a torrent ingest: count=%d err=%v", count, err)
			}
		})
	}
}

func TestBTVideoCopyDeletionAndRestart(t *testing.T) {
	for _, scenario := range []struct{ deleteSource, realS3 bool }{{true, false}, {false, false}, {true, true}, {false, true}} {
		t.Run(fmt.Sprintf("delete_source_%t_real_s3_%t", scenario.deleteSource, scenario.realS3), func(t *testing.T) {
			if scenario.realS3 && (os.Getenv("DATA_PLANE_TEST_ADDR") == "" || os.Getenv("DATA_PLANE_TEST_TOKEN") == "") {
				t.Skip("Rust data-plane integration endpoint not configured")
			}
			app := newTestApp(t)
			source, asset := importedVideoForCopy(t, app, 2)
			if scenario.realS3 {
				useRealCopyAssets(t, app, source, &asset)
			}
			copy := copyVideoForTest(t, app, source)
			deleted, survivor := copy, source
			if scenario.deleteSource {
				deleted, survivor = source, copy
			}
			for _, path := range []string{"/api/files/", "/api/trash/"} {
				rr := app.request("DELETE", path+deleted.ID, nil, true)
				if rr.Code != http.StatusNoContent {
					t.Fatalf("delete: %d %s", rr.Code, rr.Body.String())
				}
			}
			for _, table := range []string{"web_media_playback", "web_media_subtitles"} {
				var count int
				if err := app.db.QueryRow(`SELECT COUNT(*) FROM `+table+` WHERE file_id=?`, deleted.ID).Scan(&count); err != nil || count != 0 {
					t.Fatalf("deleted associations in %s: %d %v", table, count, err)
				}
			}
			// Removing the torrent's provenance must not own the copied assets.
			if _, err := app.db.Exec(`DELETE FROM download_jobs WHERE id='copy-job'`); err != nil {
				t.Fatal(err)
			}
			keys := []string{asset.Key}
			for _, sub := range asset.Subtitles {
				keys = append(keys, sub.Key)
			}
			for _, key := range keys {
				app.srv.queueObjectCleanup(context.Background(), key, "stale BT cleanup")
				app.store.age(key, time.Now().Add(-48*time.Hour))
			}
			app.srv.CleanupObjects(context.Background())
			if !scenario.realS3 {
				// The real endpoint may be shared by other integration fixtures.
				app.srv.CollectGarbage(context.Background())
			}
			(&downloadManager{server: app.srv}).cleanupTorrentImport([]storage.TorrentImportFile{{Key: asset.Key, WebPrefix: asset.Key[:len(asset.Key)-len("playback.mp4")]}})
			assertCopiedVideo(t, app, survivor, asset)

			// Close all server state and the SQLite handle, then reopen the same
			// database; keep only the external object store and login cookie.
			var seq int
			var name, dbPath string
			if err := app.db.QueryRow(`PRAGMA database_list`).Scan(&seq, &name, &dbPath); err != nil {
				t.Fatal(err)
			}
			cfg, authService, backend := app.srv.cfg, app.srv.auth, app.srv.storage
			app.srv.Close()
			if err := app.db.Close(); err != nil {
				t.Fatal(err)
			}
			db, err := database.Open(dbPath)
			if err != nil {
				t.Fatal(err)
			}
			app.db, authService.DB = db, db
			app.srv = New(db, backend, authService, cfg, nil)
			app.handler = app.srv.Handler()
			t.Cleanup(func() { app.srv.Close(); db.Close() })
			app.srv.cleanup.Close()
			app.srv.CleanupObjects(context.Background())
			assertCopiedVideo(t, app, survivor, asset)
			assertCopiedVideo(t, app, copyVideoForTest(t, app, survivor), asset)
		})
	}
}

// The opt-in variant reads actual signed S3 URLs through the Rust data plane.
// Only this test's unique object keys are written or removed.
func useRealCopyAssets(t *testing.T, app *testApp, source File, asset *storage.WebMediaAsset) {
	t.Helper()
	backend := storage.NewDataPlane(os.Getenv("DATA_PLANE_TEST_ADDR"), os.Getenv("DATA_PLANE_TEST_TOKEN"))
	prefix := "derived/media/copy-test-" + ids.New() + "/"
	storeAsset := func(oldKey, suffix, mime string) storage.ObjectInfo {
		t.Helper()
		key := prefix + suffix
		app.store.mu.Lock()
		data := append([]byte(nil), app.store.raw[oldKey]...)
		app.store.raw[key] = data
		app.store.mu.Unlock()
		t.Cleanup(func() { _ = backend.DeleteObject(context.Background(), key) })
		info, err := backend.PutObject(context.Background(), key, mime, data)
		if err != nil {
			t.Fatal(err)
		}
		return info
	}
	video := storeAsset(asset.Key, "playback.mp4", "video/mp4")
	asset.Key, asset.Size, asset.ETag = prefix+"playback.mp4", video.Size, video.ETag
	if _, err := app.db.Exec(`UPDATE files SET object_key=?,size=?,etag=? WHERE id=?`, asset.Key, asset.Size, asset.ETag, source.ID); err != nil {
		t.Fatal(err)
	}
	if _, err := app.db.Exec(`UPDATE web_media_playback SET object_key=?,size=?,etag=? WHERE file_id=?`, asset.Key, asset.Size, asset.ETag, source.ID); err != nil {
		t.Fatal(err)
	}
	for i := range asset.Subtitles {
		sub := &asset.Subtitles[i]
		suffix := fmt.Sprintf("subtitles/%d.vtt", sub.Index)
		info := storeAsset(sub.Key, suffix, "text/vtt")
		sub.Key, sub.Size, sub.ETag = prefix+suffix, info.Size, info.ETag
		if _, err := app.db.Exec(`UPDATE web_media_subtitles SET object_key=?,size=?,etag=? WHERE file_id=? AND track_index=?`, sub.Key, sub.Size, sub.ETag, source.ID, sub.Index); err != nil {
			t.Fatal(err)
		}
	}
	cfg, authService := app.srv.cfg, app.srv.auth
	app.srv.Close()
	app.srv = New(app.db, backend, authService, cfg, nil)
	app.handler = app.srv.Handler()
	app.srv.cleanup.Close()
}

func TestBTVideoCopyRollsBackMetadataFailure(t *testing.T) {
	for _, stage := range []string{"playback", "second_subtitle", "commit"} {
		t.Run(stage, func(t *testing.T) {
			app := newTestApp(t)
			source, asset := importedVideoForCopy(t, app, 2)
			statement := `CREATE TRIGGER fail_copy BEFORE INSERT ON web_media_playback BEGIN SELECT RAISE(ABORT,'injected playback failure'); END`
			if stage == "second_subtitle" {
				statement = `CREATE TRIGGER fail_copy BEFORE INSERT ON web_media_subtitles WHEN NEW.track_index=1000001 BEGIN SELECT RAISE(ABORT,'injected subtitle failure'); END`
			} else if stage == "commit" {
				// The deferred FK fails at Commit, after all copy rows were written.
				statement = `CREATE TABLE copy_failure(file_id TEXT REFERENCES files(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER fail_copy AFTER INSERT ON web_media_subtitles BEGIN INSERT INTO copy_failure VALUES('missing-file'); END`
			}
			if _, err := app.db.Exec(statement); err != nil {
				t.Fatal(err)
			}
			rr := app.request("POST", "/api/files/"+source.ID+"/copy", map[string]any{"parent_id": RootID}, true)
			if rr.Code != http.StatusInternalServerError {
				t.Fatalf("copy failure: %d %s", rr.Code, rr.Body.String())
			}
			for table, want := range map[string]int{"files": 2, "web_media_playback": 1, "web_media_subtitles": 2, "web_media_ingests": 1} {
				var count int
				if err := app.db.QueryRow(`SELECT COUNT(*) FROM ` + table).Scan(&count); err != nil || count != want {
					t.Fatalf("partial copy in %s: count=%d want=%d err=%v", table, count, want, err)
				}
			}
			assertCopiedVideo(t, app, source, asset)
			if _, err := app.db.Exec(`DROP TRIGGER fail_copy`); err != nil {
				t.Fatal(err)
			}
			assertCopiedVideo(t, app, copyVideoForTest(t, app, source), asset)
		})
	}
}
