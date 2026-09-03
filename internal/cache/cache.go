// Package cache 实现 Revaro 的统一缓存管理器（Global CacheManager）。
//
// 之前分散在 server.CacheManager（字幕内存缓存 + 扁平磁盘缓存）、
// reader.DefaultCache（解析书 LRU）、flow 对象缓存与 media 会话缓存里的
// 生命周期、容量、LRU、singleflight、统计与失效策略统一收敛到这里；不同
// cache class 可以声明不同的 tier（memory L1 / local-disk L2）与策略。
//
// 统一 key namespace（class 即命名空间）：
//
//	reader/flow     flow manifest 与 chunk（内容寻址 immutable，无 TTL）
//	reader/source   书源 blob（内容寻址 immutable，避免冷启动回源 S3）
//	media/subtitle  字幕转换产物（真正临时，带 TTL）
//	media/hls       音视频 HLS 会话工作区（external：目录由会话自己管理，
//	                通过 RegisterExternal 纳入全局统计与容量回收）
//
// 容量策略：全局 memory/disk byte-LRU；class 可声明 priority（大者更晚
// 被淘汰）与 soft quota（超出后该 class 的最旧条目优先被淘汰），避免大型
// video range / HLS 工作区把 reader/图片缓存全部挤掉。TTL 只用于真正临时
// 的产物；内容哈希寻址的 immutable 缓存依赖内容/版本键 + 容量 LRU。
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

// ClassStats 是单个 class 的累计指标与当前占用。
type ClassStats struct {
	Hits, Misses, Loads, LoadErrors, Evictions int64
	MemoryBytes, DiskBytes                     int64
	MemoryEntries, DiskEntries                 int
}

// Stats 是全局缓存指标快照。
type Stats struct {
	Classes                     map[string]ClassStats
	MemoryBytes, DiskBytes      int64
	MemoryEntries, DiskEntries  int
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
	class string
	key   string
	path  string
	size  int64
	mod   time.Time
}

// Manager 是统一缓存管理器。并发安全。
type Manager struct {
	mu         sync.Mutex
	classes    map[string]Class
	memory     map[string]*entry // 限定键 class\x00key → entry
	lru        *list.List
	flights    map[string]*flight
	stats      map[string]*ClassStats
	diskDir    string
	diskBytes  int64
	memoryLimit int64
	diskLimit  int64
	extMu      sync.Mutex
	extStats   map[string]func() (int64, int)
	extPruners map[string]func()
	ctx        context.Context
	cancel     context.CancelFunc
	wg         sync.WaitGroup
	closed     bool
}

// New 创建管理器。diskDir 为 L2 根目录；memoryLimit/diskLimit 为全局字节
// 上限（0 = 不限）。
func New(diskDir string, memoryLimit, diskLimit int64) *Manager {
	_ = os.MkdirAll(diskDir, 0o700)
	ctx, cancel := context.WithCancel(context.Background())
	return &Manager{
		classes:     map[string]Class{},
		memory:      map[string]*entry{},
		lru:         list.New(),
		flights:     map[string]*flight{},
		stats:       map[string]*ClassStats{},
		diskDir:     diskDir,
		memoryLimit: memoryLimit,
		diskLimit:   diskLimit,
		extStats:    map[string]func() (int64, int){},
		extPruners:  map[string]func(){},
		ctx:         ctx,
		cancel:      cancel,
	}
}

// RegisterClass 注册 cache class（必须在任何使用之前完成，通常在服务启动
// 时统一注册）。使用未注册的 class 属于编程错误。
func (m *Manager) RegisterClass(c Class) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.classes[c.Name] = c
	if _, ok := m.stats[c.Name]; !ok {
		m.stats[c.Name] = &ClassStats{}
	}
}

func (m *Manager) classOf(name string) Class {
	c, ok := m.classes[name]
	if !ok {
		panic("cache: unregistered class " + name)
	}
	return c
}

func (m *Manager) classStats(name string) *ClassStats {
	s, ok := m.stats[name]
	if !ok {
		s = &ClassStats{}
		m.stats[name] = s
	}
	return s
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
		m.classStats(class).Hits++
		return data, true
	}
	m.classStats(class).Misses++
	return nil, false
}

// Has 报告条目在任一 tier 中是否存在（未过期）。不读取数据。
func (m *Manager) Has(class, key string) bool {
	c := m.classOf(class)
	m.mu.Lock()
	item := m.memory[qualKey(class, key)]
	valid := item != nil && (item.expires.IsZero() || !time.Now().After(item.expires))
	m.mu.Unlock()
	if valid {
		return true
	}
	if !c.Disk {
		return false
	}
	meta, err := os.ReadFile(m.diskPath(class, key) + ".meta")
	if err != nil {
		return false
	}
	parts := strings.SplitN(string(meta), "\n", 2)
	if len(parts) != 2 || parts[0] != qualKey(class, key) {
		return false
	}
	expires, _ := strconv.ParseInt(parts[1], 10, 64)
	return expires == 0 || time.Now().UnixNano() <= expires
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
		m.classStats(class).Hits++
		return data, nil
	}
	if c.Disk {
		if data, expires, ok := m.getDisk(class, key); ok {
			m.classStats(class).Hits++
			if c.Memory {
				m.putMemory(c, key, data, expires)
			}
			return data, nil
		}
	}
	m.classStats(class).Misses++
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
			m.classStats(class).Loads++
			if err != nil {
				m.classStats(class).LoadErrors++
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
	_ = os.Remove(m.diskPath(class, key) + ".meta")
	err := os.Remove(m.diskPath(class, key))
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	return err
}

// Invalidate 按逻辑键前缀跨 class 失效（memory + disk）。
func (m *Manager) Invalidate(prefix string) {
	m.mu.Lock()
	for qk, item := range m.memory {
		class, key, _ := strings.Cut(qk, "\x00")
		if strings.HasPrefix(key, prefix) || strings.HasPrefix(class+"\x00"+key, prefix) {
			m.removeMemory(item)
		}
	}
	m.mu.Unlock()
	entries, _ := os.ReadDir(m.diskDir)
	for _, e := range entries {
		if !strings.HasSuffix(e.Name(), ".meta") {
			continue
		}
		metaPath := filepath.Join(m.diskDir, e.Name())
		meta, _ := os.ReadFile(metaPath)
		parts := strings.SplitN(string(meta), "\n", 2)
		if len(parts) != 2 {
			continue
		}
		_, key, found := strings.Cut(parts[0], "\x00")
		if found && strings.HasPrefix(key, prefix) {
			_ = os.Remove(strings.TrimSuffix(metaPath, ".meta"))
			_ = os.Remove(metaPath)
		}
	}
}

// RegisterExternal 把不由本管理器直接存储的缓存（如 HLS 会话工作区、
// 解析书内存 LRU）纳入全局统计与容量回收。
func (m *Manager) RegisterExternal(name string, stats func() (int64, int), pruner func()) {
	m.extMu.Lock()
	m.extStats[name] = stats
	m.extPruners[name] = pruner
	m.extMu.Unlock()
}

// Prune 校正磁盘占用并按「超配额 class 优先 → 低 priority 优先 → 最久未
// 用优先」收敛到全局上限，然后触发 external pruner。
func (m *Manager) Prune() {
	items := m.scanDisk()
	var total int64
	perClass := map[string]int64{}
	for _, it := range items {
		total += it.size
		perClass[it.class] += it.size
	}
	// 1) class 软配额收敛：超配额 class 按自身 LRU 收敛回配额内
	items = m.evictDisk(items, func(it diskItem) bool {
		c, ok := m.classes[it.class]
		return ok && c.SoftQuota > 0 && perClass[it.class] > c.SoftQuota
	}, &total, perClass)
	// 2) 全局上限收敛（priority 大者保留）
	if m.diskLimit > 0 {
		items = m.evictDisk(items, func(diskItem) bool { return total > m.diskLimit }, &total, perClass)
	}
	m.extMu.Lock()
	pruners := make([]func(), 0, len(m.extPruners))
	for _, pruner := range m.extPruners {
		pruners = append(pruners, pruner)
	}
	m.extMu.Unlock()
	for _, pruner := range pruners {
		pruner()
	}
}

// evictDisk 按 predicate 圈定候选、priority 大者与最近使用者保留，逐个
// 删除直到 predicate 为假。返回删除后的 items。
func (m *Manager) evictDisk(items []diskItem, need func(diskItem) bool, total *int64, perClass map[string]int64) []diskItem {
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
		vp := m.classOf(victim.class).Priority
		for _, it := range candidates[1:] {
			p := m.classOf(it.class).Priority
			if p < vp || (p == vp && it.mod.Before(victim.mod)) {
				victim, vp = it, p
			}
		}
		_ = os.Remove(victim.path)
		_ = os.Remove(victim.path + ".meta")
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

func (m *Manager) scanDisk() []diskItem {
	entries, _ := os.ReadDir(m.diskDir)
	items := make([]diskItem, 0, len(entries))
	var total int64
	for _, e := range entries {
		if strings.HasSuffix(e.Name(), ".meta") {
			continue
		}
		info, err := e.Info()
		if err != nil || info.IsDir() {
			continue
		}
		meta, _ := os.ReadFile(filepath.Join(m.diskDir, e.Name()+".meta"))
		parts := strings.SplitN(string(meta), "\n", 2)
		class, key := "", ""
		if len(parts) == 2 {
			class, key, _ = strings.Cut(parts[0], "\x00")
			expires, _ := strconv.ParseInt(strings.TrimSpace(parts[1]), 10, 64)
			if expires > 0 && time.Now().UnixNano() > expires {
				_ = os.Remove(filepath.Join(m.diskDir, e.Name()))
				_ = os.Remove(filepath.Join(m.diskDir, e.Name()+".meta"))
				continue
			}
		} else {
			// 无 meta 的孤儿文件（写入中断残留）：直接清理
			_ = os.Remove(filepath.Join(m.diskDir, e.Name()))
			continue
		}
		items = append(items, diskItem{class: class, key: key, path: filepath.Join(m.diskDir, e.Name()), size: info.Size(), mod: info.ModTime()})
		total += info.Size()
	}
	m.mu.Lock()
	m.diskBytes = total
	m.mu.Unlock()
	return items
}

// Stats 汇总各 class 指标与占用（含 external 提供者）。
func (m *Manager) Stats() Stats {
	m.mu.Lock()
	out := Stats{Classes: map[string]ClassStats{}, MemoryEntries: len(m.memory)}
	for qk, item := range m.memory {
		class, _, _ := strings.Cut(qk, "\x00")
		cs := out.Classes[class]
		cs.MemoryBytes += item.size
		cs.MemoryEntries++
		out.MemoryBytes += item.size
		out.Classes[class] = cs
	}
	for name, s := range m.stats {
		cs := out.Classes[name]
		cs.Hits, cs.Misses, cs.Loads, cs.LoadErrors, cs.Evictions = s.Hits, s.Misses, s.Loads, s.LoadErrors, s.Evictions
		out.Classes[name] = cs
	}
	m.mu.Unlock()

	items := m.scanDisk()
	for _, it := range items {
		cs := out.Classes[it.class]
		cs.DiskBytes += it.size
		cs.DiskEntries++
		out.DiskBytes += it.size
		out.DiskEntries++
	}
	m.extMu.Lock()
	providers := make([]func() (int64, int), 0, len(m.extStats))
	for _, provider := range m.extStats {
		providers = append(providers, provider)
	}
	m.extMu.Unlock()
	for _, provider := range providers {
		if bytes, count := provider(); count > 0 {
			out.DiskBytes += bytes
			out.DiskEntries += count
		}
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
	m.enforceMemoryLimit()
}

// enforceMemoryLimit 全局 byte-LRU：超限先淘汰超配额 class 的最旧条目，
// 再淘汰低 priority 的最旧条目。
func (m *Manager) enforceMemoryLimit() {
	if m.memoryLimit <= 0 {
		return
	}
	usage := m.memoryUsage()
	for usage > m.memoryLimit {
		victim := m.pickMemoryVictim(func(c Class, _ *entry) bool { return true })
		if victim == nil {
			return
		}
		usage -= victim.size
		m.removeMemory(victim)
		m.classStats(victim.class.Name).Evictions++
	}
	// class 软配额：超出者收敛回配额
	perClass := map[string]int64{}
	for _, item := range m.memory {
		perClass[item.class.Name] += item.size
	}
	for name, used := range perClass {
		c, ok := m.classes[name]
		if !ok || c.SoftQuota <= 0 || used <= c.SoftQuota {
			continue
		}
		for used > c.SoftQuota {
			victim := m.pickMemoryVictim(func(c Class, _ *entry) bool { return c.Name == name })
			if victim == nil {
				break
			}
			used -= victim.size
			m.removeMemory(victim)
			m.classStats(name).Evictions++
		}
	}
}

func (m *Manager) memoryUsage() int64 {
	var total int64
	for _, item := range m.memory {
		total += item.size
	}
	return total
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
	delete(m.memory, qualKey(item.class.Name, item.key))
	m.lru.Remove(item.element)
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
	path := m.diskPath(c.Name, key)
	tmp, err := os.CreateTemp(m.diskDir, "cache-")
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
	expiresNano := int64(0)
	if !expires.IsZero() {
		expiresNano = expires.UnixNano()
	}
	meta := qualKey(c.Name, key) + "\n" + strconv.FormatInt(expiresNano, 10)
	if err = os.WriteFile(path+".meta", []byte(meta), 0o600); err != nil {
		_ = os.Remove(path)
		return err
	}
	m.mu.Lock()
	m.diskBytes += int64(len(data))
	m.mu.Unlock()
	return nil
}

func (m *Manager) getDisk(class, key string) ([]byte, time.Time, bool) {
	path := m.diskPath(class, key)
	meta, err := os.ReadFile(path + ".meta")
	if err != nil {
		return nil, time.Time{}, false
	}
	parts := strings.SplitN(string(meta), "\n", 2)
	if len(parts) != 2 || parts[0] != qualKey(class, key) {
		return nil, time.Time{}, false
	}
	expiresNano, _ := strconv.ParseInt(parts[1], 10, 64)
	if expiresNano > 0 && time.Now().UnixNano() > expiresNano {
		_ = m.Delete(class, key)
		return nil, time.Time{}, false
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, time.Time{}, false
	}
	_ = os.Chtimes(path, time.Now(), time.Now())
	var expires time.Time
	if expiresNano > 0 {
		expires = time.Unix(0, expiresNano)
	}
	return data, expires, true
}
