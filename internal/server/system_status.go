package server

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"sync"
	"time"

	"golang.org/x/sys/unix"
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
	LocalDisk struct {
		Status         string `json:"status"`
		TotalBytes     int64  `json:"total_bytes"`
		FreeBytes      int64  `json:"free_bytes"`
		AvailableBytes int64  `json:"available_bytes"`
		UsedPercent    int    `json:"used_percent"`
	} `json:"local_disk"`
}

func localFilesystemUsage(paths ...string) (total, free, available int64, err error) {
	bestUsed := -1.0
	for _, path := range paths {
		var stat unix.Statfs_t
		if statErr := unix.Statfs(path, &stat); statErr != nil {
			err = statErr
			continue
		}
		t := int64(uint64(stat.Blocks) * uint64(stat.Bsize))
		f := int64(uint64(stat.Bfree) * uint64(stat.Bsize))
		a := int64(uint64(stat.Bavail) * uint64(stat.Bsize))
		if t <= 0 {
			continue
		}
		used := float64(t-a) / float64(t)
		if used > bestUsed {
			total, free, available, bestUsed = t, f, a, used
		}
		err = nil
	}
	if bestUsed < 0 && err == nil {
		err = fmt.Errorf("local filesystem stats unavailable")
	}
	return
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

	out.LocalDisk.Status = "ok"
	total, free, available, err := localFilesystemUsage(s.cfg.DataDir, s.cfg.WorkDir)
	if err != nil {
		degrade(&out.LocalDisk.Status)
	} else {
		out.LocalDisk.TotalBytes, out.LocalDisk.FreeBytes, out.LocalDisk.AvailableBytes = total, free, available
		out.LocalDisk.UsedPercent = int(float64(total-available)/float64(total)*100 + .5)
		degradedLimit := max(int64(5<<30), total/10)
		criticalLimit := max(int64(1<<30), total/50)
		if available < criticalLimit {
			out.LocalDisk.Status = "critical"
			out.Status = "degraded"
		} else if available < degradedLimit {
			degrade(&out.LocalDisk.Status)
		}
	}
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
