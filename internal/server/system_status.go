package server

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"sync"
	"time"
)

type systemComponent struct {
	Status string `json:"status"`
	Bytes  int64  `json:"bytes,omitempty"`
}

type systemClassStat struct {
	Hits          int64 `json:"hits"`
	Misses        int64 `json:"misses"`
	Loads         int64 `json:"loads"`
	LoadErrors    int64 `json:"load_errors"`
	Evictions     int64 `json:"evictions"`
	MemoryBytes   int64 `json:"memory_bytes,omitempty"`
	MemoryEntries int   `json:"memory_entries,omitempty"`
	DiskBytes     int64 `json:"disk_bytes,omitempty"`
	DiskEntries   int   `json:"disk_entries,omitempty"`
}

type systemStatusResponse struct {
	Status   string          `json:"status"`
	Database systemComponent `json:"database"`
	Storage  systemComponent `json:"storage"`
	Cache    struct {
		Status        string                     `json:"status"`
		MemoryBytes   int64                      `json:"memory_bytes"`
		DiskBytes     int64                      `json:"disk_bytes"`
		MemoryEntries int                        `json:"memory_entries"`
		DiskEntries   int                        `json:"disk_entries"`
		Classes       map[string]systemClassStat `json:"classes,omitempty"`
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
	Backup struct {
		Status  string `json:"status"`
		Enabled bool   `json:"enabled"`
	} `json:"backup"`
}

func (s *Server) collectSystemStatus(parent context.Context) systemStatusResponse {
	ctx, cancel := context.WithTimeout(parent, 3*time.Second)
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
		classes := make(map[string]systemClassStat, len(stats.Classes))
		for name, cs := range stats.Classes {
			classes[name] = systemClassStat{
				Hits: cs.Hits, Misses: cs.Misses, Loads: cs.Loads, LoadErrors: cs.LoadErrors, Evictions: cs.Evictions,
				MemoryBytes: cs.MemoryBytes, MemoryEntries: cs.MemoryEntries,
				DiskBytes: cs.DiskBytes, DiskEntries: cs.DiskEntries,
			}
		}
		out.Cache.Classes = classes
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

	// 备份状态只反映配置开关：备份执行失败不会拖垮主服务，也不会在这里
	// 报错，下次调度会自动重试。
	out.Backup.Enabled = s.cfg.BackupEnabled
	out.Backup.Status = "ok"
	return out
}

func (s *Server) refreshSystemStatus() {
	out := s.collectSystemStatus(context.Background())
	s.statusMu.Lock()
	s.statusSnapshot = out
	for ch := range s.statusSubscribers {
		select {
		case ch <- out:
		default:
		}
	}
	s.statusMu.Unlock()
}

func (s *Server) startSystemStatusSnapshots() {
	s.refreshSystemStatus()
	s.runBackground(func() {
		ticker := time.NewTicker(15 * time.Second)
		defer ticker.Stop()
		for {
			select {
			case <-s.statusStop:
				return
			case <-ticker.C:
				s.refreshSystemStatus()
			}
		}
	})
}

func (s *Server) currentSystemStatus() systemStatusResponse {
	s.statusMu.RLock()
	defer s.statusMu.RUnlock()
	return s.statusSnapshot
}

func (s *Server) systemStatus(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, http.StatusOK, s.currentSystemStatus())
}

func (s *Server) subscribeSystemStatus() (<-chan systemStatusResponse, func()) {
	ch := make(chan systemStatusResponse, 1)
	s.statusMu.Lock()
	s.statusSubscribers[ch] = struct{}{}
	current := s.statusSnapshot
	s.statusMu.Unlock()
	ch <- current
	var once sync.Once
	return ch, func() {
		once.Do(func() {
			s.statusMu.Lock()
			if _, ok := s.statusSubscribers[ch]; ok {
				delete(s.statusSubscribers, ch)
				close(ch)
			}
			s.statusMu.Unlock()
		})
	}
}

func (s *Server) systemStatusStream(w http.ResponseWriter, r *http.Request) {
	flusher, ok := w.(http.Flusher)
	if !ok {
		problem(w, http.StatusInternalServerError, "streaming is unavailable")
		return
	}
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache, no-transform")
	w.Header().Set("Connection", "keep-alive")
	w.Header().Set("X-Accel-Buffering", "no")
	updates, unsubscribe := s.subscribeSystemStatus()
	defer unsubscribe()
	keepalive := time.NewTicker(20 * time.Second)
	defer keepalive.Stop()
	for {
		select {
		case <-r.Context().Done():
			return
		case status, open := <-updates:
			if !open {
				return
			}
			raw, err := json.Marshal(status)
			if err != nil {
				return
			}
			if _, err = fmt.Fprintf(w, "event: status\ndata: %s\n\n", raw); err != nil {
				return
			}
			flusher.Flush()
		case <-keepalive.C:
			if _, err := fmt.Fprint(w, ": keepalive\n\n"); err != nil {
				return
			}
			flusher.Flush()
		}
	}
}
