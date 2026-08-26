package server

import (
	"context"
	"encoding/json"
	"log/slog"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
	"testing"
	"time"
)

func TestFMP4StartResponseFlattensMetadata(t *testing.T) {
	session := &videoFMP4Session{ID: "session", Duration: 12, MIMEType: `video/mp4; codecs="hvc1, mp4a.40.2"`, VideoContentType: `video/mp4; codecs="hvc1"`, VideoCodec: "hevc", AudioCodec: "eac3", OutputAudioCodec: "aac", AudioTranscoding: true}
	window := &videoFMP4Window{InitAsset: "init-w0000000000000-000001.mp4"}
	body, err := json.Marshal(videoFMP4Response(session, window, 4))
	if err != nil {
		t.Fatal(err)
	}
	text := string(body)
	for _, field := range []string{`"duration":12`, `"init_url":"/api/video/fmp4/session/init-w0000000000000-000001.mp4"`, `"selected_mode":"mse-copy-video-aac-audio"`} {
		if !strings.Contains(text, field) {
			t.Fatalf("response %s does not contain %s", text, field)
		}
	}
}

func TestFMP4FragmentIndexBuildsTimeMap(t *testing.T) {
	dir := t.TempDir()
	playlist := filepath.Join(dir, "index.m3u8")
	key := "w0000000120000-000001"
	body := "#EXTM3U\n#EXT-X-MAP:URI=\"init-" + key + ".mp4\"\n#EXTINF:2.000000,\nfragment-" + key + "-000000.m4s\n#EXTINF:3.500000,\nfragment-" + key + "-000001.m4s\n"
	if err := os.WriteFile(playlist, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	fragments, available := fmp4FragmentIndex(playlist, "session", 120, "init-"+key+".mp4")
	if len(fragments) != 2 || fragments[0].Start != 120 || fragments[1].Start != 122 || available != 125.5 {
		t.Fatalf("fragments=%+v available=%v", fragments, available)
	}
	if fragments[1].URL != "/api/video/fmp4/session/fragment-"+key+"-000001.m4s" || fragments[1].InitURL != "/api/video/fmp4/session/init-"+key+".mp4" {
		t.Fatalf("unexpected fragment URL %q", fragments[1].URL)
	}
	if fragments[1].WindowStart != 120 || fragments[1].TimestampOffset != 0 {
		t.Fatalf("fragment time mapping=%+v", fragments[1])
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
	session := &videoFMP4Session{Dir: t.TempDir(), VideoCodec: "hevc", AudioCodec: "aac", AudioMode: fmp4AudioCopy}
	window := testFMP4Window(session.Dir, 2370, videoFMP4WindowSize.Seconds(), "w0000002370000-000001")
	rawArgs := videoFMP4Args("source", session, window)
	args := strings.Join(rawArgs, " ")
	if !strings.Contains(args, "-c:v copy") || !strings.Contains(args, "-c:a copy") || strings.Contains(args, "libx264") {
		t.Fatalf("unexpected HEVC+AAC args: %s", args)
	}
	if !strings.Contains(args, "-hls_segment_type fmp4") || !strings.Contains(args, window.InitAsset) || !strings.Contains(args, "fragment-"+window.Key+"-%06d.m4s") {
		t.Fatalf("fragmented MP4 arguments missing: %s", args)
	}
	if indexOfArgument(rawArgs, "-ss") >= indexOfArgument(rawArgs, "-i") || !strings.Contains(args, "-ss 2370.000") || !strings.Contains(args, "-to 2430.000") || !strings.Contains(args, "-copyts") {
		t.Fatalf("input-side finite window seek missing: %v", rawArgs)
	}
	if strings.Contains(args, " -re ") {
		t.Fatalf("windowing must not use -re: %s", args)
	}
}

func TestFMP4WindowRemuxPreservesGlobalTimestampsForCopiedVideoAndAAC(t *testing.T) {
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
	generate := exec.Command(ffmpeg, "-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=160x90:rate=10", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000", "-t", "12", "-c:v", "libx264", "-g", "20", "-c:a", "flac", source)
	if output, generateErr := generate.CombinedOutput(); generateErr != nil {
		t.Skipf("test ffmpeg cannot create H.264 fixture: %v: %s", generateErr, output)
	}
	session := &videoFMP4Session{Dir: dir, VideoCodec: "h264", AudioCodec: "flac", AudioMode: fmp4AudioAAC, AudioTranscoding: true}
	window := testFMP4Window(dir, 7, 4, "w0000000007000-000001")
	remux := exec.Command(ffmpeg, videoFMP4Args(source, session, window)...)
	if output, remuxErr := remux.CombinedOutput(); remuxErr != nil {
		t.Fatalf("window remux failed: %v: %s", remuxErr, output)
	}
	for _, stream := range []string{"v:0", "a:0"} {
		probe := exec.Command(ffprobe, "-v", "error", "-select_streams", stream, "-show_entries", "packet=pts_time", "-of", "csv=p=0", window.Playlist)
		output, probeErr := probe.Output()
		if probeErr != nil {
			t.Fatal(probeErr)
		}
		firstLine := strings.TrimSpace(strings.Split(string(output), "\n")[0])
		firstPTS, parseErr := strconv.ParseFloat(firstLine, 64)
		if parseErr != nil || firstPTS < window.Start-2.5 || firstPTS >= window.Start+1 {
			t.Fatalf("%s first packet PTS=%q (%v), want global time near %.3f with SourceBuffer offset 0", stream, firstLine, parseErr, window.Start)
		}
	}
}

func TestFMP4AACCommandOnlyTranscodesAudio(t *testing.T) {
	session := &videoFMP4Session{Dir: t.TempDir(), VideoCodec: "hevc", AudioCodec: "eac3", AudioMode: fmp4AudioAAC, AudioTranscoding: true}
	args := strings.Join(videoFMP4Args("source", session, testFMP4Window(session.Dir, 0, videoFMP4WindowSize.Seconds(), "w0000000000000-000001")), " ")
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
	session := &videoFMP4Session{ID: "cached", FileID: "file", AudioMode: fmp4AudioAAC, lastAccess: time.Now(), windows: make(map[string]*videoFMP4Window)}
	server := &Server{videoFMP4Sessions: map[string]*videoFMP4Session{session.ID: session}}
	if got := server.reusableVideoFMP4Session("file", fmp4AudioAAC, "cached"); got != session {
		t.Fatalf("cached session was not reused: %v", got)
	}
	if got := server.reusableVideoFMP4Session("file", fmp4AudioCopy, "cached"); got != nil {
		t.Fatalf("session with a different audio mode was reused: %v", got)
	}
	request := startVideoFMP4Request{AudioMode: fmp4AudioAAC, PreviousSessionID: "cached"}
	if got := server.reusableVideoFMP4SessionForRequest("file", request); got != session {
		t.Fatalf("normal request did not reuse cached session: %v", got)
	}
	request.FreshSession = true
	if got := server.reusableVideoFMP4SessionForRequest("file", request); got != nil {
		t.Fatalf("fresh recovery reused suspect session: %v", got)
	}
}

func TestFMP4FarSeekStartsNearbyFiniteWindow(t *testing.T) {
	if got := fmp4WindowStart(40 * 60); got != 39*60+30 {
		t.Fatalf("40 minute seek starts at %.3f, want 39:30", got)
	}
	if got := fmp4WindowStart(6); got != 0 {
		t.Fatalf("near-zero seek starts at %.3f", got)
	}
}

func TestFMP4WindowsStayShortFiniteAndAdvanceWithoutOverlap(t *testing.T) {
	if videoFMP4WindowSize < 45*time.Second || videoFMP4WindowSize > 60*time.Second {
		t.Fatalf("fMP4 window=%s, want 45-60s", videoFMP4WindowSize)
	}
	first := testFMP4Window(t.TempDir(), 0, videoFMP4WindowSize.Seconds(), "first")
	start := nextFMP4WindowStart(first.Start+first.Duration+.002, map[string]*videoFMP4Window{first.Key: first})
	if start != first.Start+first.Duration {
		t.Fatalf("next sequential window starts at %.3f, want %.3f", start, first.Start+first.Duration)
	}
	rawArgs := videoFMP4Args("source", &videoFMP4Session{Dir: t.TempDir(), VideoCodec: "hevc"}, first)
	toIndex := indexOfArgument(rawArgs, "-to")
	if toIndex >= len(rawArgs)-1 || rawArgs[toIndex+1] != "60.000" {
		t.Fatalf("finite 60-second output bound missing: %v", rawArgs)
	}
}

func TestFMP4FreshRecoveryRequestIsExplicit(t *testing.T) {
	body, err := json.Marshal(startVideoFMP4Request{Start: 207, AudioMode: fmp4AudioAAC, PreviousSessionID: "suspect", FreshSession: true, FallbackReason: "watchdog"})
	if err != nil {
		t.Fatal(err)
	}
	for _, field := range []string{`"previous_session_id":"suspect"`, `"fresh_session":true`, `"fallback_reason":"watchdog"`} {
		if !strings.Contains(string(body), field) {
			t.Fatalf("fresh recovery request %s does not contain %s", body, field)
		}
	}
}

func TestFMP4CachedWindowDoesNotStartWorker(t *testing.T) {
	dir := t.TempDir()
	window := testFMP4Window(dir, 120, videoFMP4WindowSize.Seconds(), "w0000000120000-000001")
	writeFMP4Window(t, dir, window, []byte("cached"))
	session := &videoFMP4Session{ID: "cached", Duration: 3600, Dir: dir, windows: map[string]*videoFMP4Window{window.Key: window}, lastAccess: time.Now()}
	server := &Server{videoFMP4Slots: make(chan struct{}, 1)}
	got, cacheHit, err := server.ensureVideoFMP4Window(session, 121)
	if err != nil || !cacheHit || got != window {
		t.Fatalf("window=%v cacheHit=%v err=%v", got, cacheHit, err)
	}
	if len(server.videoFMP4Slots) != 0 {
		t.Fatal("cache hit acquired a worker slot")
	}
}

func TestFMP4CacheDoesNotUseAFragmentAfterTheRequestedTime(t *testing.T) {
	dir := t.TempDir()
	window := testFMP4Window(dir, 120, videoFMP4WindowSize.Seconds(), "w0000000120000-000001")
	writeFMP4Window(t, dir, window, []byte("future"))
	session := &videoFMP4Session{ID: "cached", Duration: 3600, Dir: dir, windows: map[string]*videoFMP4Window{window.Key: window}}
	fragments, _, _ := videoFMP4FragmentsAt(session, 100, 1)
	if len(fragments) != 0 {
		t.Fatalf("future window was treated as a cache hit: %+v", fragments)
	}
}

func TestFMP4PlayerReleaseStopsWorkerButKeepsCache(t *testing.T) {
	cancelled := make(chan struct{})
	_, cancel := context.WithCancel(context.Background())
	window := &videoFMP4Window{Key: "active", active: true, cancel: func() { cancel(); close(cancelled) }}
	session := &videoFMP4Session{windows: map[string]*videoFMP4Window{window.Key: window}}
	session.stopWorkers()
	select {
	case <-cancelled:
	case <-time.After(time.Second):
		t.Fatal("active worker was not cancelled")
	}
	if session.windows[window.Key] != window {
		t.Fatal("stopping workers evicted the fragment cache")
	}
}

func TestFMP4CachePrunesLeastRecentlyUsedInactiveWindow(t *testing.T) {
	dir := t.TempDir()
	old := testFMP4Window(dir, 0, videoFMP4WindowSize.Seconds(), "w0000000000000-000001")
	recent := testFMP4Window(dir, 120, videoFMP4WindowSize.Seconds(), "w0000000120000-000002")
	old.lastAccess = time.Unix(1, 0)
	recent.lastAccess = time.Unix(2, 0)
	writeFMP4Window(t, dir, old, []byte("old-fragment"))
	writeFMP4Window(t, dir, recent, []byte("recent-fragment"))
	session := &videoFMP4Session{ID: "session", FileID: "file", Dir: dir, windows: map[string]*videoFMP4Window{old.Key: old, recent.Key: recent}}
	server := &Server{log: slog.Default(), videoFMP4Sessions: map[string]*videoFMP4Session{session.ID: session}}
	recentSize := videoFMP4WindowCacheSize(dir, recent)
	server.pruneVideoFMP4CacheLimit(recentSize, session.ID, recent.Key)
	if _, ok := session.windows[old.Key]; ok {
		t.Fatal("least recently used window was not evicted")
	}
	if session.windows[recent.Key] != recent {
		t.Fatal("requested window was evicted")
	}
	if matches, _ := filepath.Glob(filepath.Join(dir, old.FragmentGlob)); len(matches) != 0 {
		t.Fatalf("evicted fragments remain: %v", matches)
	}
}

func testFMP4Window(dir string, start, duration float64, key string) *videoFMP4Window {
	return &videoFMP4Window{
		Key: key, Start: start, Duration: duration, Playlist: filepath.Join(dir, "index-"+key+".m3u8"),
		InitAsset: "init-" + key + ".mp4", FragmentGlob: "fragment-" + key + "-*.m4s",
	}
}

func writeFMP4Window(t *testing.T, dir string, window *videoFMP4Window, data []byte) {
	t.Helper()
	fragment := "fragment-" + window.Key + "-000000.m4s"
	playlist := "#EXTM3U\n#EXT-X-MAP:URI=\"" + window.InitAsset + "\"\n#EXTINF:2.000000,\n" + fragment + "\n"
	for path, body := range map[string][]byte{
		window.Playlist: []byte(playlist), filepath.Join(dir, window.InitAsset): []byte("init"), filepath.Join(dir, fragment): data,
	} {
		if err := os.WriteFile(path, body, 0o600); err != nil {
			t.Fatal(err)
		}
	}
}

func indexOfArgument(args []string, target string) int {
	for index, value := range args {
		if value == target {
			return index
		}
	}
	return len(args)
}

func TestFMP4WindowArgumentsAreStable(t *testing.T) {
	session := &videoFMP4Session{Dir: "/tmp/cache", VideoCodec: "hevc"}
	window := testFMP4Window(session.Dir, 30, videoFMP4WindowSize.Seconds(), "w0000000030000-000001")
	first := videoFMP4Args("source", session, window)
	second := videoFMP4Args("source", session, window)
	if !reflect.DeepEqual(first, second) {
		t.Fatalf("same window produced different arguments\n%v\n%v", first, second)
	}
}
