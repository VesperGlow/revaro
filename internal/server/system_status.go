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
	Storage  struct {
		Status     string `json:"status"`
		Bytes      int64  `json:"bytes"`
		TrashBytes int64  `json:"trash_bytes"`
		FileCount  int64  `json:"file_count"`
	} `json:"storage"`
	Cache struct {
		Status        string                     `json:"status"`
		MemoryBytes   int64                      `json:"memory_bytes"`
		DiskBytes     int64                      `json:"disk_bytes"`
		MemoryEntries int                        `json:"memory_entries"`
		DiskEntries   int                        `json:"disk_entries"`
		Classes       map[string]systemClassStat `json:"classes,omitempty"`
	} `json:"cache"`
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

	if err := s.db.QueryRowContext(ctx, `SELECT COALESCE(SUM(size),0),COALESCE(SUM(CASE WHEN deleted_at IS NOT NULL THEN size ELSE 0 END),0),COUNT(*) FROM files WHERE kind='file' AND status='ready'`).Scan(&out.Storage.Bytes, &out.Storage.TrashBytes, &out.Storage.FileCount); err != nil {
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
