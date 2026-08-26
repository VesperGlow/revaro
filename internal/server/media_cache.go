package server

import (
	"io/fs"
	"path/filepath"
	"sort"
	"time"
)

type mediaCacheEntry struct {
	kind, id, dir string
	lastAccess    time.Time
	done          bool
	size          int64
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

// pruneMediaCache applies one byte cap across completed audio and video HLS
// fallback sessions. Active FFmpeg workspaces are bounded separately by slot
// counts and the three-minute output duration, so they are never torn down
// merely because a cleanup tick observes them mid-write.
func (s *Server) pruneMediaCache() {
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
		total += entries[i].size
	}
	limit := s.cfg.MediaCacheCapacity
	if total <= limit && limit > 0 {
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
