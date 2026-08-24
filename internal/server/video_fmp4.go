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
	fmp4AudioCopy        = "copy"
	fmp4AudioAAC         = "aac"
)

var videoFMP4AssetName = regexp.MustCompile(`^(init\.mp4|fragment-[0-9]{6}\.m4s)$`)

type videoFMP4Session struct {
	ID               string
	FileID           string
	Duration         float64
	Dir              string
	Playlist         string
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

	mu          sync.RWMutex
	lastAccess  time.Time
	done        bool
	err         string
	cancel      context.CancelFunc
	doneCh      chan struct{}
	doneOnce    sync.Once
	destroyOnce sync.Once
}

func (session *videoFMP4Session) touch() {
	session.mu.Lock()
	session.lastAccess = time.Now()
	session.mu.Unlock()
}

func (session *videoFMP4Session) snapshot() (time.Time, bool, string) {
	session.mu.RLock()
	defer session.mu.RUnlock()
	return session.lastAccess, session.done, session.err
}

func (session *videoFMP4Session) finish(err error) {
	session.mu.Lock()
	session.done = true
	if err != nil && !errors.Is(err, context.Canceled) {
		session.err = err.Error()
	}
	session.mu.Unlock()
	session.doneOnce.Do(func() { close(session.doneCh) })
}

func (session *videoFMP4Session) destroy() {
	session.destroyOnce.Do(func() {
		if session.cancel != nil {
			session.cancel()
		}
		select {
		case <-session.doneCh:
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
		s.logVideoFMP4Selection(f.ID, session, in.FallbackReason, true)
		writeJSON(w, http.StatusCreated, videoFMP4Response(session, in.Start))
		return
	}
	select {
	case s.videoFMP4Slots <- struct{}{}:
	default:
		problem(w, http.StatusTooManyRequests, "too many fMP4 streams are active")
		return
	}
	dir, err := os.MkdirTemp("", "revaro-video-fmp4-")
	if err != nil {
		<-s.videoFMP4Slots
		problem(w, http.StatusInternalServerError, "could not create fMP4 fragment cache")
		return
	}
	ctx, cancel := context.WithCancel(s.audioHLSCtx)
	sourceURL, closeSource, err := s.startMediaHLSSource(ctx, f)
	if err != nil {
		cancel()
		<-s.videoFMP4Slots
		_ = os.RemoveAll(dir)
		problem(w, http.StatusBadGateway, "could not open video for fMP4 remux")
		return
	}
	info, err := probeFMP4Source(ctx, s.cfg.FFmpegPath, sourceURL)
	if err != nil {
		closeSource()
		cancel()
		<-s.videoFMP4Slots
		_ = os.RemoveAll(dir)
		s.log.Warn("fMP4 creation failed", "file", f.ID, "error", err)
		problem(w, http.StatusUnprocessableEntity, "video codec cannot be remuxed to browser fMP4")
		return
	}
	if in.Start >= info.duration {
		closeSource()
		cancel()
		<-s.videoFMP4Slots
		_ = os.RemoveAll(dir)
		problem(w, http.StatusBadRequest, "fMP4 start time is beyond the video duration")
		return
	}
	if in.AudioMode == fmp4AudioCopy && info.audioCodec != "" && info.audioCodecString == "" {
		closeSource()
		cancel()
		<-s.videoFMP4Slots
		_ = os.RemoveAll(dir)
		problem(w, http.StatusUnprocessableEntity, "source audio cannot be copied into browser fMP4")
		return
	}
	mimeType, audioContentType, outputAudioCodec := fmp4OutputTypes(info, in.AudioMode)
	session := &videoFMP4Session{
		ID: ids.New(), FileID: f.ID, Duration: info.duration, Dir: dir, Playlist: filepath.Join(dir, "index.m3u8"),
		MIMEType: mimeType, VideoContentType: info.videoContentType, AudioContentType: audioContentType,
		VideoCodec: info.videoCodec, AudioCodec: info.audioCodec, OutputAudioCodec: outputAudioCodec,
		AudioMode: in.AudioMode, AudioTranscoding: in.AudioMode == fmp4AudioAAC && info.audioCodec != "",
		Width: info.width, Height: info.height, Bitrate: info.bitrate, FrameRate: info.frameRate,
		lastAccess: time.Now(), cancel: cancel, doneCh: make(chan struct{}),
	}
	s.videoFMP4Mu.Lock()
	s.videoFMP4Sessions[session.ID] = session
	s.videoFMP4Mu.Unlock()
	s.pruneVideoFMP4Sessions(session.ID)
	go s.runVideoFMP4(ctx, f, sourceURL, closeSource, session)
	if err := waitForVideoFMP4(r.Context(), session); err != nil {
		s.removeVideoFMP4Session(session.ID)
		if errors.Is(err, context.DeadlineExceeded) {
			problem(w, http.StatusGatewayTimeout, "fMP4 fragments took too long to start")
		} else {
			s.log.Warn("fMP4 creation failed", "file", f.ID, "session", session.ID, "error", err)
			problem(w, http.StatusBadGateway, "fMP4 fragments failed to start")
		}
		return
	}
	s.logVideoFMP4Selection(f.ID, session, in.FallbackReason, false)
	writeJSON(w, http.StatusCreated, videoFMP4Response(session, in.Start))
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

func videoFMP4Response(session *videoFMP4Session, requestedStart float64) startVideoFMP4Response {
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
		videoFMP4MetadataResponse: metadata, SessionID: session.ID, InitURL: base + "/init.mp4",
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

func videoFMP4Args(sourceURL string, session *videoFMP4Session) []string {
	args := []string{"-hide_banner", "-loglevel", "error", "-y", "-i", sourceURL,
		"-map", "0:v:0", "-map", "0:a:0?", "-sn", "-dn", "-c:v", "copy"}
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
	return append(args,
		"-avoid_negative_ts", "make_zero", "-max_muxing_queue_size", "2048",
		"-f", "hls", "-hls_segment_type", "fmp4", "-hls_time", "2", "-hls_list_size", "0",
		"-hls_playlist_type", "event", "-hls_flags", "temp_file+independent_segments",
		"-hls_fmp4_init_filename", "init.mp4", "-hls_segment_filename", filepath.Join(session.Dir, "fragment-%06d.m4s"), session.Playlist,
	)
}

func (s *Server) runVideoFMP4(ctx context.Context, f File, sourceURL string, closeSource func(), session *videoFMP4Session) {
	defer func() { <-s.videoFMP4Slots }()
	defer closeSource()
	cmd := exec.CommandContext(ctx, s.cfg.FFmpegPath, videoFMP4Args(sourceURL, session)...)
	stderr := &limitedBuffer{limit: 64 << 10}
	cmd.Stderr = stderr
	err := cmd.Run()
	if err != nil {
		if ctx.Err() != nil {
			err = ctx.Err()
		} else {
			err = mediaCommandError("ffmpeg fMP4 fragments", err, nil, stderr.String())
		}
	}
	if err != nil && !errors.Is(err, context.Canceled) {
		s.log.Warn("fMP4 remux stopped", "file", f.ID, "session", session.ID, "error", err)
	}
	session.finish(err)
}

func waitForVideoFMP4(requestCtx context.Context, session *videoFMP4Session) error {
	timer := time.NewTimer(videoFMP4StartWait)
	defer timer.Stop()
	ticker := time.NewTicker(75 * time.Millisecond)
	defer ticker.Stop()
	for {
		fragments, _ := fmp4FragmentIndex(session.Playlist, session.ID)
		if len(fragments) > 0 {
			if _, err := os.Stat(filepath.Join(session.Dir, "init.mp4")); err == nil {
				return nil
			}
		}
		_, done, sessionErr := session.snapshot()
		if done {
			if sessionErr == "" {
				return errors.New("ffmpeg produced no complete fMP4 fragment")
			}
			return errors.New(sessionErr)
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

func fmp4FragmentIndex(path, sessionID string) ([]fmp4Fragment, float64) {
	body, err := os.ReadFile(path)
	if err != nil {
		return nil, 0
	}
	var fragments []fmp4Fragment
	var start, pendingDuration float64
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
		numberText := strings.TrimSuffix(strings.TrimPrefix(line, "fragment-"), ".m4s")
		number, parseErr := strconv.Atoi(numberText)
		if parseErr != nil {
			continue
		}
		fragments = append(fragments, fmp4Fragment{Number: number, Start: start, Duration: pendingDuration,
			URL: "/api/video/fmp4/" + sessionID + "/" + line})
		start += pendingDuration
		pendingDuration = 0
	}
	return fragments, start
}

func (s *Server) videoFMP4Index(w http.ResponseWriter, r *http.Request) {
	session := s.videoFMP4Session(chi.URLParam(r, "session"))
	if session == nil {
		problem(w, http.StatusNotFound, "fMP4 fragment session not found")
		return
	}
	target, _ := strconv.ParseFloat(r.URL.Query().Get("time"), 64)
	target = max(0, target)
	deadline := time.NewTimer(videoFMP4IndexWait)
	defer deadline.Stop()
	ticker := time.NewTicker(100 * time.Millisecond)
	defer ticker.Stop()
	for {
		fragments, available := fmp4FragmentIndex(session.Playlist, session.ID)
		_, done, sessionErr := session.snapshot()
		startIndex := 0
		for startIndex < len(fragments) && fragments[startIndex].Start+fragments[startIndex].Duration <= target+.001 {
			startIndex++
		}
		if startIndex < len(fragments) || done {
			end := min(len(fragments), startIndex+12)
			session.touch()
			w.Header().Set("Cache-Control", "no-store")
			writeJSON(w, http.StatusOK, fmp4IndexResponse{Fragments: fragments[startIndex:end], AvailableUntil: available, Done: done, Error: sessionErr})
			return
		}
		select {
		case <-r.Context().Done():
			return
		case <-deadline.C:
			w.Header().Set("Cache-Control", "no-store")
			writeJSON(w, http.StatusOK, fmp4IndexResponse{Fragments: []fmp4Fragment{}, AvailableUntil: available, Done: false})
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
	session.touch()
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
	// Releasing a player is not a cache eviction. Continue remuxing so seeks and
	// refreshes can reuse this exact init segment, index, fragments and process.
	session.touch()
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) reusableVideoFMP4Session(fileID, audioMode, preferredID string) *videoFMP4Session {
	s.videoFMP4Mu.RLock()
	defer s.videoFMP4Mu.RUnlock()
	if preferred := s.videoFMP4Sessions[preferredID]; preferred != nil && preferred.FileID == fileID && preferred.AudioMode == audioMode {
		if fragments, _ := fmp4FragmentIndex(preferred.Playlist, preferred.ID); len(fragments) > 0 {
			preferred.touch()
			return preferred
		}
	}
	for _, session := range s.videoFMP4Sessions {
		if session.FileID != fileID || session.AudioMode != audioMode {
			continue
		}
		if fragments, _ := fmp4FragmentIndex(session.Playlist, session.ID); len(fragments) > 0 {
			session.touch()
			return session
		}
	}
	return nil
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
		lastAccess, _, _ := session.snapshot()
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
				lastAccess, _, _ := session.snapshot()
				if now.Sub(lastAccess) > videoFMP4IdleTTL {
					expired = append(expired, id)
				}
			}
			s.videoFMP4Mu.RUnlock()
			for _, id := range expired {
				s.removeVideoFMP4Session(id)
			}
		}
	}
}
