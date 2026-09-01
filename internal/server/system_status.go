package server

import (
	"context"
	"net/http"
	"time"
)

type systemComponent struct {
	Status string `json:"status"`
	Bytes  int64  `json:"bytes,omitempty"`
}

type systemStatusResponse struct {
	Status   string          `json:"status"`
	Database systemComponent `json:"database"`
	Storage  systemComponent `json:"storage"`
	Cache    struct {
		Status        string `json:"status"`
		MemoryBytes   int64  `json:"memory_bytes"`
		DiskBytes     int64  `json:"disk_bytes"`
		MemoryEntries int    `json:"memory_entries"`
		DiskEntries   int    `json:"disk_entries"`
	} `json:"cache"`
	Tasks struct {
		Status  string `json:"status"`
		Running int64  `json:"running"`
		Queued  int64  `json:"queued"`
		Waiting int64  `json:"waiting"`
		Failed  int64  `json:"failed"`
	} `json:"tasks"`
	ObjectCleanup struct {
		Status  string `json:"status"`
		Pending int64  `json:"pending"`
	} `json:"object_cleanup"`
	MediaSessions struct {
		Status   string `json:"status"`
		AudioHLS int    `json:"audio_hls"`
		VideoHLS int    `json:"video_hls"`
		FMP4     int    `json:"fmp4"`
	} `json:"media_sessions"`
	BT struct {
		Status    string `json:"status"`
		Enabled   bool   `json:"enabled"`
		Available bool   `json:"available"`
	} `json:"bt"`
}

func (s *Server) systemStatus(w http.ResponseWriter, r *http.Request) {
	ctx, cancel := context.WithTimeout(r.Context(), 3*time.Second)
	defer cancel()
	out := systemStatusResponse{Status: "ok"}
	degrade := func(component *string) { *component = "degraded"; out.Status = "degraded" }

	out.Database.Status = "ok"
	var pages, pageSize int64
	if err := s.db.QueryRowContext(ctx, `PRAGMA page_count`).Scan(&pages); err != nil {
		degrade(&out.Database.Status)
	} else if err := s.db.QueryRowContext(ctx, `PRAGMA page_size`).Scan(&pageSize); err != nil {
		degrade(&out.Database.Status)
	} else {
		out.Database.Bytes = pages * pageSize
	}

	out.Storage.Status = "ok"
	if err := s.storage.Ping(ctx); err != nil {
		degrade(&out.Storage.Status)
	}

	out.Cache.Status = "ok"
	if s.cache == nil {
		degrade(&out.Cache.Status)
	} else {
		stats := s.cache.Stats()
		out.Cache.MemoryBytes, out.Cache.DiskBytes = stats.MemoryBytes, stats.DiskBytes
		out.Cache.MemoryEntries, out.Cache.DiskEntries = stats.MemoryEntries, stats.DiskEntries
	}

	out.Tasks.Status = "ok"
	if err := s.db.QueryRowContext(ctx, `SELECT COUNT(*) FILTER (WHERE status='running'), COUNT(*) FILTER (WHERE status IN ('queued','retrying')), COUNT(*) FILTER (WHERE status='waiting_input'), COUNT(*) FILTER (WHERE status='failed') FROM tasks`).Scan(&out.Tasks.Running, &out.Tasks.Queued, &out.Tasks.Waiting, &out.Tasks.Failed); err != nil {
		degrade(&out.Tasks.Status)
	}

	out.ObjectCleanup.Status = "ok"
	if err := s.db.QueryRowContext(ctx, `SELECT COUNT(*) FROM object_cleanup`).Scan(&out.ObjectCleanup.Pending); err != nil {
		degrade(&out.ObjectCleanup.Status)
	}

	out.MediaSessions.Status = "ok"
	s.audioHLSMu.RLock()
	out.MediaSessions.AudioHLS = len(s.audioHLSSessions)
	s.audioHLSMu.RUnlock()
	s.videoHLSMu.RLock()
	out.MediaSessions.VideoHLS = len(s.videoHLSSessions)
	s.videoHLSMu.RUnlock()
	s.videoFMP4Mu.RLock()
	out.MediaSessions.FMP4 = len(s.videoFMP4Sessions)
	s.videoFMP4Mu.RUnlock()

	out.BT.Enabled = s.cfg.BTEnabled
	out.BT.Available = s.downloads != nil
	out.BT.Status = "ok"
	if out.BT.Enabled && !out.BT.Available {
		degrade(&out.BT.Status)
	}
	writeJSON(w, http.StatusOK, out)
}
