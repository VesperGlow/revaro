package server

import (
	"context"
	"net/http"
	"strings"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/storage"
)

func TestOptimizedVideoMetadataUsesSignedS3Assets(t *testing.T) {
	app := newTestApp(t)
	video := app.readyFile(t, "Movie.mkv", []byte("original"))
	app.store.rawURL = "https://objects.example"
	now := time.Now().UTC().Format(time.RFC3339Nano)
	if _, err := app.db.Exec(`INSERT INTO web_media_ingests(file_id,download_job_id,file_index,state,video_codec,audio_codec,created_at,updated_at)
		VALUES(?, 'job', 0, 'completed', 'hevc', 'aac', ?, ?)`, video.ID, now, now); err == nil {
		t.Fatal("ingest without download foreign key unexpectedly succeeded")
	}
	if _, err := app.db.Exec(`INSERT INTO download_jobs(id,parent_id,source_type,source,info_hash,status,ingest_state,created_at,updated_at) VALUES('job',?,'magnet','magnet:?xt=urn:btih:test','hash','done','completed',?,?)`, RootID, now, now); err != nil {
		t.Fatal(err)
	}
	if _, err := app.db.Exec(`INSERT INTO web_media_ingests(file_id,download_job_id,file_index,state,video_codec,audio_codec,created_at,updated_at) VALUES(?, 'job', 0, 'completed', 'hevc', 'aac', ?, ?)`, video.ID, now, now); err != nil {
		t.Fatal(err)
	}
	if _, err := app.db.Exec(`INSERT INTO web_media_playback(file_id,object_key,size,etag,duration_ms,video_codec,audio_codec,created_at) VALUES(?,'derived/media/job/0/playback.mp4',123,'etag',1000,'hevc','aac',?)`, video.ID, now); err != nil {
		t.Fatal(err)
	}
	if _, err := app.db.Exec(`INSERT INTO web_media_subtitles(file_id,track_index,object_key,size,etag,language,title,is_default,is_forced) VALUES(? ,2,'derived/media/job/0/subtitles/2.vtt',20,'subetag','eng','English',1,0)`, video.ID); err != nil {
		t.Fatal(err)
	}
	rr := app.request("GET", "/api/files/"+video.ID+"/video", nil, true)
	if rr.Code != http.StatusOK || !strings.Contains(rr.Body.String(), `"optimized":true`) || !strings.Contains(rr.Body.String(), "derived/media/job/0/playback.mp4") || !strings.Contains(rr.Body.String(), "subtitles/2.vtt") {
		t.Fatalf("optimized metadata=%d: %s", rr.Code, rr.Body.String())
	}
}

func TestWebMediaPublicationIsAtomicAndRetryable(t *testing.T) {
	app := newTestApp(t)
	now := time.Now().UTC().Format(time.RFC3339Nano)
	job := downloadJob{ID: "atomic-job", ParentID: RootID, Name: "Movie.mkv"}
	manager := &downloadManager{server: app.srv}
	if _, err := app.db.Exec(`INSERT INTO download_jobs(id,parent_id,source_type,source,info_hash,status,ingest_state,created_at,updated_at) VALUES(?,?,'magnet','magnet:?xt=urn:btih:atomic','atomic','importing','uploading',?,?)`, job.ID, RootID, now, now); err != nil {
		t.Fatal(err)
	}
	asset := &storage.WebMediaAsset{State: "completed", Key: "derived/media/atomic-job/0/playback.mp4", Size: 100, ETag: "p", VideoCodec: "hevc", AudioCodec: "aac", Subtitles: []storage.WebMediaSubtitle{{Index: 2, Key: "derived/media/atomic-job/0/subtitles/2.vtt", Size: 0, ETag: "s"}}}
	file := importedDownloadFile{path: "Movie.mkv", objectKey: asset.Key, mimeType: "video/mp4", etag: asset.ETag, size: asset.Size, index: 0, web: asset}
	if err := manager.commitImported(context.Background(), job, []importedDownloadFile{file}, false); err == nil {
		t.Fatal("partial invalid asset was published")
	}
	var count int
	if err := app.db.QueryRow(`SELECT COUNT(*) FROM files WHERE id=?`, "bt-atomic-job-0").Scan(&count); err != nil || count != 0 {
		t.Fatalf("file publication count=%d err=%v", count, err)
	}
	asset.Subtitles[0].Size = 20
	if err := manager.commitImported(context.Background(), job, []importedDownloadFile{file}, false); err != nil {
		t.Fatal(err)
	}
	if err := app.db.QueryRow(`SELECT COUNT(*) FROM web_media_playback WHERE file_id=?`, "bt-atomic-job-0").Scan(&count); err != nil || count != 1 {
		t.Fatalf("playback count=%d err=%v", count, err)
	}
	var objectKey, mimeType string
	var size int64
	if err := app.db.QueryRow(`SELECT object_key,size,mime_type FROM files WHERE id=?`, "bt-atomic-job-0").Scan(&objectKey, &size, &mimeType); err != nil {
		t.Fatal(err)
	}
	if objectKey != asset.Key || size != asset.Size || mimeType != "video/mp4" || strings.Contains(objectKey, "/source") {
		t.Fatalf("logical BT media persisted raw source: key=%q size=%d mime=%q", objectKey, size, mimeType)
	}
}

func TestUnsupportedBTMediaPublishesNoFileOrObjectMetadata(t *testing.T) {
	app := newTestApp(t)
	now := time.Now().UTC().Format(time.RFC3339Nano)
	if _, err := app.db.Exec(`INSERT INTO download_jobs(id,parent_id,source_type,source,info_hash,status,ingest_state,created_at,updated_at) VALUES('unsupported-job',?,'magnet','magnet:?xt=urn:btih:unsupported','unsupported-hash','importing','processing',?,?)`, RootID, now, now); err != nil {
		t.Fatal(err)
	}
	manager := &downloadManager{server: app.srv}
	manager.unsupported("unsupported-job", 4, "unsupported video codec: vp9")
	var status, ingestState, message string
	if err := app.db.QueryRow(`SELECT status,ingest_state,error FROM download_jobs WHERE id='unsupported-job'`).Scan(&status, &ingestState, &message); err != nil {
		t.Fatal(err)
	}
	if status != "failed" || ingestState != "unsupported" || !strings.Contains(message, "vp9") {
		t.Fatalf("unsupported state=(%q,%q,%q)", status, ingestState, message)
	}
	var fileID *string
	if err := app.db.QueryRow(`SELECT file_id FROM web_media_ingests WHERE download_job_id='unsupported-job' AND file_index=4`).Scan(&fileID); err != nil || fileID != nil {
		t.Fatalf("unsupported file_id=%v err=%v", fileID, err)
	}
	var count int
	if err := app.db.QueryRow(`SELECT COUNT(*) FROM files WHERE id='bt-unsupported-job-4'`).Scan(&count); err != nil || count != 0 {
		t.Fatalf("unsupported logical files=%d err=%v", count, err)
	}
}
