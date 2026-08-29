package server

import (
	"context"
	"errors"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
	"github.com/VesperGlow/revaro/internal/storage"
	"github.com/go-chi/chi/v5"
)

const (
	videoHLSIdleTTL       = 20 * time.Minute
	videoHLSStartupChunks = 2 // 4s segments: start at ~8s, then let hls.js grow the buffer in the background.
	maxVideoHLSSessions   = 6
	maxVideoHLSPerFile    = 2
)

var videoHLSSegmentName = regexp.MustCompile(`^segment-[0-9]{6}\.ts$`)

type videoHLSSession struct {
	ID       string
	FileID   string
	Start    float64
	Duration float64
	Dir      string
	Playlist string

	mu             sync.RWMutex
	lastAccess     time.Time
	err            string
	done           bool
	videoCodec     string
	audioCodec     string
	transcoding    bool
	fallbackReason string
	cancel         context.CancelFunc
	doneCh         chan struct{}
	cancelOnce     sync.Once
	destroyOnce    sync.Once
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

func (session *videoHLSSession) cancelTranscode() {
	session.cancelOnce.Do(func() {
		session.cancel()
		select {
		case <-session.doneCh:
		case <-time.After(3 * time.Second):
		}
	})
}

func (session *videoHLSSession) destroy() {
	session.destroyOnce.Do(func() {
		session.cancelTranscode()
		_ = os.RemoveAll(session.Dir)
	})
}

type startVideoHLSRequest struct {
	Start             float64 `json:"start"`
	PreviousSessionID string  `json:"previous_session_id"`
	FallbackReason    string  `json:"fallback_reason"`
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
	s.log.Info("video HLS requested", "file", f.ID, "fallback_reason", strings.TrimSpace(in.FallbackReason))
	// A seek always destroys the previous workspace before starting at the new
	// offset. That cancels FFmpeg and its in-flight Wasabi Range immediately;
	// no cached window or near-session prewarm is reused.
	s.removeVideoHLSSessionForFile(in.PreviousSessionID, f.ID)
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
	if err := os.MkdirAll(s.cfg.WorkDir, 0o700); err != nil {
		problem(w, 500, "could not create media workspace")
		return
	}
	dir, err := os.MkdirTemp(s.cfg.WorkDir, "revaro-video-hls-")
	if err != nil {
		<-s.videoHLSSlots
		problem(w, http.StatusInternalServerError, "could not create compatibility stream")
		return
	}
	ctx, cancel := context.WithCancel(s.audioHLSCtx)
	session := &videoHLSSession{
		ID: ids.New(), FileID: f.ID, Start: float64(int64(in.Start*1000)) / 1000,
		Dir: dir, Playlist: filepath.Join(dir, "index.m3u8"), lastAccess: time.Now(),
		fallbackReason: strings.TrimSpace(in.FallbackReason), cancel: cancel, doneCh: make(chan struct{}),
	}
	s.videoHLSMu.Lock()
	s.videoHLSSessions[session.ID] = session
	s.videoHLSMu.Unlock()
	s.pruneVideoHLSSessions(session.ID)
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
	writeJSON(w, http.StatusCreated, videoHLSResponse(session))
}

func videoHLSResponse(session *videoHLSSession) startVideoHLSResponse {
	_, _, _, duration, videoCodec, audioCodec, transcoding := session.snapshot()
	return startVideoHLSResponse{
		SessionID: session.ID, PlaylistURL: "/api/video/hls/" + session.ID + "/index.m3u8",
		Start: session.Start, Duration: duration, VideoCodec: videoCodec, AudioCodec: audioCodec, Transcoding: transcoding,
	}
}

func (s *Server) removeVideoHLSSessionForFile(id, fileID string) {
	if id == "" {
		return
	}
	s.videoHLSMu.RLock()
	session := s.videoHLSSessions[id]
	s.videoHLSMu.RUnlock()
	if session == nil || session.FileID != fileID {
		return
	}
	s.removeVideoHLSSession(id)
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
				return errors.New("data plane produced no HLS segments")
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

func (s *Server) runVideoHLS(ctx context.Context, f File, session *videoHLSSession) {
	defer func() { <-s.videoHLSSlots }()
	engine, ok := s.storage.(storage.MediaEngine)
	if !ok {
		session.finish(errors.New("Rust media engine is unavailable"))
		return
	}
	probe, err := engine.ProbeMedia(ctx, f.objectKey)
	if err != nil {
		if !errors.Is(err, context.Canceled) {
			s.log.Warn("video HLS transcoder stopped", "file", f.ID, "session", session.ID, "error", err)
		}
		session.finish(err)
		return
	}
	duration, videoCodec, audioCodec := float64(probe.DurationMS)/1000, probe.VideoCodec, probe.AudioCodec
	if session.Start >= duration {
		session.finish(errors.New("HLS start time is beyond the video duration"))
		return
	}
	transcoding := videoCodec != "h264" || (audioCodec != "" && audioCodec != "aac")
	session.setProbe(duration, videoCodec, audioCodec, transcoding)
	s.log.Info("video playback selected", "file", f.ID, "video_codec", videoCodec, "audio_codec", audioCodec,
		"selected_mode", "hls-transcode", "video_transcoding", transcoding, "audio_transcoding", audioCodec != "" && audioCodec != "aac",
		"fallback_reason", session.fallbackReason)
	_, err = engine.GenerateHLS(ctx, f.objectKey, session.Dir, session.Start, false)
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
		session.destroy()
	}
	return session
}

func (s *Server) pruneVideoHLSSessions(keepID string) {
	type cachedSession struct {
		id         string
		fileID     string
		lastAccess time.Time
	}
	s.videoHLSMu.RLock()
	items := make([]cachedSession, 0, len(s.videoHLSSessions))
	for id, session := range s.videoHLSSessions {
		lastAccess, _, _, _, _, _, _ := session.snapshot()
		items = append(items, cachedSession{id: id, fileID: session.FileID, lastAccess: lastAccess})
	}
	s.videoHLSMu.RUnlock()
	sort.Slice(items, func(i, j int) bool { return items[i].lastAccess.Before(items[j].lastAccess) })
	evict := make(map[string]bool)
	perFile := make(map[string]int)
	for _, item := range items {
		perFile[item.fileID]++
	}
	for _, item := range items {
		if item.id != keepID && perFile[item.fileID] > maxVideoHLSPerFile {
			evict[item.id] = true
			perFile[item.fileID]--
		}
	}
	remaining := len(items) - len(evict)
	for _, item := range items {
		if remaining <= maxVideoHLSSessions {
			break
		}
		if item.id != keepID && !evict[item.id] {
			evict[item.id] = true
			remaining--
		}
	}
	for id := range evict {
		s.removeVideoHLSSession(id)
	}
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
			s.pruneMediaCache()
		}
	}
}
