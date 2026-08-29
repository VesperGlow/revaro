package server

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
	"time"
)

func TestFMP4StartResponseUsesOneStreamingURL(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	session := &videoFMP4Session{
		ID: "session", RequestedStart: 4, Duration: 12,
		MIMEType: `video/mp4; codecs="hvc1, mp4a.40.2"`, VideoContentType: `video/mp4; codecs="hvc1"`,
		VideoCodec: "hevc", AudioCodec: "eac3", OutputAudioCodec: "aac", AudioTranscoding: true,
		ctx: ctx, cancel: cancel,
	}
	body, err := json.Marshal(videoFMP4Response(session))
	if err != nil {
		t.Fatal(err)
	}
	text := string(body)
	for _, field := range []string{`"duration":12`, `"stream_url":"/api/video/fmp4/session/stream"`, `"requested_start":4`, `"selected_mode":"mse-copy-video-aac-audio"`} {
		if !strings.Contains(text, field) {
			t.Fatalf("response %s does not contain %s", text, field)
		}
	}
	for _, removed := range []string{"index_url", "prewarm_url", "init_url", "fragments"} {
		if strings.Contains(text, removed) {
			t.Fatalf("response still exposes removed window field %q: %s", removed, text)
		}
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

func TestFMP4SessionDestroyCancelsActiveStream(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	streamCtx, streamCancel := context.WithCancel(context.Background())
	session := &videoFMP4Session{ctx: ctx, cancel: cancel, lastAccess: time.Now()}
	if !session.beginStream(streamCancel) {
		t.Fatal("stream did not start")
	}
	session.destroy()
	select {
	case <-streamCtx.Done():
	case <-time.After(time.Second):
		t.Fatal("destroy did not cancel active stdout stream")
	}
	if ctx.Err() == nil {
		t.Fatal("destroy did not cancel session context")
	}
}
