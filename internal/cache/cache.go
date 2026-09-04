// Package cache 实现 Revaro 的统一缓存管理器（Global CacheManager）。
//
// 之前分散在 server.CacheManager（字幕内存缓存 + 扁平磁盘缓存）、
// reader.DefaultCache（解析书 LRU）、flow 对象缓存与 media 会话缓存里的
// 生命周期、容量、LRU、singleflight、统计与失效策略统一收敛到这里；不同
// cache class 可以声明不同的 tier（memory L1 / local-disk L2）与策略。
//
// 统一 key namespace（class 即命名空间）：
//
//	reader/flow-manifest  flow manifest（内存工作缓存，内容寻址 immutable）
//	reader/flow-chunk     flow chunk（内存 byte-LRU，内容寻址 immutable）
//	reader/source   书源 blob（内容寻址 immutable，避免冷启动回源 S3）
//	media/subtitle  字幕转换产物（真正临时，带 TTL）
//	media/hls       音视频 HLS 会话工作区（external：目录由会话自己管理，
//	                通过 RegisterExternal 纳入全局统计与容量回收）
//
// 容量策略：managed cache 的 memory/disk byte-LRU 与 external workspace
// 共享全局预算；class 可声明 priority（大者更晚被淘汰）与 soft quota
// （超出后该 class 的最旧条目优先被淘汰）。TTL 只用于真正临时的产物；
// 内容哈希寻址的 immutable 缓存依赖内容/版本键 + 容量 LRU。
//
// 所有读取路径：L1（memory，若 class 允许）→ L2（disk，若 class 允许）→
// 回源加载（singleflight 去重，成功后回填两级）。
package cache

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
	"sync/atomic"
	"time"
)

// Class 声明一个 cache class 的命名空间与策略。
type Class struct {
	Name string
	// Priority 越大越晚被全局淘汰（跨 class 竞争时的权重）。
	Priority int
	// SoftQuota 是该 class 的软字节配额；0 表示不单独限额。超出配额的
	// class 在淘汰时最优先被回收（按 LRU 顺序收敛回配额内）。
	SoftQuota int64
	// Memory/Disk 声明该 class 允许的 tier。
	Memory bool
	Disk   bool
}

// ClassStats 是单个 class 的累计指标与当前占用（快照值）。
type ClassStats struct {
	Hits, Misses, Loads, LoadErrors, Evictions int64
	MemoryBytes, DiskBytes                     int64
	MemoryEntries, DiskEntries                 int
}

// ExternalStats 是由其他组件自行管理的缓存快照。Memory* 与 Disk* 必须
// 表示实际 tier，不能把内存对象填入磁盘字段。没有命中语义的 provider
// 应保持 Hits/Misses 为零；CacheManager 不会替 provider 推断命中率。
type ExternalStats struct {
	Hits, Misses, Loads, LoadErrors, Evictions int64
	MemoryBytes, DiskBytes                     int64
	MemoryEntries, DiskEntries                 int
}

// ExternalBudget 是 external pruner 在一次 Prune 中应尽量收敛到的自身
// tier 上限。负数表示该 tier 不受本次全局预算约束。
type ExternalBudget struct {
	MemoryBytes int64
	DiskBytes   int64
}

// classCounters 是 class 的原子累计指标：Get/Load 的计数发生在不持有
// m.mu 的调用路径上，必须用原子操作避免并发计数竞争。
type classCounters struct {
	hits, misses, loads, loadErrors, evictions atomic.Int64
}

// Stats 是全局缓存指标快照。
type Stats struct {
	Classes                    map[string]ClassStats
	MemoryBytes, DiskBytes     int64
	MemoryEntries, DiskEntries int
}

type entry struct {
	class    Class
	key      string // class 内的逻辑键
	data     []byte
	expires  time.Time
	accessed time.Time
	size     int64
	element  *list.Element
}

type flight struct {
	ready chan struct{}
	data  []byte
	err   error
}

type diskItem struct {
	class    string
	key      string
	path     string
	size     int64
	mod      time.Time // 文件 mtime，仅作为重启后的 lastAccess fallback
	accessed time.Time // 进程内 LRU 时间，不持久化
	expires  time.Time
}

type usage struct {
	memoryBytes, diskBytes     int64
	memoryEntries, diskEntries int
}

type externalProvider struct {
	stats func() ExternalStats
	prune func(ExternalBudget)
}

// Manager 是统一缓存管理器。并发安全。
type Manager struct {
	mu            sync.Mutex
	classes       map[string]Class
	usage         map[string]*usage
	memory        map[string]*entry // 限定键 class\x00key → entry
	lru           *list.List
	flights       map[string]*flight
	stats         map[string]*classCounters
	disk          map[string]*diskItem // class\x00key → disk item
	diskDir       string
	memoryBytes   int64
	memoryEntries int
	diskBytes     int64
	diskEntries   int
	memoryLimit   int64
	diskLimit     int64
	diskMu        sync.Mutex // serializes disk I/O and disk index reconciliation
	pruneMu       sync.Mutex
	extMu         sync.Mutex
	ext           map[string]externalProvider
	ctx           context.Context
	cancel        context.CancelFunc
	wg            sync.WaitGroup
	closed        bool
}

// New 创建管理器。diskDir 为 L2 根目录；memoryLimit/diskLimit 为全局字节
// 上限（0 = 不限）。
func New(diskDir string, memoryLimit, diskLimit int64) *Manager {
	_ = os.MkdirAll(diskDir, 0o700)
	ctx, cancel := context.WithCancel(context.Background())
	m := &Manager{
		classes:     map[string]Class{},
		usage:       map[string]*usage{},
		memory:      map[string]*entry{},
		lru:         list.New(),
		flights:     map[string]*flight{},
		stats:       map[string]*classCounters{},
		disk:        map[string]*diskItem{},
		diskDir:     diskDir,
		memoryLimit: memoryLimit,
		diskLimit:   diskLimit,
		ext:         map[string]externalProvider{},
		ctx:         ctx,
		cancel:      cancel,
	}
	// Cache files are rebuildable. A single startup scan reconstructs the
	// in-memory disk index and removes incomplete/expired entries.
	m.scanDisk()
	return m
}

// RegisterClass 注册 cache class（必须在任何使用之前完成，通常在服务启动
// 时统一注册）。使用未注册的 class 属于编程错误。
func (m *Manager) RegisterClass(c Class) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.classes[c.Name] = c
	if _, ok := m.usage[c.Name]; !ok {
		m.usage[c.Name] = &usage{}
	}
	if _, ok := m.stats[c.Name]; !ok {
		m.stats[c.Name] = &classCounters{}
	}
}

func (m *Manager) classOf(name string) Class {
	c, ok := m.classes[name]
	if !ok {
		panic("cache: unregistered class " + name)
	}
	return c
}

// classStats 返回 class 的原子计数器。stats map 只在 RegisterClass（启动
// 期、任何使用之前）写入，此后只读；未注册的 class 与 classOf 同为编程
// 错误。
func (m *Manager) classStats(name string) *classCounters {
	s, ok := m.stats[name]
	if !ok {
		panic("cache: unregistered class " + name)
	}
	return s
}

func (m *Manager) ensureUsageLocked(name string) *usage {
	used := m.usage[name]
	if used == nil {
		used = &usage{}
		m.usage[name] = used
	}
	return used
}

func qualKey(class, key string) string { return class + "\x00" + key }

// getMemory 读取 memory L1，不记统计（Load 的统计口径是「每次 Load 一次
// hit 或 miss」）。
func (m *Manager) getMemory(c Class, key string) ([]byte, bool) {
	if !c.Memory {
		return nil, false
	}
	item := m.memory[qualKey(c.Name, key)]
	if item == nil {
		return nil, false
	}
	if !item.expires.IsZero() && time.Now().After(item.expires) {
		m.removeMemory(item)
		return nil, false
	}
	item.accessed = time.Now()
	m.lru.MoveToFront(item.element)
	return append([]byte(nil), item.data...), true
}

// Get 读取 memory L1（class 未声明 Memory 时永远 miss）。
func (m *Manager) Get(class, key string) ([]byte, bool) {
	m.mu.Lock()
	data, ok := m.getMemory(m.classOf(class), key)
	m.mu.Unlock()
	if ok {
		m.classStats(class).hits.Add(1)
		return data, true
	}
	m.classStats(class).misses.Add(1)
	return nil, false
}

// Has 报告条目在任一 tier 中是否存在（未过期）。磁盘状态来自内存索引，
// 因此不会为一次状态查询读取 meta；Prune 会校准外部删除或写入残留。
func (m *Manager) Has(class, key string) bool {
	c := m.classOf(class)
	qk := qualKey(class, key)
	m.mu.Lock()
	now := time.Now()
	item := m.memory[qk]
	if item != nil && !item.expires.IsZero() && now.After(item.expires) {
		m.removeMemory(item)
		item = nil
	}
	if item != nil {
		m.mu.Unlock()
		return true
	}
	diskItem := m.disk[qk]
	if diskItem != nil && !diskItem.expires.IsZero() && now.After(diskItem.expires) {
		m.removeDiskStateLocked(qk, diskItem.path)
		diskItem = nil
	}
	valid := diskItem != nil
	m.mu.Unlock()
	return c.Disk && valid
}

// Put 直写缓存（允许的 tier 各写一份）。
func (m *Manager) Put(class, key string, data []byte, ttl time.Duration) {
	c := m.classOf(class)
	var expires time.Time
	if ttl > 0 {
		expires = time.Now().Add(ttl)
	}
	if c.Memory {
		m.putMemory(c, key, data, expires)
	}
	if c.Disk {
		if err := m.putDisk(c, key, data, expires); err != nil {
			return // 磁盘写入失败只降级，不影响内存层
		}
	}
}

// Load 走完整读取路径：L1 → L2 → 回源（singleflight 去重，成功回填）。
// 统计口径：每次 Load 记一次 hit（任一 tier 命中）或 miss（回源）。
func (m *Manager) Load(ctx context.Context, class, key string, ttl time.Duration, load func(context.Context) ([]byte, error)) ([]byte, error) {
	c := m.classOf(class)
	m.mu.Lock()
	data, ok := m.getMemory(c, key)
	m.mu.Unlock()
	if ok {
		m.classStats(class).hits.Add(1)
		return data, nil
	}
	if c.Disk {
		if data, expires, ok := m.getDisk(class, key); ok {
			m.classStats(class).hits.Add(1)
			if c.Memory {
				m.putMemory(c, key, data, expires)
			}
			return data, nil
		}
	}
	m.classStats(class).misses.Add(1)
	qk := qualKey(class, key)
	m.mu.Lock()
	if m.closed {
		m.mu.Unlock()
		return nil, context.Canceled
	}
	f := m.flights[qk]
	if f == nil {
		f = &flight{ready: make(chan struct{})}
		m.flights[qk] = f
		m.wg.Add(1)
		go func() {
			defer m.wg.Done()
			workCtx, cancel := context.WithTimeout(m.ctx, 10*time.Minute)
			defer cancel()
			data, err := load(workCtx)
			m.classStats(class).loads.Add(1)
			if err != nil {
				m.classStats(class).loadErrors.Add(1)
			} else {
				m.Put(class, key, data, ttl)
			}
			m.mu.Lock()
			f.data, f.err = data, err
			delete(m.flights, qk)
			close(f.ready)
			m.mu.Unlock()
		}()
	}
	m.mu.Unlock()
	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	case <-f.ready:
		return append([]byte(nil), f.data...), f.err
	}
}

// Delete 移除一个条目（所有 tier）。
func (m *Manager) Delete(class, key string) error {
	c := m.classOf(class)
	m.mu.Lock()
	if item := m.memory[qualKey(class, key)]; item != nil {
		m.removeMemory(item)
	}
	m.mu.Unlock()
	if !c.Disk {
		return nil
	}
	m.diskMu.Lock()
	defer m.diskMu.Unlock()
	path := m.diskPath(class, key)
	dataErr := os.Remove(path)
	metaErr := os.Remove(path + ".meta")
	if dataErr == nil || errors.Is(dataErr, os.ErrNotExist) {
		m.mu.Lock()
		m.removeDiskStateLocked(qualKey(class, key), path)
		m.mu.Unlock()
	}
	if dataErr != nil && !errors.Is(dataErr, os.ErrNotExist) {
		return dataErr
	}
	if metaErr != nil && !errors.Is(metaErr, os.ErrNotExist) {
		return metaErr
	}
	return nil
}

// Invalidate 按逻辑键前缀跨 class 失效（memory + disk）。
func (m *Manager) Invalidate(prefix string) {
	m.mu.Lock()
	keys := make([][2]string, 0)
	for qk, item := range m.memory {
		class, key, _ := strings.Cut(qk, "\x00")
		if strings.HasPrefix(key, prefix) || strings.HasPrefix(class+"\x00"+key, prefix) {
			m.removeMemory(item)
		}
	}
	for qk := range m.disk {
		class, key, _ := strings.Cut(qk, "\x00")
		if _, registered := m.classes[class]; !registered {
			continue // legacy/unknown entries are handled by the next Prune.
		}
		if strings.HasPrefix(key, prefix) || strings.HasPrefix(class+"\x00"+key, prefix) {
			keys = append(keys, [2]string{class, key})
		}
	}
	m.mu.Unlock()
	for _, key := range keys {
		_ = m.Delete(key[0], key[1])
	}
}

// RegisterExternal 把不由本管理器直接存储的缓存（如 HLS 会话工作区、
// 解析书内存 LRU）纳入全局统计与容量回收。provider 不提供命中信息时
// 应返回零值 Hits/Misses；pruner 的预算是该 provider 自身的 tier 上限。
func (m *Manager) RegisterExternal(name string, stats func() ExternalStats, pruner func(ExternalBudget)) {
	m.extMu.Lock()
	m.ext[name] = externalProvider{stats: stats, prune: pruner}
	m.extMu.Unlock()
}

type externalSnapshot struct {
	name     string
	provider externalProvider
	stats    ExternalStats
}

func normalizeExternalStats(stats ExternalStats) ExternalStats {
	if stats.MemoryBytes < 0 {
		stats.MemoryBytes = 0
	}
	if stats.DiskBytes < 0 {
		stats.DiskBytes = 0
	}
	if stats.MemoryEntries < 0 {
		stats.MemoryEntries = 0
	}
	if stats.DiskEntries < 0 {
		stats.DiskEntries = 0
	}
	return stats
}

func (m *Manager) externalSnapshots() []externalSnapshot {
	m.extMu.Lock()
	registered := make(map[string]externalProvider, len(m.ext))
	for name, provider := range m.ext {
		registered[name] = provider
	}
	m.extMu.Unlock()
	providers := make([]externalSnapshot, 0, len(registered))
	for name, provider := range registered {
		stats := ExternalStats{}
		if provider.stats != nil {
			stats = normalizeExternalStats(provider.stats())
		}
		providers = append(providers, externalSnapshot{name: name, provider: provider, stats: stats})
	}
	return providers
}

func externalTotals(providers []externalSnapshot) (memory, disk int64) {
	for _, provider := range providers {
		memory += provider.stats.MemoryBytes
		disk += provider.stats.DiskBytes
	}
	return memory, disk
}

func (m *Manager) classSnapshot() map[string]Class {
	m.mu.Lock()
	defer m.mu.Unlock()
	classes := make(map[string]Class, len(m.classes))
	for name, class := range m.classes {
		classes[name] = class
	}
	return classes
}

func (m *Manager) diskItemsSnapshotLocked() []diskItem {
	m.mu.Lock()
	defer m.mu.Unlock()
	items := make([]diskItem, 0, len(m.disk))
	for _, item := range m.disk {
		items = append(items, *item)
	}
	return items
}

// Prune 校准 managed disk 状态，先处理各 class 软配额，再让 external
// provider 回收可安全删除的内容，最后让 managed cache 为 external usage
// 留出预算。普通 Stats 不调用本方法，也不扫描磁盘。
func (m *Manager) Prune() {
	m.pruneMu.Lock()
	defer m.pruneMu.Unlock()

	m.diskMu.Lock()
	items := m.scanDiskLocked()
	total := diskItemsBytes(items)
	perClass := diskItemsPerClass(items)
	classes := m.classSnapshot()
	items = m.evictDiskLocked(items, func(it diskItem) bool {
		class, ok := classes[it.class]
		return ok && class.SoftQuota > 0 && perClass[it.class] > class.SoftQuota
	}, &total, perClass, classes)
	m.diskMu.Unlock()

	providers := m.externalSnapshots()
	m.pruneExternal(providers)
	providers = m.externalSnapshots()
	_, externalDisk := externalTotals(providers)
	if m.diskLimit > 0 {
		target := m.diskLimit - externalDisk
		if target < 0 {
			target = 0
		}
		m.diskMu.Lock()
		items = m.diskItemsSnapshotLocked()
		total = diskItemsBytes(items)
		perClass = diskItemsPerClass(items)
		items = m.evictDiskLocked(items, func(diskItem) bool { return total > target }, &total, perClass, classes)
		m.diskMu.Unlock()
	}

	externalMemory, _ := externalTotals(providers)
	if m.memoryLimit > 0 {
		target := m.memoryLimit - externalMemory
		if target < 0 {
			target = 0
		}
		m.pruneMemoryTo(target)
	}
}

func (m *Manager) pruneExternal(providers []externalSnapshot) {
	if m.memoryLimit <= 0 && m.diskLimit <= 0 {
		return
	}
	managedMemory := m.memoryUsageSnapshot()
	m.mu.Lock()
	managedDisk := m.diskBytes
	m.mu.Unlock()
	totalMemory, totalDisk := externalTotals(providers)
	for _, provider := range providers {
		if provider.provider.prune == nil {
			continue
		}
		budget := ExternalBudget{MemoryBytes: -1, DiskBytes: -1}
		if m.memoryLimit > 0 {
			budget.MemoryBytes = m.memoryLimit - managedMemory - (totalMemory - provider.stats.MemoryBytes)
			if budget.MemoryBytes < 0 {
				budget.MemoryBytes = 0
			}
		}
		if m.diskLimit > 0 {
			budget.DiskBytes = m.diskLimit - managedDisk - (totalDisk - provider.stats.DiskBytes)
			if budget.DiskBytes < 0 {
				budget.DiskBytes = 0
			}
		}
		provider.provider.prune(budget)
	}
}

func diskItemsBytes(items []diskItem) (total int64) {
	for _, item := range items {
		total += item.size
	}
	return total
}

func diskItemsPerClass(items []diskItem) map[string]int64 {
	perClass := make(map[string]int64)
	for _, item := range items {
		perClass[item.class] += item.size
	}
	return perClass
}

// evictDisk 按 predicate 圈定候选、priority 大者与进程内 lastAccess 保留，
// 逐个删除直到 predicate 为假。调用者不应持有 diskMu。
func (m *Manager) evictDisk(items []diskItem, need func(diskItem) bool, total *int64, perClass map[string]int64) []diskItem {
	m.diskMu.Lock()
	defer m.diskMu.Unlock()
	return m.evictDiskLocked(items, need, total, perClass, m.classSnapshot())
}

func (m *Manager) evictDiskLocked(items []diskItem, need func(diskItem) bool, total *int64, perClass map[string]int64, classes map[string]Class) []diskItem {
	for {
		var candidates []diskItem
		for _, it := range items {
			if need(it) {
				candidates = append(candidates, it)
			}
		}
		if len(candidates) == 0 {
			return items
		}
		victim := candidates[0]
		victimClass := classes[victim.class]
		for _, it := range candidates[1:] {
			class := classes[it.class]
			if class.Priority < victimClass.Priority || (class.Priority == victimClass.Priority && diskAccessBefore(it, victim)) {
				victim, victimClass = it, class
			}
		}
		if !m.removeDiskFilesLocked(victim) {
			return items
		}
		*total -= victim.size
		perClass[victim.class] -= victim.size
		for i, it := range items {
			if it.path == victim.path {
				items = append(items[:i], items[i+1:]...)
				break
			}
		}
	}
}

func diskAccessBefore(a, b diskItem) bool {
	aAccess, bAccess := a.accessed, b.accessed
	if aAccess.IsZero() {
		aAccess = a.mod
	}
	if bAccess.IsZero() {
		bAccess = b.mod
	}
	return aAccess.Before(bAccess)
}

func (m *Manager) scanDisk() []diskItem {
	m.diskMu.Lock()
	defer m.diskMu.Unlock()
	return m.scanDiskLocked()
}

func (m *Manager) scanDiskLocked() []diskItem {
	entries, err := os.ReadDir(m.diskDir)
	if err != nil {
		return m.diskItemsSnapshotLocked()
	}
	items := make([]diskItem, 0, len(entries))
	old := make(map[string]diskItem)
	m.mu.Lock()
	for qk, item := range m.disk {
		old[qk] = *item
	}
	m.mu.Unlock()
	now := time.Now()
	for _, e := range entries {
		if strings.HasSuffix(e.Name(), ".meta") {
			continue
		}
		info, err := e.Info()
		if err != nil || info.IsDir() {
			continue
		}
		path := filepath.Join(m.diskDir, e.Name())
		meta, metaErr := os.ReadFile(path + ".meta")
		parts := strings.SplitN(string(meta), "\n", 2)
		if metaErr != nil || len(parts) != 2 {
			// 无 meta 的孤儿文件（写入中断残留）：直接清理。
			_ = os.Remove(path)
			_ = os.Remove(path + ".meta")
			continue
		}
		class, key, found := strings.Cut(parts[0], "\x00")
		if !found || class == "" {
			_ = os.Remove(path)
			_ = os.Remove(path + ".meta")
			continue
		}
		expiresNano, parseErr := strconv.ParseInt(strings.TrimSpace(parts[1]), 10, 64)
		if parseErr != nil {
			expiresNano = 0
		}
		expires := time.Time{}
		if expiresNano > 0 {
			expires = time.Unix(0, expiresNano)
			if now.After(expires) {
				_ = os.Remove(path)
				_ = os.Remove(path + ".meta")
				continue
			}
		}
		qk := qualKey(class, key)
		accessed := info.ModTime()
		if previous, ok := old[qk]; ok && previous.path == path {
			accessed = previous.accessed
			if accessed.IsZero() {
				accessed = previous.mod
			}
		}
		items = append(items, diskItem{class: class, key: key, path: path, size: info.Size(), mod: info.ModTime(), accessed: accessed, expires: expires})
	}
	newDisk := make(map[string]*diskItem, len(items))
	var total int64
	for i := range items {
		item := &items[i]
		newDisk[qualKey(item.class, item.key)] = item
		total += item.size
	}
	m.mu.Lock()
	m.disk = newDisk
	m.diskBytes = total
	m.diskEntries = len(newDisk)
	for _, used := range m.usage {
		used.diskBytes = 0
		used.diskEntries = 0
	}
	for _, item := range newDisk {
		used := m.ensureUsageLocked(item.class)
		used.diskBytes += item.size
		used.diskEntries++
	}
	m.mu.Unlock()
	return items
}

// Stats 汇总各 class 指标与占用（含 external provider）。这是锁保护的
// 内存快照；不会遍历 cache 目录，也不会读取 .meta。
func (m *Manager) Stats() Stats {
	m.mu.Lock()
	out := Stats{Classes: map[string]ClassStats{}, MemoryBytes: m.memoryBytes, DiskBytes: m.diskBytes, MemoryEntries: m.memoryEntries, DiskEntries: m.diskEntries}
	for name := range m.classes {
		used := m.ensureUsageLocked(name)
		out.Classes[name] = ClassStats{MemoryBytes: used.memoryBytes, MemoryEntries: used.memoryEntries, DiskBytes: used.diskBytes, DiskEntries: used.diskEntries}
	}
	for name, counters := range m.stats {
		cs := out.Classes[name]
		cs.Hits, cs.Misses, cs.Loads, cs.LoadErrors, cs.Evictions = counters.hits.Load(), counters.misses.Load(), counters.loads.Load(), counters.loadErrors.Load(), counters.evictions.Load()
		out.Classes[name] = cs
	}
	m.mu.Unlock()
	for _, provider := range m.externalSnapshots() {
		cs := out.Classes[provider.name]
		cs.Hits += provider.stats.Hits
		cs.Misses += provider.stats.Misses
		cs.Loads += provider.stats.Loads
		cs.LoadErrors += provider.stats.LoadErrors
		cs.Evictions += provider.stats.Evictions
		cs.MemoryBytes += provider.stats.MemoryBytes
		cs.MemoryEntries += provider.stats.MemoryEntries
		cs.DiskBytes += provider.stats.DiskBytes
		cs.DiskEntries += provider.stats.DiskEntries
		out.MemoryBytes += provider.stats.MemoryBytes
		out.MemoryEntries += provider.stats.MemoryEntries
		out.DiskBytes += provider.stats.DiskBytes
		out.DiskEntries += provider.stats.DiskEntries
		out.Classes[provider.name] = cs
	}
	return out
}

// Close 停止管理器并等待在途加载结束。
func (m *Manager) Close() {
	m.mu.Lock()
	if !m.closed {
		m.closed = true
		m.cancel()
	}
	m.mu.Unlock()
	m.wg.Wait()
}

// ---- memory L1 ----

func (m *Manager) putMemory(c Class, key string, data []byte, expires time.Time) {
	m.mu.Lock()
	defer m.mu.Unlock()
	qk := qualKey(c.Name, key)
	if old := m.memory[qk]; old != nil {
		m.removeMemory(old)
	}
	item := &entry{class: c, key: key, data: append([]byte(nil), data...), expires: expires, accessed: time.Now(), size: int64(len(data))}
	item.element = m.lru.PushFront(item)
	m.memory[qk] = item
	m.memoryBytes += item.size
	m.memoryEntries++
	used := m.ensureUsageLocked(c.Name)
	used.memoryBytes += item.size
	used.memoryEntries++
	m.enforceMemoryLimit()
}

// enforceMemoryLimit 先收敛 class 软配额，再按 priority + LRU 收敛全局
// memory 上限。当前占用完全来自增量计数，不需要重新遍历 memory map。
func (m *Manager) enforceMemoryLimit() {
	for name, class := range m.classes {
		if class.SoftQuota <= 0 {
			continue
		}
		used := m.ensureUsageLocked(name).memoryBytes
		for used > class.SoftQuota {
			victim := m.pickMemoryVictim(func(c Class, _ *entry) bool { return c.Name == name })
			if victim == nil {
				break
			}
			used -= victim.size
			m.removeMemory(victim)
			m.classStats(name).evictions.Add(1)
		}
	}
	if m.memoryLimit > 0 {
		m.evictMemoryToLocked(m.memoryLimit)
	}
}

func (m *Manager) memoryUsage() int64 {
	return m.memoryBytes
}

func (m *Manager) memoryUsageSnapshot() int64 {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.memoryBytes
}

func (m *Manager) evictMemoryToLocked(limit int64) {
	if limit < 0 {
		return
	}
	for m.memoryBytes > limit {
		victim := m.pickMemoryVictim(func(Class, *entry) bool { return true })
		if victim == nil {
			return
		}
		m.removeMemory(victim)
		m.classStats(victim.class.Name).evictions.Add(1)
	}
}

func (m *Manager) pruneMemoryTo(limit int64) {
	if limit < 0 {
		return
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	m.evictMemoryToLocked(limit)
}

func (m *Manager) pickMemoryVictim(accept func(Class, *entry) bool) *entry {
	var victim *entry
	var victimPriority int
	for cur := m.lru.Back(); cur != nil; cur = cur.Prev() {
		item := cur.Value.(*entry)
		if !accept(item.class, item) {
			continue
		}
		if victim == nil || item.class.Priority < victimPriority {
			victim, victimPriority = item, item.class.Priority
		}
	}
	return victim
}

func (m *Manager) removeMemory(item *entry) {
	qk := qualKey(item.class.Name, item.key)
	if current := m.memory[qk]; current != item {
		return
	}
	delete(m.memory, qk)
	if item.element != nil {
		m.lru.Remove(item.element)
	}
	m.memoryBytes -= item.size
	m.memoryEntries--
	used := m.ensureUsageLocked(item.class.Name)
	used.memoryBytes -= item.size
	used.memoryEntries--
}

// ---- disk L2 ----

func diskName(class, key string) string {
	sum := sha256.Sum256([]byte(qualKey(class, key)))
	return hex.EncodeToString(sum[:8])
}

func (m *Manager) diskPath(class, key string) string {
	return filepath.Join(m.diskDir, diskName(class, key)+".cache")
}

func (m *Manager) putDisk(c Class, key string, data []byte, expires time.Time) error {
	m.diskMu.Lock()
	defer m.diskMu.Unlock()
	path := m.diskPath(c.Name, key)
	tmp, err := os.CreateTemp(m.diskDir, "cache-")
	if err != nil {
		return err
	}
	name := tmp.Name()
	defer os.Remove(name)
	_, err = tmp.Write(data)
	if closeErr := tmp.Close(); err == nil {
		err = closeErr
	}
	if err != nil {
		return err
	}
	if err = os.Rename(name, path); err != nil {
		return err
	}
	expiresNano := int64(0)
	if !expires.IsZero() {
		expiresNano = expires.UnixNano()
	}
	meta := qualKey(c.Name, key) + "\n" + strconv.FormatInt(expiresNano, 10)
	if err = os.WriteFile(path+".meta", []byte(meta), 0o600); err != nil {
		_ = os.Remove(path)
		m.mu.Lock()
		m.removeDiskStateLocked(qualKey(c.Name, key), path)
		m.mu.Unlock()
		return err
	}
	info, statErr := os.Stat(path)
	mod := time.Now()
	if statErr == nil {
		mod = info.ModTime()
	}
	item := &diskItem{class: c.Name, key: key, path: path, size: int64(len(data)), mod: mod, accessed: time.Now(), expires: expires}
	m.mu.Lock()
	qk := qualKey(c.Name, key)
	if old := m.disk[qk]; old != nil {
		m.removeDiskStateLocked(qk, old.path)
	}
	m.disk[qk] = item
	m.diskBytes += item.size
	m.diskEntries++
	used := m.ensureUsageLocked(c.Name)
	used.diskBytes += item.size
	used.diskEntries++
	m.mu.Unlock()
	return nil
}

func (m *Manager) getDisk(class, key string) ([]byte, time.Time, bool) {
	m.diskMu.Lock()
	defer m.diskMu.Unlock()
	qk := qualKey(class, key)
	m.mu.Lock()
	item := m.disk[qk]
	if item != nil {
		copyItem := *item
		item = &copyItem
	}
	m.mu.Unlock()
	if item == nil {
		return nil, time.Time{}, false
	}
	if !item.expires.IsZero() && time.Now().After(item.expires) {
		m.removeDiskFilesLocked(*item)
		return nil, time.Time{}, false
	}
	data, err := os.ReadFile(item.path)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			_ = os.Remove(item.path + ".meta")
			m.mu.Lock()
			m.removeDiskStateLocked(qk, item.path)
			m.mu.Unlock()
		}
		return nil, time.Time{}, false
	}
	// Disk hits update only the in-memory access clock. Cache reads must not
	// turn into metadata writes.
	m.mu.Lock()
	if current := m.disk[qk]; current != nil && current.path == item.path {
		current.accessed = time.Now()
	}
	m.mu.Unlock()
	return data, item.expires, true
}

func (m *Manager) removeDiskStateLocked(qk, path string) bool {
	item := m.disk[qk]
	if item == nil || item.path != path {
		return false
	}
	delete(m.disk, qk)
	m.diskBytes -= item.size
	m.diskEntries--
	used := m.ensureUsageLocked(item.class)
	used.diskBytes -= item.size
	used.diskEntries--
	return true
}

// removeDiskFilesLocked removes a managed data file while diskMu is held. A
// missing data file is already absent from the cache and is treated as a
// successful removal so the in-memory counters stay truthful.
func (m *Manager) removeDiskFilesLocked(item diskItem) bool {
	err := os.Remove(item.path)
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return false
	}
	_ = os.Remove(item.path + ".meta")
	m.mu.Lock()
	removed := m.removeDiskStateLocked(qualKey(item.class, item.key), item.path)
	if removed {
		if counters, ok := m.stats[item.class]; ok {
			counters.evictions.Add(1)
		}
	}
	m.mu.Unlock()
	return true
}
