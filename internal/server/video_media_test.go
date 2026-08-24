package server

import (
	"bytes"
	"context"
	"net/http"
	"strings"
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

func TestWebVTTSubtitlePassThrough(t *testing.T) {
	app := newTestApp(t)
	want := []byte("WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nhello\n")
	input := append([]byte{0xef, 0xbb, 0xbf}, append([]byte("\n"), want...)...)
	got, err := app.srv.subtitleAsWebVTT(context.Background(), "Movie.vtt", input)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(got, want) {
		t.Fatalf("got=%q want=%q", got, want)
	}
}

func TestVideoSubtitleAPI(t *testing.T) {
	app := newTestApp(t)
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
}
