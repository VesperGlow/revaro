package storage

import (
	"container/list"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"golang.org/x/sys/unix"
)

type diskCacheEntry struct {
	id         string
	path       string
	size       int64
	lastAccess time.Time
}

// diskBlockCache is a persistent LRU for immutable content-addressed blocks.
// Files are laid out with the same two-character fanout as S3. Modtime is the
// persisted access timestamp used to rebuild LRU order after a restart.
type diskBlockCache struct {
	mu       sync.Mutex
	root     string
	capacity int64
	minFree  int64
	maxBlock int64
	size     int64
	items    map[string]*list.Element
	order    *list.List
}

func newDiskBlockCache(root string, capacity, minFree, maxBlock int64) (*diskBlockCache, error) {
	if capacity <= 0 {
		return nil, nil
	}
	if err := os.MkdirAll(root, 0o700); err != nil {
		return nil, err
	}
	c := &diskBlockCache{
		root: root, capacity: capacity, minFree: minFree, maxBlock: maxBlock,
		items: make(map[string]*list.Element), order: list.New(),
	}
	if err := c.scan(); err != nil {
		return nil, err
	}
	c.mu.Lock()
	c.ensureSpaceLocked(0)
	c.mu.Unlock()
	return c, nil
}

func (c *diskBlockCache) scan() error {
	var found []*diskCacheEntry
	err := filepath.WalkDir(c.root, func(path string, entry fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		if entry.IsDir() {
			return nil
		}
		rel, err := filepath.Rel(c.root, path)
		if err != nil {
			return err
		}
		parts := splitCachePath(rel)
		if len(parts) != 2 || len(parts[0]) != 2 || !ValidBlockID(parts[0]+parts[1]) {
			if filepath.Base(path)[0] == '.' {
				_ = os.Remove(path)
			}
			return nil
		}
		info, err := entry.Info()
		if err != nil {
			return err
		}
		if !info.Mode().IsRegular() || info.Size() <= 0 || info.Size() > c.maxBlock {
			_ = os.Remove(path)
			return nil
		}
		found = append(found, &diskCacheEntry{id: parts[0] + parts[1], path: path, size: info.Size(), lastAccess: info.ModTime()})
		return nil
	})
	if err != nil {
		return err
	}
	sort.Slice(found, func(a, b int) bool { return found[a].lastAccess.Before(found[b].lastAccess) })
	for _, entry := range found {
		c.items[entry.id] = c.order.PushFront(entry)
		c.size += entry.size
	}
	return nil
}

func splitCachePath(rel string) []string {
	return strings.Split(filepath.Clean(rel), string(os.PathSeparator))
}

func (c *diskBlockCache) path(id string) string {
	return filepath.Join(c.root, id[:2], id[2:])
}

func (c *diskBlockCache) get(id string, expectedSize int64) ([]byte, bool) {
	if c == nil || !ValidBlockID(id) {
		return nil, false
	}
	c.mu.Lock()
	element := c.items[id]
	if element == nil {
		c.mu.Unlock()
		return nil, false
	}
	path := element.Value.(*diskCacheEntry).path
	c.mu.Unlock()

	data, err := os.ReadFile(path)
	if err != nil || int64(len(data)) > c.maxBlock || (expectedSize >= 0 && int64(len(data)) != expectedSize) || hashBytes(data) != id {
		c.remove(id, path)
		return nil, false
	}
	now := time.Now()
	c.mu.Lock()
	if current := c.items[id]; current != nil && current.Value.(*diskCacheEntry).path == path {
		current.Value.(*diskCacheEntry).lastAccess = now
		c.order.MoveToFront(current)
	}
	c.mu.Unlock()
	_ = os.Chtimes(path, now, now)
	return data, true
}

func (c *diskBlockCache) put(id string, data []byte) error {
	if c == nil || !ValidBlockID(id) || len(data) == 0 || int64(len(data)) > c.maxBlock || int64(len(data)) > c.capacity {
		return nil
	}
	if hashBytes(data) != id {
		return ErrBlockHashMismatch
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if element := c.items[id]; element != nil {
		now := time.Now()
		element.Value.(*diskCacheEntry).lastAccess = now
		c.order.MoveToFront(element)
		_ = os.Chtimes(element.Value.(*diskCacheEntry).path, now, now)
		return nil
	}
	if !c.ensureSpaceLocked(int64(len(data))) {
		return nil
	}
	dir := filepath.Dir(c.path(id))
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	temp, err := os.CreateTemp(dir, ".block-*")
	if err != nil {
		return err
	}
	tempPath := temp.Name()
	keep := false
	defer func() {
		_ = temp.Close()
		if !keep {
			_ = os.Remove(tempPath)
		}
	}()
	if _, err := temp.Write(data); err != nil {
		return err
	}
	if err := temp.Sync(); err != nil {
		return err
	}
	if err := temp.Close(); err != nil {
		return err
	}
	target := c.path(id)
	if err := os.Rename(tempPath, target); err != nil {
		return err
	}
	keep = true
	now := time.Now()
	entry := &diskCacheEntry{id: id, path: target, size: int64(len(data)), lastAccess: now}
	c.items[id] = c.order.PushFront(entry)
	c.size += entry.size
	return nil
}

func (c *diskBlockCache) ensureSpaceLocked(incoming int64) bool {
	for c.size+incoming > c.capacity {
		if !c.evictOneLocked() {
			return false
		}
	}
	for {
		free, err := cacheFreeBytes(c.root)
		if err != nil {
			return false
		}
		if incoming <= free && free-incoming >= c.minFree {
			return true
		}
		if !c.evictOneLocked() {
			return false
		}
	}
}

func (c *diskBlockCache) evictOneLocked() bool {
	element := c.order.Back()
	if element == nil {
		return false
	}
	entry := element.Value.(*diskCacheEntry)
	if err := os.Remove(entry.path); err != nil && !errors.Is(err, os.ErrNotExist) {
		return false
	}
	delete(c.items, entry.id)
	c.order.Remove(element)
	c.size -= entry.size
	return true
}

func (c *diskBlockCache) remove(id, path string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	element := c.items[id]
	if element == nil || element.Value.(*diskCacheEntry).path != path {
		return
	}
	entry := element.Value.(*diskCacheEntry)
	_ = os.Remove(entry.path)
	delete(c.items, id)
	c.order.Remove(element)
	c.size -= entry.size
}

func cacheFreeBytes(path string) (int64, error) {
	var stat unix.Statfs_t
	if err := unix.Statfs(path, &stat); err != nil {
		return 0, err
	}
	return int64(stat.Bavail) * int64(stat.Bsize), nil
}
