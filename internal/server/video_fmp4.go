package server

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
	"github.com/go-chi/chi/v5"
)

const (
	videoFMP4IdleTTL     = 20 * time.Minute
	videoFMP4StartWait   = 45 * time.Second
	videoFMP4IndexWait   = 20 * time.Second
	maxVideoFMP4Sessions = 4
	videoFMP4WindowSize  = 60 * time.Second
	videoFMP4WindowStep  = 30 * time.Second
	videoFMP4WindowLead  = 10 * time.Second
	videoFMP4CacheBytes  = int64(1 << 30)
	fmp4AudioCopy        = "copy"
	fmp4AudioAAC         = "aac"
)

var videoFMP4AssetName = regexp.MustCompile(`^(init-w[0-9]+-[0-9]+\.mp4|fragment-w[0-9]+-[0-9]+-[0-9]{6}\.m4s)$`)

type videoFMP4Window struct {
	Key          string
	Start        float64
	Duration     float64
	Playlist     string
	InitAsset    string
	FragmentGlob string
	active       bool
	complete     bool
	err          string
	cancel       context.CancelFunc
	lastAccess   time.Time
	size         int64
}

type videoFMP4Session struct {
	ID               string
	FileID           string
	Duration         float64
	Dir              string
	MIMEType         string
	VideoContentType string
	AudioContentType string
	VideoCodec       string
	AudioCodec       string
	OutputAudioCodec string
	AudioMode        string
	AudioTranscoding bool
	Width            int
	Height           int
	Bitrate          int64
	FrameRate        float64
	File             File

	mu          sync.RWMutex
	lastAccess  time.Time
	ctx         context.Context
	cancel      context.CancelFunc
	windows     map[string]*videoFMP4Window
	nextWindow  uint64
	workers     sync.WaitGroup
	destroyed   bool
	destroyOnce sync.Once
}

func (session *videoFMP4Session) touch() {
	session.mu.Lock()
	session.lastAccess = time.Now()
	session.mu.Unlock()
}

func (session *videoFMP4Session) snapshot() time.Time {
	session.mu.RLock()
	defer session.mu.RUnlock()
	return session.lastAccess
}

func (session *videoFMP4Session) stopWorkers() {
	session.mu.Lock()
	var cancels []context.CancelFunc
	for _, window := range session.windows {
		if window.active && window.cancel != nil {
			cancels = append(cancels, window.cancel)
		}
	}
	session.mu.Unlock()
	for _, cancel := range cancels {
		cancel()
	}
}

func (session *videoFMP4Session) destroy() {
	session.destroyOnce.Do(func() {
		session.mu.Lock()
		session.destroyed = true
		session.mu.Unlock()
		if session.cancel != nil {
			session.cancel()
		}
		done := make(chan struct{})
		go func() { session.workers.Wait(); close(done) }()
		select {
		case <-done:
		case <-time.After(3 * time.Second):
		}
		_ = os.RemoveAll(session.Dir)
	})
}

type startVideoFMP4Request struct {
	Start             float64 `json:"start"`
	AudioMode         string  `json:"audio_mode"`
	PreviousSessionID string  `json:"previous_session_id"`
	FallbackReason    string  `json:"fallback_reason"`
}

type videoFMP4MetadataResponse struct {
	Duration         float64 `json:"duration"`
	MIMEType         string  `json:"mime_type"`
	AACMIMEType      string  `json:"aac_mime_type"`
	VideoMIMEType    string  `json:"video_mime_type"`
	AudioMIMEType    string  `json:"audio_mime_type,omitempty"`
	AACAudioMIMEType string  `json:"aac_audio_mime_type,omitempty"`
	VideoCodec       string  `json:"video_codec"`
	AudioCodec       string  `json:"audio_codec,omitempty"`
	Width            int     `json:"width"`
	Height           int     `json:"height"`
	Bitrate          int64   `json:"bitrate"`
	FrameRate        float64 `json:"frame_rate"`
}

type startVideoFMP4Response struct {
	videoFMP4MetadataResponse
	SessionID        string  `json:"session_id"`
	InitURL          string  `json:"init_url"`
	IndexURL         string  `json:"index_url"`
	Start            float64 `json:"start"`
	RequestedStart   float64 `json:"requested_start"`
	OutputAudioCodec string  `json:"output_audio_codec,omitempty"`
	AudioTranscoding bool    `json:"audio_transcoding"`
	SelectedMode     string  `json:"selected_mode"`
}

type fmp4Fragment struct {
	Number   int     `json:"number"`
	Start    float64 `json:"start"`
	Duration float64 `json:"duration"`
	URL      string  `json:"url"`
	InitURL  string  `json:"init_url"`
}

type fmp4IndexResponse struct {
	Fragments      []fmp4Fragment `json:"fragments"`
	AvailableUntil float64        `json:"available_until"`
	Done           bool           `json:"done"`
	Error          string         `json:"error,omitempty"`
}

type fmp4Probe struct {
	Streams []struct {
		CodecType string `json:"codec_type"`
		CodecName string `json:"codec_name"`
		Profile   string `json:"profile"`
		Level     int    `json:"level"`
		Width     int    `json:"width"`
		Height    int    `json:"height"`
		FrameRate string `json:"avg_frame_rate"`
		Bitrate   string `json:"bit_rate"`
	} `json:"streams"`
	Format struct {
		Duration string `json:"duration"`
		Bitrate  string `json:"bit_rate"`
	} `json:"format"`
}

type fmp4MediaInfo struct {
	duration, frameRate                float64
	videoCodec, audioCodec             string
	videoCodecString, audioCodecString string
	mimeType, aacMIMEType              string
	videoContentType, audioContentType string
	aacAudioContentType                string
	width, height                      int
	bitrate                            int64
}

func (s *Server) videoFMP4Metadata(w http.ResponseWriter, r *http.Request) {
	f, ok := s.fmp4File(w, r)
	if !ok {
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 60*time.Second)
	defer cancel()
	sourceURL, closeSource, err := s.startMediaHLSSource(ctx, f)
	if err != nil {
		problem(w, http.StatusBadGateway, "could not open video for fMP4 probe")
		return
	}
	defer closeSource()
	info, err := probeFMP4Source(ctx, s.cfg.FFmpegPath, sourceURL)
	if err != nil {
		s.log.Warn("fMP4 probe failed", "file", f.ID, "error", err)
		problem(w, http.StatusUnprocessableEntity, "video codec cannot be remuxed to browser fMP4")
		return
	}
	writeJSON(w, http.StatusOK, fmp4MetadataResponse(info))
}

func (s *Server) startVideoFMP4(w http.ResponseWriter, r *http.Request) {
	f, ok := s.fmp4File(w, r)
	if !ok {
		return
	}
	var in startVideoFMP4Request
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	if in.Start < 0 || in.Start > 7*24*60*60 {
		problem(w, http.StatusBadRequest, "invalid fMP4 start time")
		return
	}
	if in.AudioMode == "" {
		in.AudioMode = fmp4AudioCopy
	}
	if in.AudioMode != fmp4AudioCopy && in.AudioMode != fmp4AudioAAC {
		problem(w, http.StatusBadRequest, "invalid fMP4 audio mode")
		return
	}
	if session := s.reusableVideoFMP4Session(f.ID, in.AudioMode, in.PreviousSessionID); session != nil {
		if in.Start >= session.Duration {
			problem(w, http.StatusBadRequest, "fMP4 start time is beyond the video duration")
			return
		}
		window, cacheHit, err := s.ensureVideoFMP4Window(session, in.Start)
		if err != nil {
			problem(w, http.StatusServiceUnavailable, err.Error())
			return
		}
		if err := waitForVideoFMP4Window(r.Context(), session, window, in.Start); err != nil {
			problem(w, http.StatusBadGateway, "fMP4 window failed to start")
			return
		}
		s.logVideoFMP4Selection(f.ID, session, in.FallbackReason, true)
		s.log.Info("fMP4 window selected", "file", f.ID, "session", session.ID, "target", in.Start, "window_start", window.Start, "cache_hit", cacheHit)
		writeJSON(w, http.StatusCreated, videoFMP4Response(session, window, in.Start))
		return
	}
	dir, err := os.MkdirTemp("", "revaro-video-fmp4-")
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not create fMP4 fragment cache")
		return
	}
	probeCtx, probeCancel := context.WithTimeout(r.Context(), 60*time.Second)
	defer probeCancel()
	sourceURL, closeSource, err := s.startMediaHLSSource(probeCtx, f)
	if err != nil {
		_ = os.RemoveAll(dir)
		problem(w, http.StatusBadGateway, "could not open video for fMP4 remux")
		return
	}
	info, err := probeFMP4Source(probeCtx, s.cfg.FFmpegPath, sourceURL)
	closeSource()
	if err != nil {
		_ = os.RemoveAll(dir)
		s.log.Warn("fMP4 creation failed", "file", f.ID, "error", err)
		problem(w, http.StatusUnprocessableEntity, "video codec cannot be remuxed to browser fMP4")
		return
	}
	if in.Start >= info.duration {
		_ = os.RemoveAll(dir)
		problem(w, http.StatusBadRequest, "fMP4 start time is beyond the video duration")
		return
	}
	if in.AudioMode == fmp4AudioCopy && info.audioCodec != "" && info.audioCodecString == "" {
		_ = os.RemoveAll(dir)
		problem(w, http.StatusUnprocessableEntity, "source audio cannot be copied into browser fMP4")
		return
	}
	ctx, cancel := context.WithCancel(s.audioHLSCtx)
	mimeType, audioContentType, outputAudioCodec := fmp4OutputTypes(info, in.AudioMode)
	session := &videoFMP4Session{
		ID: ids.New(), FileID: f.ID, Duration: info.duration, Dir: dir,
		MIMEType: mimeType, VideoContentType: info.videoContentType, AudioContentType: audioContentType,
		VideoCodec: info.videoCodec, AudioCodec: info.audioCodec, OutputAudioCodec: outputAudioCodec,
		AudioMode: in.AudioMode, AudioTranscoding: in.AudioMode == fmp4AudioAAC && info.audioCodec != "",
		Width: info.width, Height: info.height, Bitrate: info.bitrate, FrameRate: info.frameRate,
		File: f, lastAccess: time.Now(), ctx: ctx, cancel: cancel, windows: make(map[string]*videoFMP4Window),
	}
	s.videoFMP4Mu.Lock()
	s.videoFMP4Sessions[session.ID] = session
	s.videoFMP4Mu.Unlock()
	s.pruneVideoFMP4Sessions(session.ID)
	window, _, err := s.ensureVideoFMP4Window(session, in.Start)
	if err != nil {
		s.removeVideoFMP4Session(session.ID)
		problem(w, http.StatusTooManyRequests, err.Error())
		return
	}
	if err := waitForVideoFMP4Window(r.Context(), session, window, in.Start); err != nil {
		s.removeVideoFMP4Session(session.ID)
		if errors.Is(err, context.DeadlineExceeded) {
			problem(w, http.StatusGatewayTimeout, "fMP4 window took too long to start")
		} else {
			s.log.Warn("fMP4 window failed", "file", f.ID, "session", session.ID, "error", err)
			problem(w, http.StatusBadGateway, "fMP4 window failed to start")
		}
		return
	}
	s.logVideoFMP4Selection(f.ID, session, in.FallbackReason, false)
	writeJSON(w, http.StatusCreated, videoFMP4Response(session, window, in.Start))
}

func (s *Server) fmp4File(w http.ResponseWriter, r *http.Request) (File, bool) {
	f, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || f.Kind != "file" || f.Status != "ready" || !isVideoSource(f) {
		problem(w, http.StatusNotFound, "ready video file not found")
		return File{}, false
	}
	if _, err := exec.LookPath(s.cfg.FFmpegPath); err != nil {
		problem(w, http.StatusServiceUnavailable, "ffmpeg is unavailable")
		return File{}, false
	}
	if _, err := ffprobeFor(s.cfg.FFmpegPath); err != nil {
		problem(w, http.StatusServiceUnavailable, "ffprobe is unavailable")
		return File{}, false
	}
	return f, true
}

func (s *Server) logVideoFMP4Selection(fileID string, session *videoFMP4Session, fallbackReason string, reused bool) {
	selectedMode := "mse-copy"
	if session.AudioTranscoding {
		selectedMode = "mse-copy-video-aac-audio"
	}
	s.log.Info("video playback selected", "file", fileID, "video_codec", session.VideoCodec,
		"audio_codec", session.AudioCodec, "selected_mode", selectedMode, "video_transcoding", false,
		"audio_transcoding", session.AudioTranscoding, "fallback_reason", strings.TrimSpace(fallbackReason),
		"session", session.ID, "session_reused", reused)
}

func fmp4MetadataResponse(info fmp4MediaInfo) videoFMP4MetadataResponse {
	return videoFMP4MetadataResponse{
		Duration: info.duration, MIMEType: info.mimeType, AACMIMEType: info.aacMIMEType,
		VideoMIMEType: info.videoContentType, AudioMIMEType: info.audioContentType,
		AACAudioMIMEType: info.aacAudioContentType, VideoCodec: info.videoCodec, AudioCodec: info.audioCodec,
		Width: info.width, Height: info.height, Bitrate: info.bitrate, FrameRate: info.frameRate,
	}
}

func videoFMP4Response(session *videoFMP4Session, window *videoFMP4Window, requestedStart float64) startVideoFMP4Response {
	selectedMode := "mse-copy"
	if session.AudioTranscoding {
		selectedMode = "mse-copy-video-aac-audio"
	}
	metadata := videoFMP4MetadataResponse{
		Duration: session.Duration, MIMEType: session.MIMEType, AACMIMEType: session.MIMEType,
		VideoMIMEType: session.VideoContentType, AudioMIMEType: session.AudioContentType,
		AACAudioMIMEType: `audio/mp4; codecs="mp4a.40.2"`, VideoCodec: session.VideoCodec,
		AudioCodec: session.AudioCodec, Width: session.Width, Height: session.Height,
		Bitrate: session.Bitrate, FrameRate: session.FrameRate,
	}
	base := "/api/video/fmp4/" + session.ID
	return startVideoFMP4Response{
		videoFMP4MetadataResponse: metadata, SessionID: session.ID, InitURL: base + "/" + window.InitAsset,
		IndexURL: base + "/index.json", Start: 0, RequestedStart: requestedStart,
		OutputAudioCodec: session.OutputAudioCodec, AudioTranscoding: session.AudioTranscoding, SelectedMode: selectedMode,
	}
}

func probeFMP4Source(ctx context.Context, ffmpeg, sourceURL string) (fmp4MediaInfo, error) {
	ffprobe, err := ffprobeFor(ffmpeg)
	if err != nil {
		return fmp4MediaInfo{}, err
	}
	cmd := exec.CommandContext(ctx, ffprobe, "-v", "error", "-show_entries",
		"format=duration,bit_rate:stream=codec_type,codec_name,profile,level,width,height,avg_frame_rate,bit_rate",
		"-of", "json", sourceURL)
	output := &limitedBuffer{limit: 2 << 20}
	stderr := &limitedBuffer{limit: 64 << 10}
	cmd.Stdout, cmd.Stderr = output, stderr
	if err := cmd.Run(); err != nil {
		return fmp4MediaInfo{}, mediaCommandError("ffprobe fMP4 metadata", err, ctx.Err(), stderr.String())
	}
	var probe fmp4Probe
	if err := json.Unmarshal([]byte(output.String()), &probe); err != nil {
		return fmp4MediaInfo{}, fmt.Errorf("decode ffprobe fMP4 metadata: %w", err)
	}
	var info fmp4MediaInfo
	info.duration, _ = strconv.ParseFloat(probe.Format.Duration, 64)
	info.bitrate, _ = strconv.ParseInt(probe.Format.Bitrate, 10, 64)
	var videoProfile string
	var videoLevel int
	for _, stream := range probe.Streams {
		switch stream.CodecType {
		case "video":
			if info.videoCodec == "" {
				info.videoCodec, videoProfile, videoLevel = strings.ToLower(stream.CodecName), stream.Profile, stream.Level
				info.width, info.height = stream.Width, stream.Height
				info.frameRate = parseFMP4FrameRate(stream.FrameRate)
				if bitrate, parseErr := strconv.ParseInt(stream.Bitrate, 10, 64); parseErr == nil && bitrate > 0 {
					info.bitrate = bitrate
				}
			}
		case "audio":
			if info.audioCodec == "" {
				info.audioCodec = strings.ToLower(stream.CodecName)
			}
		}
	}
	if info.duration <= 0 || info.videoCodec == "" {
		return fmp4MediaInfo{}, errors.New("video duration or codec is unavailable")
	}
	info.videoCodecString, err = fmp4VideoCodecString(info.videoCodec, videoProfile, videoLevel)
	if err != nil {
		return fmp4MediaInfo{}, err
	}
	info.audioCodecString, _ = fmp4AudioCodecString(info.audioCodec)
	info.videoContentType = `video/mp4; codecs="` + info.videoCodecString + `"`
	info.aacAudioContentType = `audio/mp4; codecs="mp4a.40.2"`
	info.mimeType = info.videoContentType
	if info.audioCodecString != "" {
		info.audioContentType = `audio/mp4; codecs="` + info.audioCodecString + `"`
		info.mimeType = `video/mp4; codecs="` + info.videoCodecString + `, ` + info.audioCodecString + `"`
	}
	info.aacMIMEType = `video/mp4; codecs="` + info.videoCodecString + `, mp4a.40.2"`
	if info.audioCodec == "" {
		info.aacMIMEType = info.mimeType
	}
	if info.bitrate <= 0 {
		info.bitrate = 8_000_000
	}
	if info.frameRate <= 0 {
		info.frameRate = 30
	}
	return info, nil
}

func fmp4OutputTypes(info fmp4MediaInfo, audioMode string) (string, string, string) {
	if info.audioCodec == "" {
		return info.videoContentType, "", ""
	}
	if audioMode == fmp4AudioAAC {
		return info.aacMIMEType, info.aacAudioContentType, "aac"
	}
	return info.mimeType, info.audioContentType, info.audioCodec
}

func parseFMP4FrameRate(value string) float64 {
	parts := strings.Split(value, "/")
	if len(parts) == 2 {
		numerator, numeratorErr := strconv.ParseFloat(parts[0], 64)
		denominator, denominatorErr := strconv.ParseFloat(parts[1], 64)
		if numeratorErr == nil && denominatorErr == nil && denominator > 0 {
			return numerator / denominator
		}
	}
	result, _ := strconv.ParseFloat(value, 64)
	return result
}

func fmp4VideoCodecString(codec, profile string, level int) (string, error) {
	switch strings.ToLower(codec) {
	case "hevc", "h265":
		if level <= 0 {
			level = 120
		}
		if strings.Contains(strings.ToLower(profile), "10") {
			return fmt.Sprintf("hvc1.2.4.L%d.B0", level), nil
		}
		return fmt.Sprintf("hvc1.1.6.L%d.B0", level), nil
	case "h264", "avc":
		if level <= 0 {
			level = 40
		}
		prefix := "6400"
		lowerProfile := strings.ToLower(profile)
		if strings.Contains(lowerProfile, "baseline") {
			prefix = "42E0"
		} else if strings.Contains(lowerProfile, "main") {
			prefix = "4D40"
		}
		return fmt.Sprintf("avc1.%s%02X", prefix, level), nil
	default:
		return "", fmt.Errorf("video codec %q is not supported by the fMP4 remux path", codec)
	}
}

func fmp4AudioCodecString(codec string) (string, error) {
	switch strings.ToLower(codec) {
	case "":
		return "", nil
	case "aac":
		return "mp4a.40.2", nil
	case "eac3":
		return "ec-3", nil
	case "ac3":
		return "ac-3", nil
	case "mp3":
		return "mp4a.6B", nil
	default:
		return "", fmt.Errorf("audio codec %q cannot be copied into browser fMP4", codec)
	}
}

func videoFMP4Args(sourceURL string, session *videoFMP4Session, window *videoFMP4Window) []string {
	args := []string{"-hide_banner", "-loglevel", "error", "-y", "-copyts"}
	if window.Start > 0 {
		args = append(args, "-ss", strconv.FormatFloat(window.Start, 'f', 3, 64))
	}
	args = append(args, "-i", sourceURL,
		"-map", "0:v:0", "-map", "0:a:0?", "-sn", "-dn", "-c:v", "copy")
	if session.VideoCodec == "hevc" || session.VideoCodec == "h265" {
		args = append(args, "-tag:v", "hvc1")
	} else if session.VideoCodec == "h264" || session.VideoCodec == "avc" {
		args = append(args, "-tag:v", "avc1")
	}
	if session.AudioTranscoding {
		args = append(args, "-c:a", "aac", "-b:a", "192k", "-ar", "48000")
	} else {
		args = append(args, "-c:a", "copy")
	}
	args = append(args, "-to", strconv.FormatFloat(window.Start+window.Duration, 'f', 3, 64))
	return append(args,
		"-max_muxing_queue_size", "2048",
		"-f", "hls", "-hls_segment_type", "fmp4", "-hls_time", "2", "-hls_list_size", "0",
		"-hls_playlist_type", "event", "-hls_flags", "temp_file+independent_segments",
		"-hls_fmp4_init_filename", window.InitAsset,
		"-hls_segment_filename", filepath.Join(session.Dir, "fragment-"+window.Key+"-%06d.m4s"), window.Playlist,
	)
}

func (s *Server) runVideoFMP4Window(ctx context.Context, session *videoFMP4Session, window *videoFMP4Window) {
	defer session.workers.Done()
	defer func() { <-s.videoFMP4Slots }()
	sourceURL, closeSource, err := s.startMediaHLSSource(ctx, session.File)
	if err != nil {
		s.finishVideoFMP4Window(session, window, err)
		return
	}
	defer closeSource()
	cmd := exec.CommandContext(ctx, s.cfg.FFmpegPath, videoFMP4Args(sourceURL, session, window)...)
	stderr := &limitedBuffer{limit: 64 << 10}
	cmd.Stderr = stderr
	err = cmd.Run()
	if err != nil {
		if ctx.Err() != nil {
			err = ctx.Err()
		} else {
			err = mediaCommandError("ffmpeg fMP4 fragments", err, nil, stderr.String())
		}
	}
	if err != nil && !errors.Is(err, context.Canceled) {
		s.log.Warn("fMP4 window remux stopped", "file", session.FileID, "session", session.ID, "window", window.Key, "error", err)
	}
	s.finishVideoFMP4Window(session, window, err)
}

func (s *Server) finishVideoFMP4Window(session *videoFMP4Session, window *videoFMP4Window, err error) {
	size := videoFMP4WindowCacheSize(session.Dir, window)
	session.mu.Lock()
	cancel := window.cancel
	window.active = false
	window.cancel = nil
	window.size = size
	window.complete = err == nil
	if err != nil && !errors.Is(err, context.Canceled) {
		window.err = err.Error()
	}
	session.mu.Unlock()
	if cancel != nil {
		cancel()
	}
	s.log.Info("fMP4 window stopped", "file", session.FileID, "session", session.ID, "window", window.Key,
		"start", window.Start, "duration", window.Duration, "cached_bytes", size, "complete", err == nil)
	s.pruneVideoFMP4Cache(session.ID, window.Key)
}

func fmp4WindowStart(target float64) float64 {
	lead := videoFMP4WindowLead.Seconds()
	step := videoFMP4WindowStep.Seconds()
	return max(0, float64(int64(max(0, target-lead)/step))*step)
}

func nextFMP4WindowStart(target float64, windows map[string]*videoFMP4Window) float64 {
	start := fmp4WindowStart(target)
	for _, window := range windows {
		end := window.Start + window.Duration
		// Sequential playback asks just past the final fragment. Continue from
		// the preceding finite window instead of applying seek lead again and
		// rereading up to half of it from S3.
		if target >= end-.01 && target <= end+videoFMP4WindowLead.Seconds() && end > start {
			start = end
		}
	}
	return start
}

func (s *Server) ensureVideoFMP4Window(session *videoFMP4Session, target float64) (*videoFMP4Window, bool, error) {
	if fragments, _, window := videoFMP4FragmentsAt(session, target, 1); len(fragments) > 0 {
		return window, true, nil
	}
	session.mu.Lock()
	if session.destroyed {
		session.mu.Unlock()
		return nil, false, errors.New("fMP4 session is closed")
	}
	for _, window := range session.windows {
		if target+.001 < window.Start || target >= window.Start+window.Duration-.001 {
			continue
		}
		if window.active {
			window.lastAccess = time.Now()
			session.mu.Unlock()
			return window, false, nil
		}
		if window.complete && window.err != "" {
			err := errors.New(window.err)
			session.mu.Unlock()
			return nil, false, err
		}
	}
	select {
	case s.videoFMP4Slots <- struct{}{}:
	default:
		session.mu.Unlock()
		return nil, false, errors.New("too many fMP4 window workers are active")
	}
	start := nextFMP4WindowStart(target, session.windows)
	duration := min(videoFMP4WindowSize.Seconds(), session.Duration-start)
	if duration <= 0 {
		<-s.videoFMP4Slots
		session.mu.Unlock()
		return nil, false, errors.New("fMP4 target is beyond video duration")
	}
	session.nextWindow++
	key := fmt.Sprintf("w%013d-%06d", int64(start*1000), session.nextWindow)
	workerCtx, cancel := context.WithCancel(session.ctx)
	window := &videoFMP4Window{
		Key: key, Start: start, Duration: duration, Playlist: filepath.Join(session.Dir, "index-"+key+".m3u8"),
		InitAsset: "init-" + key + ".mp4", FragmentGlob: "fragment-" + key + "-*.m4s",
		active: true, cancel: cancel, lastAccess: time.Now(),
	}
	session.windows[key] = window
	session.workers.Add(1)
	session.mu.Unlock()
	s.log.Info("fMP4 window started", "file", session.FileID, "session", session.ID, "window", key,
		"target", target, "input_seek", start, "duration", duration, "video_transcoding", false, "audio_transcoding", session.AudioTranscoding)
	go s.runVideoFMP4Window(workerCtx, session, window)
	return window, false, nil
}

func waitForVideoFMP4Window(requestCtx context.Context, session *videoFMP4Session, window *videoFMP4Window, target float64) error {
	timer := time.NewTimer(videoFMP4StartWait)
	defer timer.Stop()
	ticker := time.NewTicker(75 * time.Millisecond)
	defer ticker.Stop()
	for {
		fragments, _, _ := videoFMP4FragmentsAt(session, target, 1)
		if len(fragments) > 0 {
			return nil
		}
		session.mu.RLock()
		active, complete, windowErr := window.active, window.complete, window.err
		session.mu.RUnlock()
		if !active {
			if windowErr != "" {
				return errors.New(windowErr)
			}
			if complete {
				return errors.New("ffmpeg produced no fMP4 fragment for the requested window")
			}
			return context.Canceled
		}
		select {
		case <-requestCtx.Done():
			return requestCtx.Err()
		case <-timer.C:
			return context.DeadlineExceeded
		case <-ticker.C:
		}
	}
}

func fmp4FragmentIndex(path, sessionID string, windowStart float64, initAsset string) ([]fmp4Fragment, float64) {
	body, err := os.ReadFile(path)
	if err != nil {
		return nil, 0
	}
	var fragments []fmp4Fragment
	start, pendingDuration := windowStart, float64(0)
	for _, rawLine := range strings.Split(string(body), "\n") {
		line := strings.TrimSpace(rawLine)
		if strings.HasPrefix(line, "#EXTINF:") {
			value := strings.TrimSuffix(strings.TrimPrefix(line, "#EXTINF:"), ",")
			pendingDuration, _ = strconv.ParseFloat(value, 64)
			continue
		}
		if pendingDuration <= 0 || !videoFMP4AssetName.MatchString(line) || !strings.HasSuffix(line, ".m4s") {
			continue
		}
		numberText := strings.TrimSuffix(line, ".m4s")
		if dash := strings.LastIndex(numberText, "-"); dash >= 0 {
			numberText = numberText[dash+1:]
		}
		number, parseErr := strconv.Atoi(numberText)
		if parseErr != nil {
			continue
		}
		fragments = append(fragments, fmp4Fragment{Number: number, Start: start, Duration: pendingDuration,
			URL:     "/api/video/fmp4/" + sessionID + "/" + line,
			InitURL: "/api/video/fmp4/" + sessionID + "/" + initAsset})
		start += pendingDuration
		pendingDuration = 0
	}
	return fragments, start
}

func videoFMP4FragmentsAt(session *videoFMP4Session, target float64, limit int) ([]fmp4Fragment, float64, *videoFMP4Window) {
	session.mu.RLock()
	windows := make([]*videoFMP4Window, 0, len(session.windows))
	for _, window := range session.windows {
		windows = append(windows, window)
	}
	session.mu.RUnlock()
	sort.Slice(windows, func(i, j int) bool { return windows[i].Start > windows[j].Start })
	var chosen *videoFMP4Window
	var chosenFragments []fmp4Fragment
	var available float64
	for _, window := range windows {
		if _, err := os.Stat(filepath.Join(session.Dir, window.InitAsset)); err != nil {
			continue
		}
		fragments, windowAvailable := fmp4FragmentIndex(window.Playlist, session.ID, window.Start, window.InitAsset)
		if len(fragments) == 0 || target+.001 < fragments[0].Start {
			continue
		}
		startIndex := 0
		for startIndex < len(fragments) && fragments[startIndex].Start+fragments[startIndex].Duration <= target+.001 {
			startIndex++
		}
		if startIndex >= len(fragments) {
			continue
		}
		chosen, available = window, windowAvailable
		end := min(len(fragments), startIndex+limit)
		chosenFragments = fragments[startIndex:end]
		break
	}
	if chosen != nil {
		now := time.Now()
		session.mu.Lock()
		chosen.lastAccess = now
		session.lastAccess = now
		session.mu.Unlock()
	}
	return chosenFragments, available, chosen
}

func (s *Server) videoFMP4Index(w http.ResponseWriter, r *http.Request) {
	session := s.videoFMP4Session(chi.URLParam(r, "session"))
	if session == nil {
		problem(w, http.StatusNotFound, "fMP4 fragment session not found")
		return
	}
	target, _ := strconv.ParseFloat(r.URL.Query().Get("time"), 64)
	target = max(0, min(target, session.Duration))
	if target >= session.Duration-.001 {
		w.Header().Set("Cache-Control", "no-store")
		writeJSON(w, http.StatusOK, fmp4IndexResponse{Fragments: []fmp4Fragment{}, AvailableUntil: session.Duration, Done: true})
		return
	}
	deadline := time.NewTimer(videoFMP4IndexWait)
	defer deadline.Stop()
	ticker := time.NewTicker(100 * time.Millisecond)
	defer ticker.Stop()
	for {
		fragments, available, _ := videoFMP4FragmentsAt(session, target, 12)
		if len(fragments) > 0 {
			session.touch()
			w.Header().Set("Cache-Control", "no-store")
			writeJSON(w, http.StatusOK, fmp4IndexResponse{Fragments: fragments, AvailableUntil: available, Done: false})
			return
		}
		window, _, ensureErr := s.ensureVideoFMP4Window(session, target)
		if ensureErr != nil {
			w.Header().Set("Cache-Control", "no-store")
			writeJSON(w, http.StatusOK, fmp4IndexResponse{Fragments: []fmp4Fragment{}, AvailableUntil: available, Error: ensureErr.Error()})
			return
		}
		session.mu.RLock()
		active, complete, windowErr := window.active, window.complete, window.err
		session.mu.RUnlock()
		if !active && (complete || windowErr != "") {
			if windowErr == "" {
				windowErr = "fMP4 window ended before the requested timestamp"
			}
			w.Header().Set("Cache-Control", "no-store")
			writeJSON(w, http.StatusOK, fmp4IndexResponse{Fragments: []fmp4Fragment{}, AvailableUntil: available, Error: windowErr})
			return
		}
		select {
		case <-r.Context().Done():
			return
		case <-deadline.C:
			w.Header().Set("Cache-Control", "no-store")
			writeJSON(w, http.StatusOK, fmp4IndexResponse{Fragments: []fmp4Fragment{}, AvailableUntil: available})
			return
		case <-ticker.C:
		}
	}
}

func (s *Server) videoFMP4Asset(w http.ResponseWriter, r *http.Request) {
	session := s.videoFMP4Session(chi.URLParam(r, "session"))
	if session == nil {
		problem(w, http.StatusNotFound, "fMP4 fragment session not found")
		return
	}
	asset := chi.URLParam(r, "asset")
	if !videoFMP4AssetName.MatchString(asset) {
		problem(w, http.StatusNotFound, "fMP4 fragment not found")
		return
	}
	path := filepath.Join(session.Dir, asset)
	if _, err := os.Stat(path); err != nil {
		problem(w, http.StatusNotFound, "fMP4 fragment is not ready")
		return
	}
	now := time.Now()
	session.mu.Lock()
	session.lastAccess = now
	for _, window := range session.windows {
		if asset == window.InitAsset || strings.HasPrefix(asset, "fragment-"+window.Key+"-") {
			window.lastAccess = now
			break
		}
	}
	session.mu.Unlock()
	w.Header().Set("Content-Type", "video/mp4")
	w.Header().Set("Cache-Control", "private, max-age=3600")
	w.Header().Set("X-Content-Type-Options", "nosniff")
	http.ServeFile(w, r, path)
}

func (s *Server) stopVideoFMP4(w http.ResponseWriter, r *http.Request) {
	session := s.videoFMP4Session(chi.URLParam(r, "session"))
	if session == nil {
		problem(w, http.StatusNotFound, "fMP4 fragment session not found")
		return
	}
	// Keep completed fragments for reuse, but do not keep reading the source after
	// the player has gone away.
	session.stopWorkers()
	session.touch()
	s.log.Info("fMP4 player released", "session", session.ID, "file", session.FileID, "workers_stopped", true)
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) reusableVideoFMP4Session(fileID, audioMode, preferredID string) *videoFMP4Session {
	s.videoFMP4Mu.RLock()
	defer s.videoFMP4Mu.RUnlock()
	if preferred := s.videoFMP4Sessions[preferredID]; reusableFMP4Session(preferred, fileID, audioMode) {
		preferred.touch()
		return preferred
	}
	for _, session := range s.videoFMP4Sessions {
		if reusableFMP4Session(session, fileID, audioMode) {
			session.touch()
			return session
		}
	}
	return nil
}

func reusableFMP4Session(session *videoFMP4Session, fileID, audioMode string) bool {
	if session == nil || session.FileID != fileID || session.AudioMode != audioMode {
		return false
	}
	session.mu.RLock()
	defer session.mu.RUnlock()
	return !session.destroyed
}

func videoFMP4WindowCacheSize(dir string, window *videoFMP4Window) int64 {
	paths := []string{window.Playlist, filepath.Join(dir, window.InitAsset)}
	fragments, _ := filepath.Glob(filepath.Join(dir, window.FragmentGlob))
	paths = append(paths, fragments...)
	var size int64
	for _, path := range paths {
		if info, err := os.Stat(path); err == nil && info.Mode().IsRegular() {
			size += info.Size()
		}
	}
	return size
}

func removeVideoFMP4WindowFiles(dir string, window *videoFMP4Window) {
	_ = os.Remove(window.Playlist)
	_ = os.Remove(filepath.Join(dir, window.InitAsset))
	fragments, _ := filepath.Glob(filepath.Join(dir, window.FragmentGlob))
	for _, path := range fragments {
		_ = os.Remove(path)
	}
}

func (s *Server) pruneVideoFMP4Cache(keepSessionID, keepWindowKey string) {
	s.pruneVideoFMP4CacheLimit(videoFMP4CacheBytes, keepSessionID, keepWindowKey)
}

func (s *Server) pruneVideoFMP4CacheLimit(limit int64, keepSessionID, keepWindowKey string) {
	type cachedWindow struct {
		session    *videoFMP4Session
		window     *videoFMP4Window
		lastAccess time.Time
		size       int64
	}
	s.videoFMP4Mu.RLock()
	sessions := make([]*videoFMP4Session, 0, len(s.videoFMP4Sessions))
	for _, session := range s.videoFMP4Sessions {
		sessions = append(sessions, session)
	}
	s.videoFMP4Mu.RUnlock()

	var total int64
	var candidates []cachedWindow
	for _, session := range sessions {
		session.mu.Lock()
		for _, window := range session.windows {
			size := videoFMP4WindowCacheSize(session.Dir, window)
			window.size = size
			total += size
			if !window.active && !(session.ID == keepSessionID && window.Key == keepWindowKey) {
				candidates = append(candidates, cachedWindow{session: session, window: window, lastAccess: window.lastAccess, size: size})
			}
		}
		session.mu.Unlock()
	}
	if total <= limit {
		return
	}
	sort.Slice(candidates, func(i, j int) bool { return candidates[i].lastAccess.Before(candidates[j].lastAccess) })
	for _, candidate := range candidates {
		if total <= limit {
			break
		}
		session, window := candidate.session, candidate.window
		session.mu.Lock()
		if session.windows[window.Key] != window || window.active || window.lastAccess.After(candidate.lastAccess) {
			session.mu.Unlock()
			continue
		}
		delete(session.windows, window.Key)
		session.mu.Unlock()
		removeVideoFMP4WindowFiles(session.Dir, window)
		total -= candidate.size
		s.log.Info("fMP4 cache evicted", "file", session.FileID, "session", session.ID, "window", window.Key,
			"cached_bytes", candidate.size, "cache_bytes_after", total)
	}
}

func (s *Server) videoFMP4Session(id string) *videoFMP4Session {
	s.videoFMP4Mu.RLock()
	defer s.videoFMP4Mu.RUnlock()
	return s.videoFMP4Sessions[id]
}

func (s *Server) removeVideoFMP4Session(id string) *videoFMP4Session {
	s.videoFMP4Mu.Lock()
	session := s.videoFMP4Sessions[id]
	delete(s.videoFMP4Sessions, id)
	s.videoFMP4Mu.Unlock()
	if session != nil {
		session.destroy()
	}
	return session
}

func (s *Server) pruneVideoFMP4Sessions(keepID string) {
	type cachedSession struct {
		id         string
		lastAccess time.Time
	}
	s.videoFMP4Mu.RLock()
	items := make([]cachedSession, 0, len(s.videoFMP4Sessions))
	for id, session := range s.videoFMP4Sessions {
		lastAccess := session.snapshot()
		items = append(items, cachedSession{id: id, lastAccess: lastAccess})
	}
	s.videoFMP4Mu.RUnlock()
	for len(items) > maxVideoFMP4Sessions {
		oldest := -1
		for index := range items {
			if items[index].id == keepID {
				continue
			}
			if oldest < 0 || items[index].lastAccess.Before(items[oldest].lastAccess) {
				oldest = index
			}
		}
		if oldest < 0 {
			break
		}
		s.removeVideoFMP4Session(items[oldest].id)
		items = append(items[:oldest], items[oldest+1:]...)
	}
}

func (s *Server) cleanupVideoFMP4Sessions() {
	ticker := time.NewTicker(time.Minute)
	defer ticker.Stop()
	for {
		select {
		case <-s.audioHLSCtx.Done():
			return
		case now := <-ticker.C:
			var expired []string
			s.videoFMP4Mu.RLock()
			for id, session := range s.videoFMP4Sessions {
				lastAccess := session.snapshot()
				if now.Sub(lastAccess) > videoFMP4IdleTTL {
					expired = append(expired, id)
				}
			}
			s.videoFMP4Mu.RUnlock()
			for _, id := range expired {
				s.removeVideoFMP4Session(id)
			}
			s.pruneVideoFMP4Cache("", "")
		}
	}
}
