package server

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestFMP4StartResponseFlattensMetadata(t *testing.T) {
	session := &videoFMP4Session{ID: "session", Duration: 12, MIMEType: `video/mp4; codecs="hvc1, mp4a.40.2"`, VideoContentType: `video/mp4; codecs="hvc1"`, VideoCodec: "hevc", AudioCodec: "eac3", OutputAudioCodec: "aac", AudioTranscoding: true}
	body, err := json.Marshal(videoFMP4Response(session, 4))
	if err != nil {
		t.Fatal(err)
	}
	text := string(body)
	for _, field := range []string{`"duration":12`, `"init_url":"/api/video/fmp4/session/init.mp4"`, `"selected_mode":"mse-copy-video-aac-audio"`} {
		if !strings.Contains(text, field) {
			t.Fatalf("response %s does not contain %s", text, field)
		}
	}
}

func TestFMP4FragmentIndexBuildsTimeMap(t *testing.T) {
	dir := t.TempDir()
	playlist := filepath.Join(dir, "index.m3u8")
	body := "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:2.000000,\nfragment-000000.m4s\n#EXTINF:3.500000,\nfragment-000001.m4s\n"
	if err := os.WriteFile(playlist, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	fragments, available := fmp4FragmentIndex(playlist, "session")
	if len(fragments) != 2 || fragments[0].Start != 0 || fragments[1].Start != 2 || available != 5.5 {
		t.Fatalf("fragments=%+v available=%v", fragments, available)
	}
	if fragments[1].URL != "/api/video/fmp4/session/fragment-000001.m4s" {
		t.Fatalf("unexpected fragment URL %q", fragments[1].URL)
	}
}

func TestFMP4CodecStringsPreserveHEVCAndEAC3(t *testing.T) {
	video, err := fmp4VideoCodecString("hevc", "Main 10", 120)
	if err != nil || video != "hvc1.2.4.L120.B0" {
		t.Fatalf("HEVC codec=%q error=%v", video, err)
	}
	audio, err := fmp4AudioCodecString("eac3")
	if err != nil || audio != "ec-3" {
		t.Fatalf("EAC3 codec=%q error=%v", audio, err)
	}
	if _, err := fmp4VideoCodecString("vp9", "", 0); err == nil || !strings.Contains(err.Error(), "vp9") {
		t.Fatalf("unsupported codec error=%v", err)
	}
}

func TestFMP4CopyCommandNeverUsesLibx264(t *testing.T) {
	session := &videoFMP4Session{Dir: t.TempDir(), Playlist: "index.m3u8", VideoCodec: "hevc", AudioCodec: "aac", AudioMode: fmp4AudioCopy}
	args := strings.Join(videoFMP4Args("source", session), " ")
	if !strings.Contains(args, "-c:v copy") || !strings.Contains(args, "-c:a copy") || strings.Contains(args, "libx264") {
		t.Fatalf("unexpected HEVC+AAC args: %s", args)
	}
	if !strings.Contains(args, "-hls_segment_type fmp4") || !strings.Contains(args, "init.mp4") || !strings.Contains(args, "fragment-%06d.m4s") {
		t.Fatalf("fragmented MP4 arguments missing: %s", args)
	}
}

func TestFMP4AACCommandOnlyTranscodesAudio(t *testing.T) {
	session := &videoFMP4Session{Dir: t.TempDir(), Playlist: "index.m3u8", VideoCodec: "hevc", AudioCodec: "eac3", AudioMode: fmp4AudioAAC, AudioTranscoding: true}
	args := strings.Join(videoFMP4Args("source", session), " ")
	if !strings.Contains(args, "-c:v copy") || !strings.Contains(args, "-c:a aac") || strings.Contains(args, "libx264") {
		t.Fatalf("unexpected HEVC+EAC3 args: %s", args)
	}
}

func TestFMP4OutputTypesSelectAACWithoutChangingVideo(t *testing.T) {
	info := fmp4MediaInfo{
		videoCodec: "hevc", audioCodec: "eac3", videoContentType: `video/mp4; codecs="hvc1"`,
		mimeType: `video/mp4; codecs="hvc1, ec-3"`, aacMIMEType: `video/mp4; codecs="hvc1, mp4a.40.2"`,
		audioContentType: `audio/mp4; codecs="ec-3"`, aacAudioContentType: `audio/mp4; codecs="mp4a.40.2"`,
	}
	mimeType, audioType, outputCodec := fmp4OutputTypes(info, fmp4AudioAAC)
	if mimeType != info.aacMIMEType || audioType != info.aacAudioContentType || outputCodec != "aac" {
		t.Fatalf("AAC output=(%q,%q,%q)", mimeType, audioType, outputCodec)
	}
}

func TestFMP4SessionCacheReusesFileAndAudioMode(t *testing.T) {
	dir := t.TempDir()
	playlist := filepath.Join(dir, "index.m3u8")
	if err := os.WriteFile(playlist, []byte("#EXTM3U\n#EXTINF:2,\nfragment-000000.m4s\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	session := &videoFMP4Session{ID: "cached", FileID: "file", AudioMode: fmp4AudioAAC, Playlist: playlist, lastAccess: time.Now()}
	server := &Server{videoFMP4Sessions: map[string]*videoFMP4Session{session.ID: session}}
	if got := server.reusableVideoFMP4Session("file", fmp4AudioAAC, "cached"); got != session {
		t.Fatalf("cached session was not reused: %v", got)
	}
	if got := server.reusableVideoFMP4Session("file", fmp4AudioCopy, "cached"); got != nil {
		t.Fatalf("session with a different audio mode was reused: %v", got)
	}
}
