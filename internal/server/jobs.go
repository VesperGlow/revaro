package server

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"sync"
	"time"
)

const (
	JobQueued    = "queued"
	JobRunning   = "running"
	JobWaiting   = "waiting"
	JobCompleted = "completed"
	JobFailed    = "failed"
	JobCancelled = "cancelled"
)

// JobManager is the common lifecycle/event plane for background work. Media
// playback sessions intentionally remain separate because they are ephemeral
// stream resources rather than user-visible jobs.
type JobManager struct {
	mu     sync.RWMutex
	subs   map[chan struct{}]struct{}
	closed bool
}

func NewJobManager() *JobManager { return &JobManager{subs: make(map[chan struct{}]struct{})} }

func (m *JobManager) Changed() {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.closed {
		return
	}
	for ch := range m.subs {
		select {
		case ch <- struct{}{}:
		default:
		}
	}
}

func (m *JobManager) Subscribe() (<-chan struct{}, func()) {
	ch := make(chan struct{}, 1)
	m.mu.Lock()
	if !m.closed {
		m.subs[ch] = struct{}{}
	}
	m.mu.Unlock()
	var once sync.Once
	return ch, func() {
		once.Do(func() {
			m.mu.Lock()
			if _, ok := m.subs[ch]; ok {
				delete(m.subs, ch)
				close(ch)
			}
			m.mu.Unlock()
		})
	}
}

func (m *JobManager) Close() {
	m.mu.Lock()
	if !m.closed {
		m.closed = true
		for ch := range m.subs {
			close(ch)
		}
		m.subs = map[chan struct{}]struct{}{}
	}
	m.mu.Unlock()
}

func (s *Server) jobEvents(w http.ResponseWriter, r *http.Request) {
	flusher, ok := w.(http.Flusher)
	if !ok {
		problem(w, http.StatusInternalServerError, "streaming is unavailable")
		return
	}
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache, no-transform")
	w.Header().Set("Connection", "keep-alive")
	w.Header().Set("X-Accel-Buffering", "no")
	updates, unsubscribe := s.jobs.Subscribe()
	defer unsubscribe()
	keepalive := time.NewTicker(20 * time.Second)
	defer keepalive.Stop()
	progress := time.NewTicker(2 * time.Second)
	defer progress.Stop()
	send := func(event string, data any) error {
		raw, err := json.Marshal(data)
		if err != nil {
			return err
		}
		if _, err = fmt.Fprintf(w, "event: %s\ndata: %s\n\n", event, raw); err == nil {
			flusher.Flush()
		}
		return err
	}
	if err := send("jobs", map[string]any{"changed": true}); err != nil {
		return
	}
	for {
		select {
		case <-r.Context().Done():
			return
		case _, open := <-updates:
			if !open {
				return
			}
			if send("jobs", map[string]any{"changed": true}) != nil {
				return
			}
		case <-progress.C:
			if send("jobs", map[string]any{"changed": true}) != nil {
				return
			}
		case <-keepalive.C:
			if _, err := fmt.Fprint(w, ": keepalive\n\n"); err != nil {
				return
			}
			flusher.Flush()
		}
	}
}

func waitForJobSlot(ctx context.Context, slots chan struct{}) error {
	select {
	case slots <- struct{}{}:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}
