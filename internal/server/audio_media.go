package server

import (
	"database/sql"
	"encoding/json"
	"errors"
	"net/http"
	"path/filepath"
	"time"

	"github.com/VesperGlow/revaro/internal/storage"
	"github.com/go-chi/chi/v5"
)

type storedAudioChapter struct {
	Title   string `json:"title"`
	StartMS int64  `json:"start_ms"`
	EndMS   int64  `json:"end_ms"`
}

type storedAudioSubtitle struct {
	StartMS int64  `json:"start_ms"`
	EndMS   int64  `json:"end_ms"`
	Text    string `json:"text"`
}

type audioChapterResponse struct {
	ID    int     `json:"id"`
	Title string  `json:"title"`
	Start float64 `json:"start"`
	End   float64 `json:"end"`
}

type audioSubtitleResponse struct {
	ID    int     `json:"id"`
	Start float64 `json:"start"`
	End   float64 `json:"end"`
	Text  string  `json:"text"`
}

func (s *Server) audioMediaInfo(w http.ResponseWriter, r *http.Request) {
	f, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || f.Kind != "file" || f.Status != "ready" || !isAudioSource(f) {
		problem(w, http.StatusNotFound, "ready audio file not found")
		return
	}
	var durationMS, streamSize int64
	var chaptersJSON, subtitlesJSON, streamKey, streamETag string
	var hasCover bool
	err = s.db.QueryRowContext(r.Context(), `SELECT duration_ms,chapters_json,subtitles_json,stream_object_key,stream_size,stream_etag,has_cover FROM audio_media WHERE file_id=?`, f.ID).
		Scan(&durationMS, &chaptersJSON, &subtitlesJSON, &streamKey, &streamSize, &streamETag, &hasCover)
	if errors.Is(err, sql.ErrNoRows) {
		problem(w, http.StatusNotFound, "chapter metadata is not available for this audio")
		return
	}
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not read audio metadata")
		return
	}
	var stored []storedAudioChapter
	if err := json.Unmarshal([]byte(chaptersJSON), &stored); err != nil {
		problem(w, http.StatusInternalServerError, "audio chapter metadata is invalid")
		return
	}
	chapters := make([]audioChapterResponse, 0, len(stored))
	for index, chapter := range stored {
		chapters = append(chapters, audioChapterResponse{
			ID: index + 1, Title: chapter.Title,
			Start: float64(chapter.StartMS) / 1000, End: float64(chapter.EndMS) / 1000,
		})
	}
	var storedSubtitles []storedAudioSubtitle
	if err := json.Unmarshal([]byte(subtitlesJSON), &storedSubtitles); err != nil {
		problem(w, http.StatusInternalServerError, "audio subtitle metadata is invalid")
		return
	}
	subtitles := make([]audioSubtitleResponse, 0, len(storedSubtitles))
	for index, cue := range storedSubtitles {
		subtitles = append(subtitles, audioSubtitleResponse{
			ID: index + 1, Start: float64(cue.StartMS) / 1000,
			End: float64(cue.EndMS) / 1000, Text: cue.Text,
		})
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"duration":    float64(durationMS) / 1000,
		"chapters":    chapters,
		"subtitles":   subtitles,
		"stream_url":  "/api/files/" + f.ID + "/audio/stream",
		"cover_url":   coverURL(f.ID, f.ETag, hasCover),
		"has_cover":   hasCover,
		"stream_size": streamSize,
	})
}

func coverURL(fileID, etag string, hasCover bool) string {
	if !hasCover {
		return ""
	}
	return "/api/files/" + fileID + "/thumbnail?v=" + etag
}

func (s *Server) audioMediaStream(w http.ResponseWriter, r *http.Request) {
	f, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || f.Kind != "file" || f.Status != "ready" || !isAudioSource(f) {
		problem(w, http.StatusNotFound, "ready audio file not found")
		return
	}
	var key, etag string
	var size int64
	err = s.db.QueryRowContext(r.Context(), `SELECT stream_object_key,stream_size,stream_etag FROM audio_media WHERE file_id=?`, f.ID).Scan(&key, &size, &etag)
	if errors.Is(err, sql.ErrNoRows) {
		problem(w, http.StatusNotFound, "stream is not available for this audio")
		return
	}
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not read audio stream metadata")
		return
	}
	stream := f
	stream.Name = filepath.Base(f.Name)
	stream.Size = size
	stream.MimeType = responseMime(f)
	stream.ETag = etag
	stream.objectKey = key
	w.Header().Set("Cache-Control", "private, max-age=3600")
	rc, err := s.storage.Open(storage.WithDynamicReadAhead(r.Context()), stream.objectKey)
	if err != nil {
		s.log.Error("audio stream open failed", "file", f.ID, "error", err)
		problem(w, http.StatusBadGateway, "audio stream storage read failed")
		return
	}
	defer rc.Close()
	w.Header().Set("Content-Type", stream.MimeType)
	w.Header().Set("Content-Disposition", "inline")
	var modtime time.Time
	if parsed, err := time.Parse(time.RFC3339Nano, f.UpdatedAt); err == nil {
		modtime = parsed
	}
	http.ServeContent(w, r, stream.Name, modtime, rc)
}

func durationMilliseconds(durations []time.Duration) int64 {
	var total time.Duration
	for _, duration := range durations {
		total += duration
	}
	return max(total.Milliseconds(), 1)
}
