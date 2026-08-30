package server

import (
	"context"
	"errors"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
	"github.com/VesperGlow/revaro/internal/storage"
	"github.com/go-chi/chi/v5"
)

const audioHLSIdleTTL = 20 * time.Minute
const mediaFallbackDuration = 3 * time.Minute

var audioHLSSegmentName = regexp.MustCompile(`^segment-[0-9]{6}\.ts$`)

type audioHLSSession struct {
	ID       string
	FileID   string
	Start    float64
	Dir      string
	Playlist string

	mu         sync.RWMutex
	lastAccess time.Time
	err        string
	done       bool
	cancel     context.CancelFunc
	doneCh     chan struct{}
	stopOnce   sync.Once
}

func (session *audioHLSSession) touch() {
	session.mu.Lock()
	session.lastAccess = time.Now()
	session.mu.Unlock()
}

func (session *audioHLSSession) snapshot() (time.Time, bool, string) {
	session.mu.RLock()
	defer session.mu.RUnlock()
	return session.lastAccess, session.done, session.err
}

func (session *audioHLSSession) finish(err error) {
	session.mu.Lock()
	session.done = true
	if err != nil && !errors.Is(err, context.Canceled) {
		session.err = err.Error()
	}
	session.mu.Unlock()
	close(session.doneCh)
}

func (session *audioHLSSession) stop() {
	session.stopOnce.Do(func() {
		session.cancel()
		select {
		case <-session.doneCh:
		case <-time.After(3 * time.Second):
		}
		_ = os.RemoveAll(session.Dir)
	})
}

type startAudioHLSRequest struct {
	Start float64 `json:"start"`
}

type startAudioHLSResponse struct {
	SessionID   string  `json:"session_id"`
	PlaylistURL string  `json:"playlist_url"`
	Start       float64 `json:"start"`
}

func (s *Server) startAudioHLS(w http.ResponseWriter, r *http.Request) {
	f, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || f.Kind != "file" || f.Status != "ready" || !isAudioSource(f) {
		problem(w, http.StatusNotFound, "ready audio file not found")
		return
	}
	var in startAudioHLSRequest
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	if in.Start < 0 || in.Start > 7*24*60*60 {
		problem(w, http.StatusBadRequest, "invalid HLS start time")
		return
	}
	var durationMS int64
	if err := s.db.QueryRowContext(r.Context(), `SELECT duration_ms FROM audio_media WHERE file_id=?`, f.ID).Scan(&durationMS); err == nil && durationMS > 0 && int64(in.Start*1000) >= durationMS {
		problem(w, http.StatusBadRequest, "HLS start time is beyond the audio duration")
		return
	}
	select {
	case s.audioHLSSlots <- struct{}{}:
	default:
		problem(w, http.StatusTooManyRequests, "too many compatibility streams are active")
		return
	}

	if err := os.MkdirAll(s.cfg.WorkDir, 0o700); err != nil {
		problem(w, 500, "could not create media workspace")
		return
	}
	dir, err := os.MkdirTemp(s.cfg.WorkDir, "revaro-audio-hls-")
	if err != nil {
		<-s.audioHLSSlots
		problem(w, http.StatusInternalServerError, "could not create compatibility stream")
		return
	}
	ctx, cancel := context.WithCancel(s.audioHLSCtx)
	session := &audioHLSSession{
		ID: ids.New(), FileID: f.ID, Start: float64(int64(in.Start*1000)) / 1000,
		Dir: dir, Playlist: filepath.Join(dir, "index.m3u8"), lastAccess: time.Now(),
		cancel: cancel, doneCh: make(chan struct{}),
	}
	s.audioHLSMu.Lock()
	s.audioHLSSessions[session.ID] = session
	s.audioHLSMu.Unlock()
	if !s.runBackground(func() { s.runAudioHLS(ctx, f, session) }) {
		s.removeAudioHLSSession(session.ID)
		problem(w, http.StatusServiceUnavailable, "service is shutting down")
		return
	}

	if err := waitForAudioHLS(r.Context(), session); err != nil {
		s.removeAudioHLSSession(session.ID)
		if errors.Is(err, context.DeadlineExceeded) {
			problem(w, http.StatusGatewayTimeout, "compatibility stream took too long to start")
		} else {
			problem(w, http.StatusBadGateway, "compatibility stream failed to start")
		}
		return
	}
	writeJSON(w, http.StatusCreated, startAudioHLSResponse{
		SessionID: session.ID, PlaylistURL: "/api/audio/hls/" + session.ID + "/index.m3u8", Start: session.Start,
	})
}

func waitForAudioHLS(requestCtx context.Context, session *audioHLSSession) error {
	timer := time.NewTimer(20 * time.Second)
	defer timer.Stop()
	ticker := time.NewTicker(50 * time.Millisecond)
	defer ticker.Stop()
	for {
		if body, err := os.ReadFile(session.Playlist); err == nil && strings.Contains(string(body), "#EXTINF:") {
			return nil
		}
		_, done, sessionErr := session.snapshot()
		if done {
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

func (s *Server) runAudioHLS(ctx context.Context, f File, session *audioHLSSession) {
	defer func() { <-s.audioHLSSlots }()
	engine, ok := s.storage.(storage.MediaEngine)
	if !ok {
		session.finish(errors.New("Rust media engine is unavailable"))
		return
	}
	started, err := engine.GenerateHLS(ctx, f.objectKey, session.Dir, session.Start, true)
	if err == nil {
		err = waitForDataPlaneHLSJob(ctx, engine, started.JobID)
	}
	if err != nil {
		s.log.Warn("audio HLS transcoder stopped", "file", f.ID, "session", session.ID, "error", err)
	}
	session.finish(err)
}

func (s *Server) startMediaHLSSource(ctx context.Context, f File) (string, func(), error) {
	u, err := s.storage.PresignGetObject(ctx, f.objectKey, f.Name, responseMime(f), true, s.cfg.PresignExpires)
	if err != nil {
		return "", nil, err
	}
	return u, func() {}, nil
}

func (s *Server) audioHLSAsset(w http.ResponseWriter, r *http.Request) {
	session := s.audioHLSSession(chi.URLParam(r, "session"))
	if session == nil {
		problem(w, http.StatusNotFound, "compatibility stream not found")
		return
	}
	asset := chi.URLParam(r, "asset")
	if asset != "index.m3u8" && !audioHLSSegmentName.MatchString(asset) {
		problem(w, http.StatusNotFound, "compatibility stream asset not found")
		return
	}
	session.touch()
	path := filepath.Join(session.Dir, asset)
	if _, err := os.Stat(path); err != nil {
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
	http.ServeFile(w, r, path)
}

func (s *Server) stopAudioHLS(w http.ResponseWriter, r *http.Request) {
	if s.removeAudioHLSSession(chi.URLParam(r, "session")) == nil {
		problem(w, http.StatusNotFound, "compatibility stream not found")
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) audioHLSSession(id string) *audioHLSSession {
	s.audioHLSMu.RLock()
	defer s.audioHLSMu.RUnlock()
	return s.audioHLSSessions[id]
}

func (s *Server) removeAudioHLSSession(id string) *audioHLSSession {
	s.audioHLSMu.Lock()
	session := s.audioHLSSessions[id]
	delete(s.audioHLSSessions, id)
	s.audioHLSMu.Unlock()
	if session != nil {
		session.stop()
	}
	return session
}

func (s *Server) cleanupAudioHLSSessions() {
	ticker := time.NewTicker(time.Minute)
	defer ticker.Stop()
	for {
		select {
		case <-s.audioHLSCtx.Done():
			return
		case now := <-ticker.C:
			var expired []string
			s.audioHLSMu.RLock()
			for id, session := range s.audioHLSSessions {
				lastAccess, _, _ := session.snapshot()
				if now.Sub(lastAccess) > audioHLSIdleTTL {
					expired = append(expired, id)
				}
			}
			s.audioHLSMu.RUnlock()
			for _, id := range expired {
				s.removeAudioHLSSession(id)
			}
		}
	}
}
