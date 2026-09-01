package server

import (
	"container/list"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"
)

type CacheStats struct {
	MemoryBytes, DiskBytes     int64
	MemoryEntries, DiskEntries int
}

type cacheItem struct {
	key               string
	data              []byte
	expires, accessed time.Time
	element           *list.Element
}
type cacheFlight struct {
	ready chan struct{}
	data  []byte
	err   error
}

// CacheManager provides a small byte-bounded memory LRU and a larger disk
// tier. Large media workspaces remain disk-only and are tracked separately by
// their session registries.
type CacheManager struct {
	mu                                  sync.Mutex
	memory                              map[string]*cacheItem
	lru                                 *list.List
	flights                             map[string]*cacheFlight
	statsProviders                      map[string]func() (int64, int)
	pruners                             map[string]func()
	memoryBytes, memoryLimit, diskLimit int64
	diskDir                             string
	ctx                                 context.Context
	cancel                              context.CancelFunc
	wg                                  sync.WaitGroup
	closed                              bool
}

func newCacheManager(dir string, memoryLimit, diskLimit int64) *CacheManager {
	_ = os.MkdirAll(dir, 0o700)
	ctx, cancel := context.WithCancel(context.Background())
	return &CacheManager{memory: map[string]*cacheItem{}, lru: list.New(), flights: map[string]*cacheFlight{}, statsProviders: map[string]func() (int64, int){}, pruners: map[string]func(){}, memoryLimit: memoryLimit, diskLimit: diskLimit, diskDir: dir, ctx: ctx, cancel: cancel}
}

func (c *CacheManager) RegisterPruner(name string, pruner func()) {
	c.mu.Lock()
	c.pruners[name] = pruner
	c.mu.Unlock()
}

func (c *CacheManager) RegisterStats(name string, provider func() (int64, int)) {
	c.mu.Lock()
	c.statsProviders[name] = provider
	c.mu.Unlock()
}

func (c *CacheManager) Get(key string) ([]byte, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	item := c.memory[key]
	if item == nil || (!item.expires.IsZero() && time.Now().After(item.expires)) {
		if item != nil {
			c.deleteMemory(item)
		}
		return nil, false
	}
	item.accessed = time.Now()
	c.lru.MoveToFront(item.element)
	return append([]byte(nil), item.data...), true
}

func (c *CacheManager) Put(key string, data []byte, ttl time.Duration) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if old := c.memory[key]; old != nil {
		c.deleteMemory(old)
	}
	item := &cacheItem{key: key, data: append([]byte(nil), data...), accessed: time.Now()}
	if ttl > 0 {
		item.expires = time.Now().Add(ttl)
	}
	item.element = c.lru.PushFront(item)
	c.memory[key] = item
	c.memoryBytes += int64(len(item.data))
	c.pruneMemory()
}

func (c *CacheManager) Delete(key string) error {
	c.mu.Lock()
	if item := c.memory[key]; item != nil {
		c.deleteMemory(item)
	}
	c.mu.Unlock()
	_ = os.Remove(c.diskPath(key) + ".meta")
	err := os.Remove(c.diskPath(key))
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	return err
}

func (c *CacheManager) Invalidate(prefix string) {
	c.mu.Lock()
	for key, item := range c.memory {
		if strings.HasPrefix(key, prefix) {
			c.deleteMemory(item)
		}
	}
	c.mu.Unlock()
	entries, _ := os.ReadDir(c.diskDir)
	for _, entry := range entries {
		if !strings.HasSuffix(entry.Name(), ".meta") {
			continue
		}
		metaPath := filepath.Join(c.diskDir, entry.Name())
		meta, _ := os.ReadFile(metaPath)
		parts := strings.SplitN(string(meta), "\n", 2)
		if len(parts) == 2 && strings.HasPrefix(parts[0], prefix) {
			_ = os.Remove(strings.TrimSuffix(metaPath, ".meta"))
			_ = os.Remove(metaPath)
		}
	}
}

func (c *CacheManager) GetOrCreate(ctx context.Context, key string, ttl time.Duration, create func(context.Context) ([]byte, error)) ([]byte, error) {
	if data, ok := c.Get(key); ok {
		return data, nil
	}
	c.mu.Lock()
	if c.closed {
		c.mu.Unlock()
		return nil, context.Canceled
	}
	flight := c.flights[key]
	if flight == nil {
		flight = &cacheFlight{ready: make(chan struct{})}
		c.flights[key] = flight
		c.wg.Add(1)
		go func() {
			defer c.wg.Done()
			workCtx, cancel := context.WithTimeout(c.ctx, 10*time.Minute)
			defer cancel()
			data, err := create(workCtx)
			if err == nil {
				c.Put(key, data, ttl)
			}
			c.mu.Lock()
			flight.data, flight.err = data, err
			delete(c.flights, key)
			close(flight.ready)
			c.mu.Unlock()
		}()
	}
	c.mu.Unlock()
	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	case <-flight.ready:
		return append([]byte(nil), flight.data...), flight.err
	}
}

func (c *CacheManager) PutDisk(key string, data []byte, ttl time.Duration) error {
	path := c.diskPath(key)
	tmp, err := os.CreateTemp(c.diskDir, "cache-")
	if err != nil {
		return err
	}
	name := tmp.Name()
	defer os.Remove(name)
	if _, err = tmp.Write(data); err == nil {
		err = tmp.Sync()
	}
	if closeErr := tmp.Close(); err == nil {
		err = closeErr
	}
	if err != nil {
		return err
	}
	if err = os.Rename(name, path); err != nil {
		return err
	}
	expires := int64(0)
	if ttl > 0 {
		expires = time.Now().Add(ttl).UnixNano()
	}
	if err = os.WriteFile(path+".meta", []byte(key+"\n"+strconv.FormatInt(expires, 10)), 0o600); err != nil {
		_ = os.Remove(path)
		return err
	}
	c.Prune()
	return nil
}

func (c *CacheManager) GetDisk(key string) ([]byte, bool) {
	path := c.diskPath(key)
	meta, err := os.ReadFile(path + ".meta")
	if err != nil {
		return nil, false
	}
	parts := strings.SplitN(string(meta), "\n", 2)
	if len(parts) != 2 || parts[0] != key {
		return nil, false
	}
	expires, _ := strconv.ParseInt(parts[1], 10, 64)
	if expires > 0 && time.Now().UnixNano() > expires {
		_ = c.Delete(key)
		return nil, false
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, false
	}
	_ = os.Chtimes(path, time.Now(), time.Now())
	return data, true
}
func (c *CacheManager) Prune() {
	entries, _ := os.ReadDir(c.diskDir)
	type diskItem struct {
		path string
		size int64
		when time.Time
	}
	items := []diskItem{}
	var total int64
	for _, e := range entries {
		if i, err := e.Info(); err == nil && !i.IsDir() && !strings.HasSuffix(e.Name(), ".meta") {
			items = append(items, diskItem{filepath.Join(c.diskDir, e.Name()), i.Size(), i.ModTime()})
			total += i.Size()
		}
	}
	for total > c.diskLimit && len(items) > 0 {
		old := 0
		for i := 1; i < len(items); i++ {
			if items[i].when.Before(items[old].when) {
				old = i
			}
		}
		_ = os.Remove(items[old].path)
		_ = os.Remove(items[old].path + ".meta")
		total -= items[old].size
		items = append(items[:old], items[old+1:]...)
	}
	c.mu.Lock()
	pruners := make([]func(), 0, len(c.pruners))
	for _, pruner := range c.pruners {
		pruners = append(pruners, pruner)
	}
	c.mu.Unlock()
	for _, pruner := range pruners {
		pruner()
	}
}
func (c *CacheManager) Stats() CacheStats {
	c.mu.Lock()
	out := CacheStats{MemoryBytes: c.memoryBytes, MemoryEntries: len(c.memory)}
	providers := make([]func() (int64, int), 0, len(c.statsProviders))
	for _, provider := range c.statsProviders {
		providers = append(providers, provider)
	}
	c.mu.Unlock()
	entries, _ := os.ReadDir(c.diskDir)
	for _, e := range entries {
		if i, err := e.Info(); err == nil && !i.IsDir() && !strings.HasSuffix(e.Name(), ".meta") {
			out.DiskEntries++
			out.DiskBytes += i.Size()
		}
	}
	for _, provider := range providers {
		bytes, entries := provider()
		out.DiskBytes += bytes
		out.DiskEntries += entries
	}
	return out
}
func (c *CacheManager) Close() {
	c.mu.Lock()
	if !c.closed {
		c.closed = true
		c.cancel()
	}
	c.mu.Unlock()
	c.wg.Wait()
}
func (c *CacheManager) deleteMemory(item *cacheItem) {
	delete(c.memory, item.key)
	c.lru.Remove(item.element)
	c.memoryBytes -= int64(len(item.data))
}
func (c *CacheManager) pruneMemory() {
	for c.memoryBytes > c.memoryLimit && c.lru.Len() > 0 {
		c.deleteMemory(c.lru.Back().Value.(*cacheItem))
	}
}
func diskPrefix(key string) string {
	sum := sha256.Sum256([]byte(key))
	return hex.EncodeToString(sum[:8])
}
func (c *CacheManager) diskPath(key string) string {
	return filepath.Join(c.diskDir, diskPrefix(key)+".cache")
}
