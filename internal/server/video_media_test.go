package server

import (
	"context"
	"errors"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
)

func TestVideoSubtitleMatchPriority(t *testing.T) {
	tests := []struct {
		subtitle string
		priority int
		match    bool
	}{
		{"Movie.vtt", 0, true},
		{"Movie.mkv.srt", 1, true},
		{"Movie.zh-CN.ass", 2, true},
		{"Movie [简日].ssa", 2, true},
		{"Other.srt", 0, false},
		{"Movie.txt", 0, false},
	}
	for _, test := range tests {
		priority, match := videoSubtitleMatchPriority("Movie.mkv", test.subtitle)
		if priority != test.priority || match != test.match {
			t.Errorf("subtitle %q=(%d,%v), want (%d,%v)", test.subtitle, priority, match, test.priority, test.match)
		}
	}
}

func TestVideoSubtitleLanguage(t *testing.T) {
	for _, test := range []struct{ name, language string }{
		{"Movie.chs.srt", "zh-CN"}, {"Movie.zh-TW.ass", "zh-TW"},
		{"Movie.en.vtt", "en"}, {"Movie.jpn.srt", "ja"},
	} {
		language, _ := videoSubtitleLanguage("Movie.mkv", test.name)
		if language != test.language {
			t.Errorf("subtitle %q language=%q, want %q", test.name, language, test.language)
		}
	}
}

func TestVideoSubtitleConversionContinuesAfterRequestCancellationAndIsCached(t *testing.T) {
	app := newTestApp(t)
	started := make(chan struct{})
	release := make(chan struct{})
	var conversions atomic.Int32
	convert := func(context.Context) ([]byte, error) {
		conversions.Add(1)
		close(started)
		<-release
		return []byte("WEBVTT\n"), nil
	}

	requestCtx, cancel := context.WithCancel(context.Background())
	result := make(chan error, 1)
	go func() {
		_, err := app.srv.cachedVideoSubtitle(requestCtx, "video:track", convert)
		result <- err
	}()
	<-started
	cancel()
	if err := <-result; !errors.Is(err, context.Canceled) {
		t.Fatalf("first request error=%v, want context canceled", err)
	}
	close(release)

	got, err := app.srv.cachedVideoSubtitle(context.Background(), "video:track", convert)
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != "WEBVTT\n" || conversions.Load() != 1 {
		t.Fatalf("got=%q conversions=%d, want one cached conversion", got, conversions.Load())
	}
}

func TestOffsetVideoSubtitleUsesHLSSessionTimeline(t *testing.T) {
	input := []byte("WEBVTT\n\nold\n00:00:05.000 --> 00:00:09.000\nold\n\ncrossing\n00:00:09.500 --> 00:00:11.000 line:90%\ncross\n\nafter\n00:01:12.250 --> 00:01:14.500\nafter\n")
	got := string(offsetVideoSubtitle(input, 10))
	if strings.Contains(got, "old\n") {
		t.Fatalf("expired cue was retained: %q", got)
	}
	for _, want := range []string{
		"00:00:00.000 --> 00:00:01.000 line:90%",
		"00:01:02.250 --> 00:01:04.500",
		"WEBVTT",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("offset subtitle missing %q: %q", want, got)
		}
	}
}

func TestOffsetVideoSubtitleLeavesCanonicalCacheUnchanged(t *testing.T) {
	input := []byte("WEBVTT\n\n00:00:20.000 --> 00:00:21.000\nhello\n")
	got := offsetVideoSubtitle(input, 15)
	if !strings.Contains(string(got), "00:00:05.000 --> 00:00:06.000") {
		t.Fatalf("unexpected derived track: %q", got)
	}
	if !strings.Contains(string(input), "00:00:20.000 --> 00:00:21.000") {
		t.Fatalf("canonical cache was mutated: %q", input)
	}
}

func TestVideoSubtitleAPI(t *testing.T) {
	app := newTestApp(t)
	app.requireMediaEngine(t)
	video := app.readyFile(t, "Movie.mkv", []byte("not needed by metadata endpoint"))
	subtitle := app.readyFile(t, "Movie.zh-CN.vtt", []byte("WEBVTT\n\n00:00:00.000 --> 00:00:01.000\n你好\n"))
	app.readyFile(t, "Other.srt", []byte("unrelated"))

	info := app.request("GET", "/api/files/"+video.ID+"/video", nil, true)
	if info.Code != http.StatusOK {
		t.Fatalf("video info=%d: %s", info.Code, info.Body.String())
	}
	var payload struct {
		Subtitles []videoSubtitleResponse `json:"subtitles"`
	}
	payload = decode[struct {
		Subtitles []videoSubtitleResponse `json:"subtitles"`
	}](t, info)
	if len(payload.Subtitles) != 1 || payload.Subtitles[0].ID != subtitle.ID || payload.Subtitles[0].Language != "zh-CN" {
		t.Fatalf("subtitles=%+v", payload.Subtitles)
	}
	track := app.request("GET", payload.Subtitles[0].URL, nil, true)
	if track.Code != http.StatusOK || track.Header().Get("Content-Type") != "text/vtt; charset=utf-8" || !strings.Contains(track.Body.String(), "你好") {
		t.Fatalf("subtitle track=%d type=%q body=%q", track.Code, track.Header().Get("Content-Type"), track.Body.String())
	}
	offsetTrack := app.request("GET", payload.Subtitles[0].URL+"?start=0.500", nil, true)
	if offsetTrack.Code != http.StatusOK || !strings.Contains(offsetTrack.Body.String(), "00:00:00.000 --> 00:00:00.500") {
		t.Fatalf("offset subtitle track=%d body=%q", offsetTrack.Code, offsetTrack.Body.String())
	}
}

func TestEmbeddedASSSubtitleAPIUsesGlobalMSETimelineAndHLSSessionOffset(t *testing.T) {
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		t.Skip("ffmpeg not available")
	}
	if _, err := exec.LookPath("ffprobe"); err != nil {
		t.Skip("ffprobe not available")
	}
	dir := t.TempDir()
	assPath := filepath.Join(dir, "subtitle.ass")
	mkvPath := filepath.Join(dir, "movie.mkv")
	ass := `[Script Info]
ScriptType: v4.00+
PlayResX: 640
PlayResY: 360

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Arial,24,&H00FFFFFF,&H000000FF,&H00000000,&H64000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,10,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,开始字幕
Dialogue: 0,0:06:15.00,0:06:20.00,Default,,0,0,0,,六分钟字幕
`
	if err := os.WriteFile(assPath, []byte(ass), 0o600); err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command("ffmpeg", "-hide_banner", "-loglevel", "error",
		"-f", "lavfi", "-i", "color=c=black:s=160x90:r=1:d=1", "-i", assPath,
		"-map", "0:v:0", "-map", "1:s:0", "-c:v", "mpeg4", "-c:s", "ass",
		"-metadata:s:s:0", "language=chi", "-metadata:s:s:0", "title=简体 ASS",
		"-disposition:s:0", "default", mkvPath)
	if output, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("create embedded ASS fixture: %v: %s", err, output)
	}
	data, err := os.ReadFile(mkvPath)
	if err != nil {
		t.Fatal(err)
	}
	app := newTestApp(t)
	app.requireMediaEngine(t)
	video := app.readyFile(t, "Movie.mkv", data)
	info := app.request("GET", "/api/files/"+video.ID+"/video", nil, true)
	if info.Code != http.StatusOK {
		t.Fatalf("video info=%d: %s", info.Code, info.Body.String())
	}
	payload := decode[struct {
		Subtitles []videoSubtitleResponse `json:"subtitles"`
	}](t, info)
	if len(payload.Subtitles) != 1 || payload.Subtitles[0].ID != "embedded-1" || payload.Subtitles[0].Language != "zh-CN" {
		t.Fatalf("embedded subtitles=%+v", payload.Subtitles)
	}
	global := app.request("GET", payload.Subtitles[0].URL, nil, true)
	if global.Code != http.StatusOK || global.Header().Get("Content-Type") != "text/vtt; charset=utf-8" {
		t.Fatalf("global subtitle=%d type=%q body=%q", global.Code, global.Header().Get("Content-Type"), global.Body.String())
	}
	for _, cue := range []string{"00:00.000 --> 00:01.000", "06:15.000 --> 06:20.000", "六分钟字幕"} {
		if !strings.Contains(global.Body.String(), cue) {
			t.Fatalf("global MSE subtitle missing %q: %q", cue, global.Body.String())
		}
	}
	hls := app.request("GET", payload.Subtitles[0].URL+"?start=360.000", nil, true)
	if hls.Code != http.StatusOK || !strings.Contains(hls.Body.String(), "00:00:15.000 --> 00:00:20.000") || strings.Contains(hls.Body.String(), "开始字幕") {
		t.Fatalf("HLS offset subtitle=%d body=%q", hls.Code, hls.Body.String())
	}
}
