package server

import (
	"bytes"
	"context"
	"encoding/json"
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
		})
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
