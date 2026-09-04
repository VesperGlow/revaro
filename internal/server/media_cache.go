package server

import (
	"io/fs"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/VesperGlow/revaro/internal/cache"
)

type mediaCacheEntry struct {
	kind, id, dir string
	lastAccess    time.Time
	done          bool
	size          int64
}

func mediaCacheEntryKey(kind, id string) string { return kind + "\x00" + id }

func (s *Server) setMediaCacheSize(kind, id string, size int64) {
	s.mediaCacheMu.Lock()
	defer s.mediaCacheMu.Unlock()
	if !s.mediaCacheSessionPresentLocked(kind, id) {
		return
	}
	if s.mediaCacheSizes == nil {
		s.mediaCacheSizes = make(map[string]int64)
	}
	s.mediaCacheSizes[mediaCacheEntryKey(kind, id)] = max(size, int64(0))
}

func (s *Server) forgetMediaCacheSize(kind, id string) {
	s.mediaCacheMu.Lock()
	delete(s.mediaCacheSizes, mediaCacheEntryKey(kind, id))
	s.mediaCacheMu.Unlock()
}

func (s *Server) refreshMediaCacheEntry(kind, id, dir string) {
	size := directoryBytes(dir)
	s.setMediaCacheSize(kind, id, size)
}

// mediaCacheSessionPresentLocked must be called with mediaCacheMu held. The
// same lock order is used by session removal so a worker finishing while its
// workspace is being removed cannot reinsert a stale size snapshot.
func (s *Server) mediaCacheSessionPresentLocked(kind, id string) bool {
	switch kind {
	case "audio":
		s.audioHLSMu.RLock()
		_, ok := s.audioHLSSessions[id]
		s.audioHLSMu.RUnlock()
		return ok
	case "video":
		s.videoHLSMu.RLock()
		_, ok := s.videoHLSSessions[id]
		s.videoHLSMu.RUnlock()
		return ok
	default:
		return false
	}
}

func directoryBytes(path string) int64 {
	var total int64
	_ = filepath.WalkDir(path, func(_ string, entry fs.DirEntry, err error) error {
		if err != nil || entry.IsDir() {
			return nil
		}
		if info, statErr := entry.Info(); statErr == nil {
			total += info.Size()
		}
		return nil
	})
	return total
}

func (s *Server) mediaCacheStats() cache.ExternalStats {
	s.mediaCacheMu.Lock()
	defer s.mediaCacheMu.Unlock()
	var bytes int64
	entries := len(s.mediaCacheSizes)
	for _, size := range s.mediaCacheSizes {
		bytes += size
	}
	return cache.ExternalStats{DiskBytes: bytes, DiskEntries: entries}
}

// refreshMediaCacheUsage periodically reconciles external workspace sizes.
// The normal status path reads the resulting in-memory snapshot and never
// walks HLS directories.
func (s *Server) refreshMediaCacheUsage() {
	type workspace struct {
		kind, id, dir string
	}
	workspaces := make([]workspace, 0)
	s.audioHLSMu.RLock()
	for id, session := range s.audioHLSSessions {
		workspaces = append(workspaces, workspace{kind: "audio", id: id, dir: session.Dir})
	}
	s.audioHLSMu.RUnlock()
	s.videoHLSMu.RLock()
	for id, session := range s.videoHLSSessions {
		workspaces = append(workspaces, workspace{kind: "video", id: id, dir: session.Dir})
	}
	s.videoHLSMu.RUnlock()
	for _, item := range workspaces {
		s.refreshMediaCacheEntry(item.kind, item.id, item.dir)
	}
	s.mediaCacheMu.Lock()
	for key := range s.mediaCacheSizes {
		kind, id, found := strings.Cut(key, "\x00")
		if !found || !s.mediaCacheSessionPresentLocked(kind, id) {
			delete(s.mediaCacheSizes, key)
		}
	}
	s.mediaCacheMu.Unlock()
}

// pruneMediaCache applies the global disk budget to completed audio and video
// HLS fallback sessions. Active FFmpeg workspaces are never removed by this
// budget pass; their bytes remain part of the external provider usage, so the
// managed cache gives them the remaining budget.
func (s *Server) pruneMediaCache(budget cache.ExternalBudget) {
	limit := budget.DiskBytes
	if limit < 0 {
		return
	}
	entries := make([]mediaCacheEntry, 0)
	s.audioHLSMu.RLock()
	for id, session := range s.audioHLSSessions {
		lastAccess, done, _ := session.snapshot()
		entries = append(entries, mediaCacheEntry{kind: "audio", id: id, dir: session.Dir, lastAccess: lastAccess, done: done})
	}
	s.audioHLSMu.RUnlock()
	s.videoHLSMu.RLock()
	for id, session := range s.videoHLSSessions {
		lastAccess, done, _, _, _, _, _ := session.snapshot()
		entries = append(entries, mediaCacheEntry{kind: "video", id: id, dir: session.Dir, lastAccess: lastAccess, done: done})
	}
	s.videoHLSMu.RUnlock()
	var total int64
	for i := range entries {
		entries[i].size = directoryBytes(entries[i].dir)
		s.refreshMediaCacheEntry(entries[i].kind, entries[i].id, entries[i].dir)
		total += entries[i].size
	}
	if total <= limit {
		return
	}
	sort.Slice(entries, func(i, j int) bool { return entries[i].lastAccess.Before(entries[j].lastAccess) })
	for _, entry := range entries {
		if !entry.done || (limit > 0 && total <= limit) {
			continue
		}
		if entry.kind == "audio" {
			s.removeAudioHLSSession(entry.id)
		} else {
			s.removeVideoHLSSession(entry.id)
		}
		total -= entry.size
		s.log.Info("media cache entry evicted", "kind", entry.kind, "session", entry.id, "bytes", entry.size, "cache_bytes", max(total, int64(0)), "capacity", limit)
	}
}
