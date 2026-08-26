package server

import (
	"context"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
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

func TestFMP4CopyCommandStreamsStdoutWithoutWindowsOrEOFBound(t *testing.T) {
	session := &videoFMP4Session{RequestedStart: 2370, VideoCodec: "hevc", AudioCodec: "aac", AudioMode: fmp4AudioCopy}
	rawArgs := videoFMP4Args("source", session)
	args := strings.Join(rawArgs, " ")
	if !strings.Contains(args, "-c:v copy") || !strings.Contains(args, "-c:a copy") || strings.Contains(args, "libx264") {
		t.Fatalf("unexpected HEVC+AAC args: %s", args)
	}
	if !strings.Contains(args, "-movflags frag_keyframe+empty_moov+default_base_moof") || !strings.HasSuffix(args, "-f mp4 pipe:1") {
		t.Fatalf("fragmented MP4 stdout arguments missing: %s", args)
	}
	if indexOfArgument(rawArgs, "-ss") >= indexOfArgument(rawArgs, "-i") || !strings.Contains(args, "-ss 2370.000") {
		t.Fatalf("input-side seek missing: %v", rawArgs)
	}
	for _, forbidden := range []string{"-to", "-t", "-f hls", "m3u8", "segment_filename"} {
		if indexOfArgument(rawArgs, forbidden) < len(rawArgs) || strings.Contains(args, " "+forbidden+" ") {
			t.Fatalf("stdout stream contains fixed-window argument %q: %s", forbidden, args)
		}
	}
}

func TestFMP4AACCommandOnlyTranscodesAudio(t *testing.T) {
	session := &videoFMP4Session{VideoCodec: "hevc", AudioCodec: "eac3", AudioMode: fmp4AudioAAC, AudioTranscoding: true}
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

func TestFMP4StdoutUsesLocalClockForMSETimestampOffset(t *testing.T) {
	ffmpeg, err := exec.LookPath("ffmpeg")
	if err != nil {
		t.Skip("ffmpeg is unavailable")
	}
	ffprobe, err := exec.LookPath("ffprobe")
	if err != nil {
		t.Skip("ffprobe is unavailable")
	}
	dir := t.TempDir()
	source := filepath.Join(dir, "source.mp4")
	generate := exec.Command(ffmpeg, "-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=160x90:rate=10", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000", "-t", "12", "-c:v", "libx264", "-g", "20", "-c:a", "aac", source)
	if output, generateErr := generate.CombinedOutput(); generateErr != nil {
		t.Skipf("cannot create fixture: %v: %s", generateErr, output)
	}
	outputPath := filepath.Join(dir, "stream.mp4")
	output, err := os.Create(outputPath)
	if err != nil {
		t.Fatal(err)
	}
	session := &videoFMP4Session{RequestedStart: 7, VideoCodec: "h264", AudioCodec: "aac", AudioMode: fmp4AudioCopy}
	remux := exec.Command(ffmpeg, videoFMP4Args(source, session)...)
	remux.Stdout = output
	stderr := &limitedBuffer{limit: 64 << 10}
	remux.Stderr = stderr
	remuxErr := remux.Run()
	_ = output.Close()
	if remuxErr != nil {
		t.Fatalf("stdout remux failed: %v: %s", remuxErr, stderr.String())
	}
	probe := exec.Command(ffprobe, "-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts_time", "-of", "csv=p=0", "-read_intervals", "%+#1", outputPath)
	ptsRaw, probeErr := probe.Output()
	if probeErr != nil {
		t.Fatal(probeErr)
	}
	pts, parseErr := strconv.ParseFloat(strings.TrimSpace(string(ptsRaw)), 64)
	if parseErr != nil || pts < 0 || pts >= 2.5 {
		t.Fatalf("first stdout packet PTS=%q (%v), want a local stream clock", ptsRaw, parseErr)
	}
}

func indexOfArgument(args []string, value string) int {
	for index, arg := range args {
		if arg == value {
			return index
		}
	}
	return len(args)
}
