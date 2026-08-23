package server

import (
	"bytes"
	"context"
	"encoding/json"
	"image"
	"image/color"
	"image/jpeg"
	"io"
	"net/http"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
)

type audioMediaTestResponse struct {
	Duration   float64 `json:"duration"`
	StreamURL  string  `json:"stream_url"`
	CoverURL   string  `json:"cover_url"`
	HasCover   bool    `json:"has_cover"`
	StreamSize int64   `json:"stream_size"`
	Chapters   []struct {
		ID    int     `json:"id"`
		Title string  `json:"title"`
		Start float64 `json:"start"`
		End   float64 `json:"end"`
	} `json:"chapters"`
}

type audioHLSTestResponse struct {
	SessionID   string  `json:"session_id"`
	PlaylistURL string  `json:"playlist_url"`
	Start       float64 `json:"start"`
}

func audioFixture(t *testing.T, format string, frequency int) []byte {
	t.Helper()
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		t.Skip("ffmpeg not available")
	}
	codec := []string{}
	if format == "wav" {
		codec = []string{"-c:a", "pcm_s16le"}
	} else if format == "mp3" {
		codec = []string{"-c:a", "libmp3lame", "-b:a", "96k"}
	}
	args := []string{"-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "sine=frequency=" + strconv.Itoa(frequency) + ":duration=0.25", "-ar", "44100", "-ac", "1"}
	args = append(args, codec...)
	args = append(args, "-f", format, "pipe:1")
	out, err := exec.Command("ffmpeg", args...).Output()
	if err != nil {
		t.Fatalf("create %s fixture: %v", format, err)
	}
	return out
}

func waitAudioMerge(t *testing.T, a *testApp, job audioMergeSnapshot) audioMergeSnapshot {
	t.Helper()
	deadline := time.Now().Add(15 * time.Second)
	for !strings.Contains(" done failed cancelled ", " "+job.Status+" ") && time.Now().Before(deadline) {
		time.Sleep(40 * time.Millisecond)
		rr := a.request("GET", "/api/audio-merges/"+job.ID, nil, true)
		if rr.Code != http.StatusOK {
			t.Fatalf("poll audio merge=%d: %s", rr.Code, rr.Body.String())
		}
		job = decode[audioMergeSnapshot](t, rr)
	}
	return job
}

func TestAudioMergeOutputFormats(t *testing.T) {
	if _, err := exec.LookPath("ffprobe"); err != nil {
		t.Skip("ffprobe not available")
	}
	a := newTestApp(t)
	a.srv.cfg.DataDir = t.TempDir()
	first := a.readyFile(t, "01 耳语.wav", audioFixture(t, "wav", 440))
	second := a.readyFile(t, "02 敲击.mp3", audioFixture(t, "mp3", 880))

	for _, tc := range []struct {
		format, name, mime, codec, extension string
		chapters                             bool
	}{
		{format: "", name: "完整 ASMR.flac", mime: "audio/flac", codec: "flac", extension: ".flac"},
		{format: "alac", name: "完整 ASMR ALAC.m4a", mime: "audio/mp4", codec: "alac", extension: ".m4a", chapters: true},
		{format: "aac", name: "完整 ASMR AAC.m4a", mime: "audio/mp4", codec: "aac", extension: ".m4a", chapters: true},
	} {
		t.Run(tc.codec, func(t *testing.T) {
			createdRR := a.request("POST", "/api/audio-merges", map[string]any{
				"parent_id": RootID,
				"name":      tc.name,
				"format":    tc.format,
				"file_ids":  []string{first.ID, second.ID},
			}, true)
			if createdRR.Code != http.StatusAccepted {
				t.Fatalf("create audio merge=%d: %s", createdRR.Code, createdRR.Body.String())
			}
			job := waitAudioMerge(t, a, decode[audioMergeSnapshot](t, createdRR))
			if job.Status != "done" || job.Progress != 100 {
				t.Fatalf("audio merge did not finish: %+v", job)
			}
			wantedFormat := tc.format
			if wantedFormat == "" {
				wantedFormat = "flac"
			}
			if job.OutputFormat != wantedFormat {
				t.Fatalf("output format=%q want %q", job.OutputFormat, wantedFormat)
			}
			merged, err := a.srv.file(context.Background(), job.OutputFileID)
			if err != nil {
				t.Fatal(err)
			}
			if merged.Status != "ready" || merged.MimeType != tc.mime || merged.Size <= 0 {
				t.Fatalf("merged file=%+v", merged)
			}
			rc, err := a.store.Open(context.Background(), merged.objectKey)
			if err != nil {
				t.Fatal(err)
			}
			data, err := io.ReadAll(rc)
			rc.Close()
			if err != nil {
				t.Fatal(err)
			}
			path := t.TempDir() + "/merged" + tc.extension
			if err := os.WriteFile(path, data, 0o600); err != nil {
				t.Fatal(err)
			}
			probe, err := exec.Command("ffprobe", "-v", "error", "-select_streams", "a:0", "-show_entries", "stream=codec_name,sample_rate,channels:chapter_tags=title", "-of", "json", path).Output()
			if err != nil {
				t.Fatalf("probe merged audio: %v", err)
			}
			var info struct {
				Streams []struct {
					CodecName  string `json:"codec_name"`
					SampleRate string `json:"sample_rate"`
					Channels   int    `json:"channels"`
				} `json:"streams"`
				Chapters []struct {
					Tags struct {
						Title string `json:"title"`
					} `json:"tags"`
				} `json:"chapters"`
			}
			if err := json.Unmarshal(probe, &info); err != nil || len(info.Streams) != 1 {
				t.Fatalf("decode probe: %v %s", err, probe)
			}
			if stream := info.Streams[0]; stream.CodecName != tc.codec || stream.SampleRate != "44100" || stream.Channels != 1 {
				t.Fatalf("output stream=%+v", stream)
			}
			if tc.chapters && (len(info.Chapters) != 2 || info.Chapters[0].Tags.Title != "01 耳语" || info.Chapters[1].Tags.Title != "02 敲击") {
				t.Fatalf("ordered chapters missing: %+v", info.Chapters)
			}
			mediaRR := a.request("GET", "/api/files/"+merged.ID+"/audio", nil, true)
			if mediaRR.Code != http.StatusOK {
				t.Fatalf("audio metadata=%d: %s", mediaRR.Code, mediaRR.Body.String())
			}
			media := decode[audioMediaTestResponse](t, mediaRR)
			if media.Duration <= 0 || media.StreamSize <= 0 || len(media.Chapters) != 2 || media.Chapters[0].Title != "01 耳语" || media.Chapters[1].Title != "02 敲击" {
				t.Fatalf("audio metadata=%+v", media)
			}
			streamRR := a.request("GET", media.StreamURL, nil, true)
			if streamRR.Code != http.StatusOK || streamRR.Header().Get("Content-Type") != tc.mime {
				t.Fatalf("audio stream=%d type=%q: %s", streamRR.Code, streamRR.Header().Get("Content-Type"), streamRR.Body.String())
			}
			if !bytes.Equal(streamRR.Body.Bytes(), data) || media.StreamSize != merged.Size {
				t.Fatal("Range playback source is not the merged master")
			}
			streamPath := t.TempDir() + "/stream" + tc.extension
			if err := os.WriteFile(streamPath, streamRR.Body.Bytes(), 0o600); err != nil {
				t.Fatal(err)
			}
			streamCodec, err := exec.Command("ffprobe", "-v", "error", "-select_streams", "a:0", "-show_entries", "stream=codec_name", "-of", "default=nw=1:nk=1", streamPath).Output()
			if err != nil || strings.TrimSpace(string(streamCodec)) != tc.codec {
				t.Fatalf("direct stream codec=%q want=%q err=%v", streamCodec, tc.codec, err)
			}
			rangeRR := a.requestH("GET", media.StreamURL, nil, true, map[string]string{"Range": "bytes=0-63"})
			if rangeRR.Code != http.StatusPartialContent || rangeRR.Body.Len() != 64 || rangeRR.Header().Get("Accept-Ranges") != "bytes" {
				t.Fatalf("audio stream range=%d len=%d accept=%q", rangeRR.Code, rangeRR.Body.Len(), rangeRR.Header().Get("Accept-Ranges"))
			}
		})
	}
}

func TestAudioHLSFallbackStreamsAndCleansUp(t *testing.T) {
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		t.Skip("ffmpeg not available")
	}
	a := newTestApp(t)
	a.srv.cfg.DataDir = t.TempDir()
	first := a.readyFile(t, "01 耳语.wav", audioFixture(t, "wav", 440))
	second := a.readyFile(t, "02 敲击.mp3", audioFixture(t, "mp3", 880))
	createdRR := a.request("POST", "/api/audio-merges", map[string]any{
		"parent_id": RootID, "name": "HLS fallback.m4a", "format": "alac", "file_ids": []string{first.ID, second.ID},
	}, true)
	if createdRR.Code != http.StatusAccepted {
		t.Fatalf("create merge=%d: %s", createdRR.Code, createdRR.Body.String())
	}
	job := waitAudioMerge(t, a, decode[audioMergeSnapshot](t, createdRR))
	if job.Status != "done" {
		t.Fatalf("merge failed: %+v", job)
	}
	startRR := a.request("POST", "/api/files/"+job.OutputFileID+"/audio/hls", map[string]any{"start": 0.1}, true)
	if startRR.Code != http.StatusCreated {
		t.Fatalf("start HLS=%d: %s", startRR.Code, startRR.Body.String())
	}
	started := decode[audioHLSTestResponse](t, startRR)
	if started.SessionID == "" || started.PlaylistURL == "" || started.Start != 0.1 {
		t.Fatalf("invalid HLS response: %+v", started)
	}
	playlistRR := a.request("GET", started.PlaylistURL, nil, true)
	if playlistRR.Code != http.StatusOK || playlistRR.Header().Get("Content-Type") != "application/vnd.apple.mpegurl" {
		t.Fatalf("playlist=%d type=%q: %s", playlistRR.Code, playlistRR.Header().Get("Content-Type"), playlistRR.Body.String())
	}
	var segment string
	for _, line := range strings.Split(playlistRR.Body.String(), "\n") {
		if strings.HasPrefix(line, "segment-") && strings.HasSuffix(line, ".ts") {
			segment = line
			break
		}
	}
	if segment == "" {
		t.Fatalf("playlist has no media segment: %s", playlistRR.Body.String())
	}
	segmentRR := a.request("GET", "/api/audio/hls/"+started.SessionID+"/"+segment, nil, true)
	if segmentRR.Code != http.StatusOK || segmentRR.Header().Get("Content-Type") != "video/mp2t" || segmentRR.Body.Len() == 0 {
		t.Fatalf("segment=%d type=%q bytes=%d", segmentRR.Code, segmentRR.Header().Get("Content-Type"), segmentRR.Body.Len())
	}
	stopRR := a.request("DELETE", "/api/audio/hls/"+started.SessionID, nil, true)
	if stopRR.Code != http.StatusNoContent {
		t.Fatalf("stop HLS=%d: %s", stopRR.Code, stopRR.Body.String())
	}
	missingRR := a.request("GET", started.PlaylistURL, nil, true)
	if missingRR.Code != http.StatusNotFound {
		t.Fatalf("stopped playlist=%d", missingRR.Code)
	}
}

func decodeAudioPCM(t *testing.T, data []byte, extension string) []byte {
	t.Helper()
	path := t.TempDir() + "/audio" + extension
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
	out, err := exec.Command("ffmpeg", "-hide_banner", "-loglevel", "error", "-i", path, "-map", "0:a:0", "-c:a", "pcm_s32le", "-f", "s32le", "pipe:1").Output()
	if err != nil {
		t.Fatalf("decode PCM: %v", err)
	}
	return out
}

func TestAudioMergeLosslessFormatsArePCMExact(t *testing.T) {
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		t.Skip("ffmpeg not available")
	}
	a := newTestApp(t)
	a.srv.cfg.DataDir = t.TempDir()
	firstData := audioFixture(t, "wav", 330)
	secondData := audioFixture(t, "wav", 990)
	first := a.readyFile(t, "01.wav", firstData)
	second := a.readyFile(t, "02.wav", secondData)
	expected := append(decodeAudioPCM(t, firstData, ".wav"), decodeAudioPCM(t, secondData, ".wav")...)

	for _, tc := range []struct{ format, name, extension string }{
		{format: "flac", name: "bit-perfect.flac", extension: ".flac"},
		{format: "alac", name: "bit-perfect.m4a", extension: ".m4a"},
	} {
		t.Run(tc.format, func(t *testing.T) {
			createdRR := a.request("POST", "/api/audio-merges", map[string]any{"parent_id": RootID, "name": tc.name, "format": tc.format, "file_ids": []string{first.ID, second.ID}}, true)
			if createdRR.Code != http.StatusAccepted {
				t.Fatalf("create audio merge=%d: %s", createdRR.Code, createdRR.Body.String())
			}
			job := waitAudioMerge(t, a, decode[audioMergeSnapshot](t, createdRR))
			if job.Status != "done" {
				t.Fatalf("lossless merge failed: %+v", job)
			}
			merged, err := a.srv.file(context.Background(), job.OutputFileID)
			if err != nil {
				t.Fatal(err)
			}
			rc, err := a.store.Open(context.Background(), merged.objectKey)
			if err != nil {
				t.Fatal(err)
			}
			data, err := io.ReadAll(rc)
			rc.Close()
			if err != nil {
				t.Fatal(err)
			}
			actual := decodeAudioPCM(t, data, tc.extension)
			if !bytes.Equal(actual, expected) {
				t.Fatalf("%s changed decoded PCM: got %d bytes, want %d", tc.format, len(actual), len(expected))
			}
		})
	}
}

func audioCoverFixture(t *testing.T) []byte {
	t.Helper()
	img := image.NewRGBA(image.Rect(0, 0, 96, 96))
	for y := 0; y < 96; y++ {
		for x := 0; x < 96; x++ {
			img.Set(x, y, color.RGBA{R: uint8(40 + x), G: uint8(70 + y), B: 180, A: 255})
		}
	}
	var out bytes.Buffer
	if err := jpeg.Encode(&out, img, &jpeg.Options{Quality: 88}); err != nil {
		t.Fatal(err)
	}
	return out.Bytes()
}

func TestAudioMergeEmbedsAndServesCover(t *testing.T) {
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		t.Skip("ffmpeg not available")
	}
	a := newTestApp(t)
	a.srv.cfg.DataDir = t.TempDir()
	first := a.readyFile(t, "01 夜晚.wav", audioFixture(t, "wav", 280))
	second := a.readyFile(t, "02 雨声.wav", audioFixture(t, "wav", 560))
	coverFile := a.readyFile(t, "cover.jpg", audioCoverFixture(t))
	createdRR := a.request("POST", "/api/audio-merges", map[string]any{
		"parent_id": RootID, "name": "带封面的 ASMR.m4a", "format": "alac",
		"file_ids": []string{first.ID, second.ID}, "cover_file_id": coverFile.ID,
	}, true)
	if createdRR.Code != http.StatusAccepted {
		t.Fatalf("create audio merge=%d: %s", createdRR.Code, createdRR.Body.String())
	}
	job := waitAudioMerge(t, a, decode[audioMergeSnapshot](t, createdRR))
	if job.Status != "done" {
		t.Fatalf("covered merge failed: %+v", job)
	}
	mediaRR := a.request("GET", "/api/files/"+job.OutputFileID+"/audio", nil, true)
	if mediaRR.Code != http.StatusOK {
		t.Fatalf("audio metadata=%d: %s", mediaRR.Code, mediaRR.Body.String())
	}
	media := decode[audioMediaTestResponse](t, mediaRR)
	if !media.HasCover || media.CoverURL == "" {
		t.Fatalf("cover metadata missing: %+v", media)
	}
	thumbRR := a.request("GET", media.CoverURL, nil, true)
	if thumbRR.Code != http.StatusOK || thumbRR.Header().Get("Content-Type") != "image/jpeg" || thumbRR.Body.Len() == 0 {
		t.Fatalf("cover thumbnail=%d type=%q len=%d", thumbRR.Code, thumbRR.Header().Get("Content-Type"), thumbRR.Body.Len())
	}
	merged, err := a.srv.file(context.Background(), job.OutputFileID)
	if err != nil {
		t.Fatal(err)
	}
	rc, err := a.store.Open(context.Background(), merged.objectKey)
	if err != nil {
		t.Fatal(err)
	}
	data, err := io.ReadAll(rc)
	rc.Close()
	if err != nil {
		t.Fatal(err)
	}
	path := t.TempDir() + "/covered.m4a"
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
	probe, err := exec.Command("ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name:stream_disposition=attached_pic", "-of", "json", path).Output()
	if err != nil || !bytes.Contains(probe, []byte(`"codec_name": "mjpeg"`)) || !bytes.Contains(probe, []byte(`"attached_pic": 1`)) {
		t.Fatalf("embedded cover missing: err=%v probe=%s", err, probe)
	}
}

func TestAudioCoverFileMustBeInOutputDirectory(t *testing.T) {
	a := newTestApp(t)
	cover := a.readyFile(t, "cover.jpg", audioCoverFixture(t))
	if data, err := a.srv.audioCoverFromFile(context.Background(), RootID, cover.ID); err != nil || len(data) == 0 {
		t.Fatalf("read directory cover: bytes=%d err=%v", len(data), err)
	}
	directoryID := ids.New()
	now := time.Now().UTC().Format(time.RFC3339Nano)
	if _, err := a.db.Exec(`INSERT INTO files(id,parent_id,name,kind,status,created_at,updated_at) VALUES(?,?,?,?,?,?,?)`, directoryID, RootID, "nested", "directory", "ready", now, now); err != nil {
		t.Fatal(err)
	}
	if _, err := a.db.Exec(`UPDATE files SET parent_id=? WHERE id=?`, directoryID, cover.ID); err != nil {
		t.Fatal(err)
	}
	if _, err := a.srv.audioCoverFromFile(context.Background(), RootID, cover.ID); err == nil {
		t.Fatal("cover outside the output directory was accepted")
	}
}

func TestAudioMergeListsMultipleBackgroundJobs(t *testing.T) {
	a := newTestApp(t)
	if capacity := cap(a.srv.audioMergeSlots); capacity != 2 {
		t.Fatalf("audio merge concurrency=%d want=2", capacity)
	}
	older := &audioMergeJob{ID: ids.New(), Status: "done", Progress: 100, OutputName: "older.flac", OutputFormat: "flac", ParentID: RootID, InputCount: 2, CreatedAt: "2026-08-23T01:00:00Z", UpdatedAt: "2026-08-23T01:01:00Z"}
	newer := &audioMergeJob{ID: ids.New(), Status: "queued", Progress: 1, OutputName: "newer.m4a", OutputFormat: "alac", ParentID: RootID, InputCount: 7, CreatedAt: "2026-08-23T02:00:00Z", UpdatedAt: "2026-08-23T02:00:00Z"}
	a.srv.audioMergeJobs[older.ID] = older
	a.srv.audioMergeJobs[newer.ID] = newer
	rr := a.request("GET", "/api/audio-merges", nil, true)
	if rr.Code != http.StatusOK {
		t.Fatalf("list audio merges=%d: %s", rr.Code, rr.Body.String())
	}
	listed := decode[struct {
		Items []audioMergeSnapshot `json:"items"`
	}](t, rr)
	if len(listed.Items) != 2 || listed.Items[0].ID != newer.ID || listed.Items[1].ID != older.ID || listed.Items[0].ParentID != RootID {
		t.Fatalf("audio merge list=%+v", listed.Items)
	}
	if removed := a.request("DELETE", "/api/audio-merges/"+older.ID, nil, true); removed.Code != http.StatusNoContent {
		t.Fatalf("remove finished audio merge=%d", removed.Code)
	}
	if _, ok := a.srv.audioMergeJobs[older.ID]; ok {
		t.Fatal("finished audio merge remained in task history")
	}
}

func TestNormalizeAudioLayoutDoesNotSilentlyDownmix(t *testing.T) {
	for _, tc := range []struct {
		layout   string
		channels int
		want     string
		ok       bool
	}{
		{channels: 1, want: "mono", ok: true},
		{channels: 2, want: "stereo", ok: true},
		{layout: "5.1(side)", channels: 6, want: "5.1(side)", ok: true},
		{layout: "unknown", channels: 6},
		{layout: "5.1;movie", channels: 6},
	} {
		got, ok := normalizeAudioLayout(tc.layout, tc.channels)
		if got != tc.want || ok != tc.ok {
			t.Fatalf("normalizeAudioLayout(%q, %d)=(%q, %t), want (%q, %t)", tc.layout, tc.channels, got, ok, tc.want, tc.ok)
		}
	}
}

func TestAudioMergeValidatesSelection(t *testing.T) {
	a := newTestApp(t)
	a.srv.cfg.FFmpegPath = "definitely-missing-ffmpeg"
	audio := a.readyFile(t, "voice.wav", []byte("not needed for validation"))
	document := a.readyFile(t, "notes.txt", []byte("text"))

	for name, body := range map[string]map[string]any{
		"one input":       {"parent_id": RootID, "name": "one.flac", "format": "flac", "file_ids": []string{audio.ID}},
		"duplicate":       {"parent_id": RootID, "name": "duplicate.flac", "format": "flac", "file_ids": []string{audio.ID, audio.ID}},
		"non audio":       {"parent_id": RootID, "name": "mixed.flac", "format": "flac", "file_ids": []string{audio.ID, document.ID}},
		"wrong extension": {"parent_id": RootID, "name": "wrong.m4a", "format": "flac", "file_ids": []string{audio.ID, ids.New()}},
		"unknown format":  {"parent_id": RootID, "name": "wrong.wav", "format": "pcm", "file_ids": []string{audio.ID, ids.New()}},
	} {
		t.Run(name, func(t *testing.T) {
			rr := a.request("POST", "/api/audio-merges", body, true)
			if rr.Code < 400 || rr.Code >= 500 {
				t.Fatalf("validation status=%d: %s", rr.Code, rr.Body.String())
			}
		})
	}
}

func TestNewCleansOnlyInterruptedAudioMergePlaceholders(t *testing.T) {
	a := newTestApp(t)
	now := time.Now().UTC().Format(time.RFC3339Nano)
	interruptedID := ids.New()
	if _, err := a.db.Exec(`INSERT INTO files(id,parent_id,name,kind,size,mime_type,status,created_at,updated_at) VALUES(?,?,?,?,0,'audio/mp4','pending',?,?)`, interruptedID, RootID, "interrupted.m4a", "file", now, now); err != nil {
		t.Fatal(err)
	}
	interruptedFLACID := ids.New()
	if _, err := a.db.Exec(`INSERT INTO files(id,parent_id,name,kind,size,mime_type,status,created_at,updated_at) VALUES(?,?,?,?,0,'audio/flac','pending',?,?)`, interruptedFLACID, RootID, "interrupted.flac", "file", now, now); err != nil {
		t.Fatal(err)
	}
	uploadRR := a.request("POST", "/api/uploads", map[string]any{"parent_id": RootID, "name": "uploading.m4a", "size": 10, "mime_type": "audio/mp4"}, true)
	if uploadRR.Code != http.StatusCreated {
		t.Fatalf("create regular upload=%d: %s", uploadRR.Code, uploadRR.Body.String())
	}
	upload := decode[createdUpload](t, uploadRR)

	_ = New(a.db, a.store, a.srv.auth, a.srv.cfg, nil)
	var interrupted, uploading int
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM files WHERE id IN (?,?)`, interruptedID, interruptedFLACID).Scan(&interrupted); err != nil {
		t.Fatal(err)
	}
	if err := a.db.QueryRow(`SELECT COUNT(*) FROM files WHERE id=?`, upload.FileID).Scan(&uploading); err != nil {
		t.Fatal(err)
	}
	if interrupted != 0 || uploading != 1 {
		t.Fatalf("cleanup interrupted=%d regular_upload=%d", interrupted, uploading)
	}
}
