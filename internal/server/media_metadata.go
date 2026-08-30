package server

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"sync"
	"time"

	"github.com/go-chi/chi/v5"
)

type mediaAnalysisScheduler struct {
	slots  chan struct{}
	mu     sync.Mutex
	active map[string]struct{}
	closed bool
	wg     sync.WaitGroup
}

func newMediaAnalysisScheduler(limit int) *mediaAnalysisScheduler {
	return &mediaAnalysisScheduler{slots: make(chan struct{}, limit), active: make(map[string]struct{})}
}

// schedule returns immediately. A file ID can be queued or running only once,
// and no more than two background media probe workers acquire a slot at a time.
func (q *mediaAnalysisScheduler) schedule(ctx context.Context, fileID string, work func(context.Context)) bool {
	q.mu.Lock()
	if q.closed {
		q.mu.Unlock()
		return false
	}
	if _, exists := q.active[fileID]; exists {
		q.mu.Unlock()
		return false
	}
	q.active[fileID] = struct{}{}
	q.wg.Add(1)
	q.mu.Unlock()
	go func() {
		defer q.wg.Done()
		defer func() {
			q.mu.Lock()
			delete(q.active, fileID)
			q.mu.Unlock()
		}()
		select {
		case q.slots <- struct{}{}:
			defer func() { <-q.slots }()
		case <-ctx.Done():
			return
		}
		work(ctx)
	}()
	return true
}

func (q *mediaAnalysisScheduler) close() {
	q.mu.Lock()
	q.closed = true
	q.mu.Unlock()
	q.wg.Wait()
}

type probedMediaMetadata struct {
	DurationMS   int64
	Container    string
	VideoCodec   string
	AudioCodec   string
	Width        int
	Height       int
	Bitrate      int64
	Chapters     []storedAudioChapter
	FrameRate    string
	VideoProfile string
	VideoLevel   int
	Subtitles    []embeddedSubtitle
}

const mediaProbeVersion = 2
const emptyMediaProbeTTL = 24 * time.Hour

type embeddedSubtitle struct {
	Index    int    `json:"index"`
	Codec    string `json:"codec"`
	Language string `json:"language,omitempty"`
	Title    string `json:"title,omitempty"`
	Default  bool   `json:"default"`
	Forced   bool   `json:"forced"`
}

func (s *Server) scheduleMediaAnalysis(f File) {
	if !isAudioSource(f) && !isVideoSource(f) {
		return
	}
	s.mediaAnalysis.schedule(s.audioHLSCtx, f.ID, func(parent context.Context) {
		ctx, cancel := context.WithTimeout(parent, 2*time.Minute)
		defer cancel()
		if _, err := s.ensureMediaMetadata(ctx, f); err != nil && !errors.Is(err, context.Canceled) {
			s.log.Warn("media metadata analysis failed", "file", f.ID, "error", err)
		}
	})
}

func (s *Server) ensureMediaMetadata(ctx context.Context, f File) (probedMediaMetadata, error) {
	result := s.mediaProbeGroup.DoChan(f.ID, func() (any, error) {
		probeCtx, cancel := context.WithTimeout(s.audioHLSCtx, 2*time.Minute)
		defer cancel()
		return s.probeMediaMetadata(probeCtx, f)
	})
	select {
	case <-ctx.Done():
		return probedMediaMetadata{}, ctx.Err()
	case call := <-result:
		if call.Err != nil {
			return probedMediaMetadata{}, call.Err
		}
		return call.Val.(probedMediaMetadata), nil
	}
}

func (s *Server) probeMediaMetadata(ctx context.Context, f File) (probedMediaMetadata, error) {
	var metadata probedMediaMetadata
	var chaptersJSON, subtitlesJSON, analyzedAt string
	err := s.db.QueryRowContext(ctx, `SELECT duration_ms,container,video_codec,audio_codec,width,height,bitrate,chapters_json,frame_rate,video_profile,video_level,subtitles_json,analyzed_at FROM media_metadata WHERE file_id=? AND source_etag=? AND probe_version=?`, f.ID, f.ETag, mediaProbeVersion).
		Scan(&metadata.DurationMS, &metadata.Container, &metadata.VideoCodec, &metadata.AudioCodec, &metadata.Width, &metadata.Height, &metadata.Bitrate, &chaptersJSON, &metadata.FrameRate, &metadata.VideoProfile, &metadata.VideoLevel, &subtitlesJSON, &analyzedAt)
	if err == nil {
		_ = json.Unmarshal([]byte(chaptersJSON), &metadata.Chapters)
		_ = json.Unmarshal([]byte(subtitlesJSON), &metadata.Subtitles)
		analyzed, parseErr := time.Parse(time.RFC3339Nano, analyzedAt)
		if len(metadata.Subtitles) > 0 || parseErr == nil && time.Since(analyzed) < emptyMediaProbeTTL {
			return metadata, nil
		}
		s.log.Info("refreshing empty media probe result", "file", f.ID, "analyzed_at", analyzedAt)
	}
	result, err := s.probeMediaSource(ctx, f)
	if err != nil {
		return metadata, err
	}
	metadata.DurationMS, metadata.Container, metadata.VideoCodec, metadata.AudioCodec = result.DurationMS, result.Container, result.VideoCodec, result.AudioCodec
	metadata.Width, metadata.Height, metadata.Bitrate = result.Width, result.Height, result.Bitrate
	metadata.FrameRate, metadata.VideoProfile, metadata.VideoLevel = result.FrameRate, result.VideoProfile, result.VideoLevel
	for _, chapter := range result.Chapters {
		metadata.Chapters = append(metadata.Chapters, storedAudioChapter{Title: chapter.Title, StartMS: chapter.StartMS, EndMS: chapter.EndMS})
	}
	for _, subtitle := range result.Subtitles {
		metadata.Subtitles = append(metadata.Subtitles, embeddedSubtitle{Index: subtitle.Index, Codec: subtitle.Codec, Language: subtitle.Language, Title: subtitle.Title, Default: subtitle.Default, Forced: subtitle.Forced})
	}
	chapters, _ := json.Marshal(metadata.Chapters)
	subtitles, _ := json.Marshal(metadata.Subtitles)
	now := time.Now().UTC().Format(time.RFC3339Nano)
	_, err = s.db.ExecContext(ctx, `INSERT INTO media_metadata(file_id,duration_ms,container,video_codec,audio_codec,width,height,bitrate,chapters_json,analyzed_at,frame_rate,video_profile,video_level,subtitles_json,source_etag,probe_version) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(file_id) DO UPDATE SET duration_ms=excluded.duration_ms,container=excluded.container,video_codec=excluded.video_codec,audio_codec=excluded.audio_codec,width=excluded.width,height=excluded.height,bitrate=excluded.bitrate,chapters_json=excluded.chapters_json,analyzed_at=excluded.analyzed_at,frame_rate=excluded.frame_rate,video_profile=excluded.video_profile,video_level=excluded.video_level,subtitles_json=excluded.subtitles_json,source_etag=excluded.source_etag,probe_version=excluded.probe_version`,
		f.ID, metadata.DurationMS, metadata.Container, metadata.VideoCodec, metadata.AudioCodec, metadata.Width, metadata.Height, max(metadata.Bitrate, int64(0)), string(chapters), now, metadata.FrameRate, metadata.VideoProfile, metadata.VideoLevel, string(subtitles), f.ETag, mediaProbeVersion)
	if err == nil {
		s.log.Info("media metadata analyzed", "file", f.ID, "container", metadata.Container, "video_codec", metadata.VideoCodec, "audio_codec", metadata.AudioCodec, "duration_ms", metadata.DurationMS, "chapters", len(metadata.Chapters))
	}
	return metadata, err
}

func (s *Server) reanalyzeMedia(w http.ResponseWriter, r *http.Request) {
	file, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || file.Kind != "file" || file.Status != "ready" || !isAudioSource(file) && !isVideoSource(file) {
		problem(w, http.StatusNotFound, "ready media file not found")
		return
	}
	// Drain any scheduled/in-flight analysis before invalidating the row. This
	// prevents an older probe from racing the forced result and writing last.
	_, _ = s.ensureMediaMetadata(r.Context(), file)
	if _, err := s.db.ExecContext(r.Context(), `DELETE FROM media_metadata WHERE file_id=?`, file.ID); err != nil {
		problem(w, http.StatusInternalServerError, "could not invalidate media analysis")
		return
	}
	s.mediaProbeGroup.Forget(file.ID)
	s.clearVideoSubtitleCache(file.ID)
	metadata, err := s.ensureMediaMetadata(r.Context(), file)
	if err != nil {
		problem(w, http.StatusUnprocessableEntity, "media re-analysis failed")
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"status": "ready", "subtitles": len(metadata.Subtitles)})
}
