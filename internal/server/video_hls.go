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
	videoHLSIdleTTL       = 20 * time.Minute
	videoHLSStartupChunks = 4 // 4s segments: keep roughly 16 seconds ahead before playback.
)

var videoHLSSegmentName = regexp.MustCompile(`^segment-[0-9]{6}\.ts$`)

type videoHLSSession struct {
	ID       string
	FileID   string
	Start    float64
	Duration float64
	Dir      string
	Playlist string

	mu          sync.RWMutex
	lastAccess  time.Time
	err         string
	done        bool
	videoCodec  string
	audioCodec  string
	transcoding bool
	cancel      context.CancelFunc
	doneCh      chan struct{}
	stopOnce    sync.Once
}

func (session *videoHLSSession) touch() {
	session.mu.Lock()
	session.lastAccess = time.Now()
	session.mu.Unlock()
}

func (session *videoHLSSession) snapshot() (time.Time, bool, string, float64, string, string, bool) {
	session.mu.RLock()
	defer session.mu.RUnlock()
	return session.lastAccess, session.done, session.err, session.Duration, session.videoCodec, session.audioCodec, session.transcoding
}

func (session *videoHLSSession) setProbe(duration float64, videoCodec, audioCodec string, transcoding bool) {
	session.mu.Lock()
	session.Duration = duration
	session.videoCodec = videoCodec
	session.audioCodec = audioCodec
	session.transcoding = transcoding
	session.mu.Unlock()
}

func (session *videoHLSSession) finish(err error) {
	session.mu.Lock()
	session.done = true
	if err != nil && !errors.Is(err, context.Canceled) {
		session.err = err.Error()
	}
	session.mu.Unlock()
	close(session.doneCh)
}

func (session *videoHLSSession) stop() {
	session.stopOnce.Do(func() {
		session.cancel()
		select {
		case <-session.doneCh:
		case <-time.After(3 * time.Second):
		}
		_ = os.RemoveAll(session.Dir)
	})
}

type startVideoHLSRequest struct {
	Start             float64 `json:"start"`
	PreviousSessionID string  `json:"previous_session_id"`
}

type startVideoHLSResponse struct {
	SessionID   string  `json:"session_id"`
	PlaylistURL string  `json:"playlist_url"`
	Start       float64 `json:"start"`
	Duration    float64 `json:"duration"`
	VideoCodec  string  `json:"video_codec"`
	AudioCodec  string  `json:"audio_codec"`
	Transcoding bool    `json:"transcoding"`
}

func (s *Server) startVideoHLS(w http.ResponseWriter, r *http.Request) {
	f, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || f.Kind != "file" || f.Status != "ready" || !isVideoSource(f) {
		problem(w, http.StatusNotFound, "ready video file not found")
		return
	}
	var in startVideoHLSRequest
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	if in.Start < 0 || in.Start > 7*24*60*60 {
		problem(w, http.StatusBadRequest, "invalid HLS start time")
		return
	}
	if _, err := exec.LookPath(s.cfg.FFmpegPath); err != nil {
		problem(w, http.StatusServiceUnavailable, "ffmpeg is unavailable")
		return
	}
	if _, err := ffprobeFor(s.cfg.FFmpegPath); err != nil {
		problem(w, http.StatusServiceUnavailable, "ffprobe is unavailable")
		return
	}
	// A seek can rebuild hls.js without throwing away the FFmpeg output. Reuse a
	// presented session whenever its event playlist already covers the target.
	if response, ok := s.reusableVideoHLSResponse(in.PreviousSessionID, f.ID, in.Start); ok {
		writeJSON(w, http.StatusCreated, response)
		return
	}
	// A page refresh frequently prevents the browser's best-effort DELETE from
	// reaching us. Reclaim only the token presented by the same file before a
	// replacement transcode is started, so another tab/device is never stopped.
	if in.PreviousSessionID != "" {
		s.removeReplacedVideoHLSSession(in.PreviousSessionID, f.ID)
	}
	slotTimer := time.NewTimer(5 * time.Second)
	defer slotTimer.Stop()
	select {
	case s.videoHLSSlots <- struct{}{}:
	case <-r.Context().Done():
		return
	case <-slotTimer.C:
		problem(w, http.StatusTooManyRequests, "another compatibility video stream is already active")
		return
	}
	dir, err := os.MkdirTemp("", "revaro-video-hls-")
	if err != nil {
		<-s.videoHLSSlots
		problem(w, http.StatusInternalServerError, "could not create compatibility stream")
		return
	}
	ctx, cancel := context.WithCancel(s.audioHLSCtx)
	session := &videoHLSSession{
		ID: ids.New(), FileID: f.ID, Start: float64(int64(in.Start*1000)) / 1000,
		Dir: dir, Playlist: filepath.Join(dir, "index.m3u8"), lastAccess: time.Now(),
		cancel: cancel, doneCh: make(chan struct{}),
	}
	s.videoHLSMu.Lock()
	s.videoHLSSessions[session.ID] = session
	s.videoHLSMu.Unlock()
	go s.runVideoHLS(ctx, f, session)
	if err := waitForVideoHLS(r.Context(), session); err != nil {
		s.removeVideoHLSSession(session.ID)
		if errors.Is(err, context.DeadlineExceeded) {
			problem(w, http.StatusGatewayTimeout, "video compatibility stream took too long to start")
		} else {
			s.log.Warn("video compatibility stream failed to start", "file", f.ID, "error", err)
			problem(w, http.StatusBadGateway, "video compatibility stream failed to start")
		}
		return
	}
	_, _, _, duration, videoCodec, audioCodec, transcoding := session.snapshot()
	writeJSON(w, http.StatusCreated, startVideoHLSResponse{
		SessionID: session.ID, PlaylistURL: "/api/video/hls/" + session.ID + "/index.m3u8",
		Start: session.Start, Duration: duration, VideoCodec: videoCodec, AudioCodec: audioCodec, Transcoding: transcoding,
	})
}

func (s *Server) reusableVideoHLSResponse(id, fileID string, target float64) (startVideoHLSResponse, bool) {
	if id == "" {
		return startVideoHLSResponse{}, false
	}
	s.videoHLSMu.RLock()
	session := s.videoHLSSessions[id]
	if session == nil || session.FileID != fileID {
		s.videoHLSMu.RUnlock()
		return startVideoHLSResponse{}, false
	}
	_, _, _, duration, videoCodec, audioCodec, transcoding := session.snapshot()
	_, available := videoHLSPlaylistState(session.Playlist)
	if target < session.Start || target > session.Start+available-.25 {
		s.videoHLSMu.RUnlock()
		return startVideoHLSResponse{}, false
	}
	session.touch()
	response := startVideoHLSResponse{
		SessionID: session.ID, PlaylistURL: "/api/video/hls/" + session.ID + "/index.m3u8",
		Start: session.Start, Duration: duration, VideoCodec: videoCodec, AudioCodec: audioCodec, Transcoding: transcoding,
	}
	s.videoHLSMu.RUnlock()
	return response, true
}

func (s *Server) removeReplacedVideoHLSSession(id, fileID string) {
	s.videoHLSMu.Lock()
	session := s.videoHLSSessions[id]
	if session != nil && session.FileID == fileID {
		delete(s.videoHLSSessions, id)
	} else {
		session = nil
	}
	s.videoHLSMu.Unlock()
	if session != nil {
		session.stop()
	}
}

func waitForVideoHLS(requestCtx context.Context, session *videoHLSSession) error {
	timer := time.NewTimer(90 * time.Second)
	defer timer.Stop()
	ticker := time.NewTicker(75 * time.Millisecond)
	defer ticker.Stop()
	for {
		segments, _ := videoHLSPlaylistState(session.Playlist)
		if segments >= videoHLSStartupChunks {
			return nil
		}
		_, done, sessionErr, _, _, _, _ := session.snapshot()
		if done {
			if segments > 0 {
				// Short clips may finish with fewer than four segments.
				return nil
			}
			if sessionErr == "" {
				return errors.New("ffmpeg produced no HLS segments")
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

func videoHLSPlaylistState(path string) (int, float64) {
	body, err := os.ReadFile(path)
	if err != nil {
		return 0, 0
	}
	segments := 0
	var duration float64
	for _, line := range strings.Split(string(body), "\n") {
		if !strings.HasPrefix(line, "#EXTINF:") {
			continue
		}
		value := strings.TrimSuffix(strings.TrimPrefix(line, "#EXTINF:"), ",")
		if parsed, err := strconv.ParseFloat(value, 64); err == nil && parsed > 0 {
			duration += parsed
		}
		segments++
	}
	return segments, duration
}

type videoProbe struct {
	Streams []struct {
		CodecType string `json:"codec_type"`
		CodecName string `json:"codec_name"`
	} `json:"streams"`
	Format struct {
		Duration string `json:"duration"`
	} `json:"format"`
}

func probeVideoSource(ctx context.Context, ffmpeg, sourceURL string) (float64, string, string, error) {
	ffprobe, err := ffprobeFor(ffmpeg)
	if err != nil {
		return 0, "", "", err
	}
	cmd := exec.CommandContext(ctx, ffprobe, "-v", "error", "-show_entries", "format=duration:stream=codec_type,codec_name", "-of", "json", sourceURL)
	output, err := cmd.Output()
	if err != nil {
		return 0, "", "", err
	}
	var result videoProbe
	if err := json.Unmarshal(output, &result); err != nil {
		return 0, "", "", err
	}
	duration, _ := strconv.ParseFloat(result.Format.Duration, 64)
	videoCodec, audioCodec := "", ""
	for _, stream := range result.Streams {
		if stream.CodecType == "video" && videoCodec == "" {
			videoCodec = stream.CodecName
		}
		if stream.CodecType == "audio" && audioCodec == "" {
			audioCodec = stream.CodecName
		}
	}
	if duration <= 0 || videoCodec == "" {
		return 0, "", "", errors.New("video duration or codec is unavailable")
	}
	return duration, videoCodec, audioCodec, nil
}

func (s *Server) runVideoHLS(ctx context.Context, f File, session *videoHLSSession) {
	defer func() { <-s.videoHLSSlots }()
	sourceURL, closeSource, err := s.startMediaHLSSource(ctx, f)
	if err != nil {
		session.finish(err)
		return
	}
	defer closeSource()
	duration, videoCodec, audioCodec, err := probeVideoSource(ctx, s.cfg.FFmpegPath, sourceURL)
	if err != nil {
		session.finish(fmt.Errorf("ffprobe: %w", err))
		return
	}
	if session.Start >= duration {
		session.finish(errors.New("HLS start time is beyond the video duration"))
		return
	}
	transcoding := videoCodec != "h264"
	session.setProbe(duration, videoCodec, audioCodec, transcoding)
	args := []string{"-hide_banner", "-loglevel", "error"}
	if session.Start > 0 {
		args = append(args, "-ss", strconv.FormatFloat(session.Start, 'f', 3, 64))
	}
	args = append(args, "-i", sourceURL, "-map", "0:v:0", "-map", "0:a:0?", "-sn", "-dn")
	if transcoding {
		args = append(args,
			"-c:v", "libx264", "-preset", "superfast", "-crf", "23", "-pix_fmt", "yuv420p", "-threads", "0",
			"-vf", "scale=w='min(1920,iw)':h='min(1080,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2",
			"-force_key_frames", "expr:gte(t,n_forced*4)")
	} else {
		args = append(args, "-c:v", "copy")
	}
	if audioCodec == "aac" {
		args = append(args, "-c:a", "copy")
	} else {
		args = append(args, "-c:a", "aac", "-b:a", "160k", "-ar", "48000")
	}
	args = append(args,
		"-max_muxing_queue_size", "2048", "-f", "hls", "-hls_time", "4", "-hls_list_size", "0",
		"-hls_playlist_type", "event", "-hls_flags", "temp_file+independent_segments",
		"-hls_segment_filename", filepath.Join(session.Dir, "segment-%06d.ts"), session.Playlist)
	cmd := exec.CommandContext(ctx, s.cfg.FFmpegPath, args...)
	stderr := &limitedBuffer{limit: 64 << 10}
	cmd.Stderr = stderr
	err = cmd.Run()
	if err != nil {
		if ctx.Err() != nil {
			err = ctx.Err()
		} else {
			err = fmt.Errorf("ffmpeg HLS: %w: %s", err, strings.TrimSpace(stderr.String()))
		}
	}
	if err != nil {
		s.log.Warn("video HLS transcoder stopped", "file", f.ID, "session", session.ID, "error", err)
	}
	session.finish(err)
}

func (s *Server) videoHLSAsset(w http.ResponseWriter, r *http.Request) {
	session := s.videoHLSSession(chi.URLParam(r, "session"))
	if session == nil {
		problem(w, http.StatusNotFound, "compatibility stream not found")
		return
	}
	asset := chi.URLParam(r, "asset")
	if asset != "index.m3u8" && !videoHLSSegmentName.MatchString(asset) {
		problem(w, http.StatusNotFound, "compatibility stream asset not found")
		return
	}
	session.touch()
	assetPath := filepath.Join(session.Dir, asset)
	if _, err := os.Stat(assetPath); err != nil {
		problem(w, http.StatusNotFound, "compatibility stream asset is not ready")
		return
	}
	if asset == "index.m3u8" {
		w.Header().Set("Content-Type", "application/vnd.apple.mpegurl")
		w.Header().Set("Cache-Control", "no-store")
	} else {
		w.Header().Set("Content-Type", "video/mp2t")
		w.Header().Set("Cache-Control", "private, max-age=3600")
	}
	http.ServeFile(w, r, assetPath)
}

func (s *Server) stopVideoHLS(w http.ResponseWriter, r *http.Request) {
	if s.removeVideoHLSSession(chi.URLParam(r, "session")) == nil {
		problem(w, http.StatusNotFound, "compatibility stream not found")
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) videoHLSSession(id string) *videoHLSSession {
	s.videoHLSMu.RLock()
	defer s.videoHLSMu.RUnlock()
	return s.videoHLSSessions[id]
}

func (s *Server) removeVideoHLSSession(id string) *videoHLSSession {
	s.videoHLSMu.Lock()
	session := s.videoHLSSessions[id]
	delete(s.videoHLSSessions, id)
	s.videoHLSMu.Unlock()
	if session != nil {
		session.stop()
	}
	return session
}

func (s *Server) cleanupVideoHLSSessions() {
	ticker := time.NewTicker(time.Minute)
	defer ticker.Stop()
	for {
		select {
		case <-s.audioHLSCtx.Done():
			return
		case now := <-ticker.C:
			var expired []string
			s.videoHLSMu.RLock()
			for id, session := range s.videoHLSSessions {
				lastAccess, _, _, _, _, _, _ := session.snapshot()
				if now.Sub(lastAccess) > videoHLSIdleTTL {
					expired = append(expired, id)
				}
			}
			s.videoHLSMu.RUnlock()
			for _, id := range expired {
				s.removeVideoHLSSession(id)
			}
		}
	}
}
