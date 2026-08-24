package server

import (
	"database/sql"
	"math"
	"net/http"
	"time"

	"github.com/go-chi/chi/v5"
)

type mediaProgressResponse struct {
	Position  float64 `json:"position"`
	Duration  float64 `json:"duration"`
	UpdatedAt string  `json:"updated_at,omitempty"`
}

func (s *Server) mediaProgress(w http.ResponseWriter, r *http.Request) {
	f, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || f.Kind != "file" || f.Status != "ready" || (!isAudioSource(f) && !isVideoSource(f)) {
		problem(w, http.StatusNotFound, "ready media file not found")
		return
	}
	var positionMS, durationMS int64
	var updatedAt string
	err = s.db.QueryRowContext(r.Context(), `SELECT position_ms,duration_ms,updated_at FROM media_progress WHERE file_id=?`, f.ID).Scan(&positionMS, &durationMS, &updatedAt)
	if err == sql.ErrNoRows {
		writeJSON(w, http.StatusOK, mediaProgressResponse{})
		return
	}
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not read media progress")
		return
	}
	writeJSON(w, http.StatusOK, mediaProgressResponse{Position: float64(positionMS) / 1000, Duration: float64(durationMS) / 1000, UpdatedAt: updatedAt})
}

func (s *Server) saveMediaProgress(w http.ResponseWriter, r *http.Request) {
	f, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || f.Kind != "file" || f.Status != "ready" || (!isAudioSource(f) && !isVideoSource(f)) {
		problem(w, http.StatusNotFound, "ready media file not found")
		return
	}
	var in mediaProgressResponse
	if decodeJSON(w, r, &in) != nil {
		return
	}
	if math.IsNaN(in.Position) || math.IsInf(in.Position, 0) || in.Position < 0 || in.Position > 7*24*60*60 ||
		math.IsNaN(in.Duration) || math.IsInf(in.Duration, 0) || in.Duration < 0 || in.Duration > 7*24*60*60 ||
		(in.Duration > 0 && in.Position > in.Duration+5) {
		problem(w, http.StatusBadRequest, "media progress values are invalid")
		return
	}
	positionMS := int64(math.Round(in.Position * 1000))
	durationMS := int64(math.Round(in.Duration * 1000))
	now := time.Now().UTC().Format(time.RFC3339Nano)
	_, err = s.db.ExecContext(r.Context(), `INSERT INTO media_progress(file_id,position_ms,duration_ms,updated_at) VALUES(?,?,?,?) ON CONFLICT(file_id) DO UPDATE SET position_ms=excluded.position_ms,duration_ms=CASE WHEN excluded.duration_ms>0 THEN excluded.duration_ms ELSE media_progress.duration_ms END,updated_at=excluded.updated_at`, f.ID, positionMS, durationMS, now)
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not save media progress")
		return
	}
	writeJSON(w, http.StatusOK, mediaProgressResponse{Position: float64(positionMS) / 1000, Duration: float64(durationMS) / 1000, UpdatedAt: now})
}
