package server

import (
	"net/http"

	"github.com/go-chi/chi/v5"
)

type storedAudioChapter struct {
	Title   string `json:"title"`
	StartMS int64  `json:"start_ms"`
	EndMS   int64  `json:"end_ms"`
}
type audioChapterResponse struct {
	ID    int     `json:"id"`
	Title string  `json:"title"`
	Start float64 `json:"start"`
	End   float64 `json:"end"`
}

func (s *Server) audioMediaInfo(w http.ResponseWriter, r *http.Request) {
	f, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || !isAudioSource(f) {
		problem(w, http.StatusNotFound, "ready audio file not found")
		return
	}
	metadata, err := s.ensureMediaMetadata(r.Context(), f)
	if err != nil {
		problem(w, http.StatusNotFound, "audio metadata is not available")
		return
	}
	chapters := make([]audioChapterResponse, 0, len(metadata.Chapters))
	for index, chapter := range metadata.Chapters {
		chapters = append(chapters, audioChapterResponse{ID: index + 1, Title: chapter.Title, Start: float64(chapter.StartMS) / 1000, End: float64(chapter.EndMS) / 1000})
	}
	hasCover := metadata.VideoCodec != ""
	cover := ""
	if hasCover {
		cover = "/api/files/" + f.ID + "/thumbnail?v=" + f.ETag
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"duration": float64(metadata.DurationMS) / 1000, "chapters": chapters,
		"cover_url": cover, "has_cover": hasCover,
	})
}
