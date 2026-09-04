package cache

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strconv"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

func newTestManager(t *testing.T, memoryLimit, diskLimit int64) *Manager {
	t.Helper()
	m := New(t.TempDir(), memoryLimit, diskLimit)
	m.RegisterClass(Class{Name: "reader/flow-manifest", Priority: 90, SoftQuota: 64, Memory: true})
	m.RegisterClass(Class{Name: "reader/flow-chunk", Priority: 70, SoftQuota: 128, Memory: true})
	m.RegisterClass(Class{Name: "tiered", Priority: 50, SoftQuota: 64, Memory: true, Disk: true})
	m.RegisterClass(Class{Name: "reader/source", Priority: 40, SoftQuota: 64, Memory: false, Disk: true})
	m.RegisterClass(Class{Name: "media/subtitle", Priority: 10, Memory: true, Disk: true})
	t.Cleanup(m.Close)
	return m
}

func TestLoadTierFallbackAndBackfill(t *testing.T) {
	m := newTestManager(t, 1<<20, 1<<20)
	var loads atomic.Int32
	load := func(context.Context) ([]byte, error) {
		loads.Add(1)
		return []byte("payload"), nil
	}
	// 首次：回源并回填 L1
	data, err := m.Load(context.Background(), "tiered", "a", 0, load)
	if err != nil || string(data) != "payload" || loads.Load() != 1 {
		t.Fatalf("first load: %q %v loads=%d", data, err, loads.Load())
	}
	// 第二次：L1 命中，不回源
	if data, err = m.Load(context.Background(), "tiered", "a", 0, load); err != nil || string(data) != "payload" || loads.Load() != 1 {
		t.Fatalf("second load should hit L1: %q %v loads=%d", data, err, loads.Load())
	}
	// 清掉 L1 后：L2（磁盘）命中并回填，仍不回源
	if err := m.Delete("tiered", "a"); err != nil {
		t.Fatal(err)
	}
	// Delete 同时删除了磁盘层；重新 Put 只写磁盘（模拟 L2 残留）
	if err := m.putDisk(m.classOf("tiered"), "a", []byte("payload"), time.Time{}); err != nil {
		t.Fatal(err)
	}
	data, err = m.Load(context.Background(), "tiered", "a", 0, load)
	if err != nil || string(data) != "payload" || loads.Load() != 1 {
		t.Fatalf("L2 hit expected: %q %v loads=%d", data, err, loads.Load())
	}
	stats := m.Stats().Classes["tiered"]
	if stats.Hits < 2 || stats.Misses != 1 || stats.Loads != 1 {
		t.Fatalf("stats wrong: %+v", stats)
	}
}

func TestDiskOnlyClassNeverUsesMemory(t *testing.T) {
	m := newTestManager(t, 1<<20, 1<<20)
	if _, err := m.Load(context.Background(), "reader/source", "blob", 0, func(context.Context) ([]byte, error) {
		return []byte("book"), nil
	}); err != nil {
		t.Fatal(err)
	}
	if _, ok := m.Get("reader/source", "blob"); ok {
		t.Fatal("disk-only class must not backfill memory")
	}
	if !m.Has("reader/source", "blob") {
		t.Fatal("disk entry should exist")
	}
}

func TestTTLExpiry(t *testing.T) {
	m := newTestManager(t, 1<<20, 1<<20)
	// 新鲜条目：长 TTL，避免 CI 慢机上短 TTL 在断言前过期的抖动
	if _, err := m.Load(context.Background(), "media/subtitle", "fresh", time.Minute, func(context.Context) ([]byte, error) {
		return []byte("vtt"), nil
	}); err != nil {
		t.Fatal(err)
	}
	if _, ok := m.Get("media/subtitle", "fresh"); !ok {
		t.Fatal("fresh entry should hit")
	}
	// 过期：短 TTL + 足量等待
	if _, err := m.Load(context.Background(), "media/subtitle", "stale", 60*time.Millisecond, func(context.Context) ([]byte, error) {
		return []byte("vtt"), nil
	}); err != nil {
		t.Fatal(err)
	}
	time.Sleep(250 * time.Millisecond)
	if _, ok := m.Get("media/subtitle", "stale"); ok {
		t.Fatal("expired memory entry was served")
	}
	if m.Has("media/subtitle", "stale") {
		t.Fatal("expired disk entry reported present")
	}
}

func TestMemoryLRUEvictsLowestPriorityAndOverQuotaFirst(t *testing.T) {
	// 预算 12 字节：写满 hot(6)+cold(6) 后再写 hot(6) → 低 priority 的
	// cold 被逐出，hot 全部保留。
	m := New(t.TempDir(), 12, 0)
	m.RegisterClass(Class{Name: "hot", Priority: 90, Memory: true})
	m.RegisterClass(Class{Name: "cold", Priority: 1, Memory: true})
	t.Cleanup(m.Close)
	m.Put("hot", "h", []byte("111111"), 0)
	m.Put("cold", "c", []byte("222222"), 0)
	m.Put("hot", "h2", []byte("333333"), 0)
	if _, ok := m.Get("hot", "h"); !ok {
		t.Fatal("hot entry evicted despite higher priority")
	}
	if _, ok := m.Get("cold", "c"); ok {
		t.Fatal("cold entry should be evicted first")
	}

	// 同 priority 内按 LRU：预算 6 字节，h2 覆盖后 h 最旧 → 被逐出。
	m3 := New(t.TempDir(), 6, 0)
	m3.RegisterClass(Class{Name: "hot", Priority: 90, Memory: true})
	t.Cleanup(m3.Close)
	m3.Put("hot", "h", []byte("111111"), 0)
	m3.Put("hot", "h2", []byte("222222"), 0)
	if _, ok := m3.Get("hot", "h"); ok {
		t.Fatal("oldest same-priority entry should be evicted")
	}
	if _, ok := m3.Get("hot", "h2"); !ok {
		t.Fatal("newest entry should survive")
	}

	// 软配额：big 配额 6 字节，写入 12 字节后触发收敛；small 不受影响。
	m2 := New(t.TempDir(), 1<<20, 0)
	m2.RegisterClass(Class{Name: "big", Priority: 50, SoftQuota: 6, Memory: true})
	m2.RegisterClass(Class{Name: "small", Priority: 50, Memory: true})
	t.Cleanup(m2.Close)
	m2.Put("big", "b1", []byte("aaaa"), 0)
	m2.Put("big", "b2", []byte("bbbb"), 0)
	m2.Put("small", "s", []byte("cc"), 0)
	m2.enforceMemoryLimit()
	stats := m2.Stats().Classes["big"]
	if stats.MemoryBytes > 6 || stats.Evictions == 0 {
		t.Fatalf("soft quota not enforced: %+v", stats)
	}
	if _, ok := m2.Get("small", "s"); !ok {
		t.Fatal("unrelated class entry was evicted by quota enforcement")
	}
}

func TestDiskPruneRespectsQuotaAndPriority(t *testing.T) {
	m := New(t.TempDir(), 0, 10)
	m.RegisterClass(Class{Name: "media", Priority: 1, SoftQuota: 6, Disk: true})
	m.RegisterClass(Class{Name: "reader", Priority: 50, SoftQuota: 100, Disk: true})
	t.Cleanup(m.Close)
	if err := m.putDisk(m.classOf("media"), "v1", []byte("111111"), time.Time{}); err != nil {
		t.Fatal(err)
	}
	if err := m.putDisk(m.classOf("reader"), "r1", []byte("222222"), time.Time{}); err != nil {
		t.Fatal(err)
	}
	// media 超配额：只收缩 media，reader 不动
	m.Prune()
	if !m.Has("reader", "r1") {
		t.Fatal("high priority class was pruned while over-quota class shrank")
	}
	// 全局超限：media（低 priority）先被淘汰
	if err := m.putDisk(m.classOf("media"), "v2", []byte("333333"), time.Time{}); err != nil {
		t.Fatal(err)
	}
	m.Prune()
	if m.Has("media", "v1") || m.Has("media", "v2") {
		t.Fatal("low priority class should be evicted under global pressure")
	}
	if !m.Has("reader", "r1") {
		t.Fatal("reader entry should survive global eviction")
	}
	stats := m.Stats()
	if stats.DiskBytes != 6 || stats.DiskEntries != 1 || stats.Classes["reader"].DiskBytes != 6 {
		t.Fatalf("disk counters after prune = %+v", stats)
	}
}

func TestExternalStatsKeepMemoryAndDiskSeparate(t *testing.T) {
	m := New(t.TempDir(), 1<<20, 1<<20)
	m.RegisterClass(Class{Name: "managed", Priority: 10, Memory: true, Disk: true})
	m.RegisterExternal("reader/books", func() ExternalStats {
		return ExternalStats{MemoryBytes: 11, MemoryEntries: 2}
	}, nil)
	m.RegisterExternal("media/hls", func() ExternalStats {
		return ExternalStats{DiskBytes: 13, DiskEntries: 1}
	}, nil)
	t.Cleanup(m.Close)
	m.Put("managed", "one", []byte("123"), 0)

	stats := m.Stats()
	if stats.MemoryBytes != 14 || stats.MemoryEntries != 3 {
		t.Fatalf("memory total = %d/%d", stats.MemoryBytes, stats.MemoryEntries)
	}
	if stats.DiskBytes != 16 || stats.DiskEntries != 2 {
		t.Fatalf("disk total = %d/%d", stats.DiskBytes, stats.DiskEntries)
	}
	books := stats.Classes["reader/books"]
	if books.MemoryBytes != 11 || books.MemoryEntries != 2 || books.DiskBytes != 0 || books.DiskEntries != 0 {
		t.Fatalf("reader/books stats = %+v", books)
	}
	hls := stats.Classes["media/hls"]
	if hls.DiskBytes != 13 || hls.DiskEntries != 1 || hls.MemoryBytes != 0 || hls.MemoryEntries != 0 {
		t.Fatalf("media/hls stats = %+v", hls)
	}
}

func TestStatsUsesIncrementalDiskCountersUntilPrune(t *testing.T) {
	dir := t.TempDir()
	m := New(dir, 0, 1<<20)
	m.RegisterClass(Class{Name: "disk", Priority: 10, Disk: true})
	t.Cleanup(m.Close)
	m.Put("disk", "key", []byte("1234"), 0)
	if stats := m.Stats().Classes["disk"]; stats.DiskBytes != 4 || stats.DiskEntries != 1 {
		t.Fatalf("after put = %+v", stats)
	}
	m.Put("disk", "key", []byte("123456"), 0)
	if stats := m.Stats().Classes["disk"]; stats.DiskBytes != 6 || stats.DiskEntries != 1 {
		t.Fatalf("after replacement = %+v", stats)
	}
	if err := m.Delete("disk", "key"); err != nil {
		t.Fatal(err)
	}
	if stats := m.Stats().Classes["disk"]; stats.DiskBytes != 0 || stats.DiskEntries != 0 {
		t.Fatalf("after delete = %+v", stats)
	}

	// A valid cache file appearing outside Manager is intentionally invisible
	// until the explicit reconciliation pass.
	externalPath := filepath.Join(dir, "external.cache")
	if err := os.WriteFile(externalPath, []byte("orphan"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(externalPath+".meta", []byte("disk\x00external\n0"), 0o600); err != nil {
		t.Fatal(err)
	}
	if stats := m.Stats(); stats.DiskBytes != 0 || stats.DiskEntries != 0 {
		t.Fatalf("Stats scanned an unindexed file = %+v", stats)
	}
	m.Prune()
	stats := m.Stats().Classes["disk"]
	if stats.DiskBytes != 6 || stats.DiskEntries != 1 || !m.Has("disk", "external") {
		t.Fatalf("Prune did not reconcile external file: %+v", stats)
	}
}

func TestDiskHitTouchesMemoryLRUWithoutChangingFileMtime(t *testing.T) {
	dir := t.TempDir()
	m := New(dir, 0, 10)
	m.RegisterClass(Class{Name: "disk", Priority: 10, Disk: true})
	t.Cleanup(m.Close)
	m.Put("disk", "old", []byte("111111"), 0)
	m.Put("disk", "new", []byte("222222"), 0)
	oldPath := m.diskPath("disk", "old")
	before, err := os.Stat(oldPath)
	if err != nil {
		t.Fatal(err)
	}
	if _, _, ok := m.getDisk("disk", "old"); !ok {
		t.Fatal("disk hit failed")
	}
	after, err := os.Stat(oldPath)
	if err != nil {
		t.Fatal(err)
	}
	if !before.ModTime().Equal(after.ModTime()) {
		t.Fatalf("disk hit changed mtime: before=%v after=%v", before.ModTime(), after.ModTime())
	}

	m.Prune()
	if !m.Has("disk", "old") || m.Has("disk", "new") {
		t.Fatal("disk LRU did not preserve the touched entry")
	}
}

func TestExternalDiskUsageReservesGlobalBudget(t *testing.T) {
	m := New(t.TempDir(), 0, 10)
	m.RegisterClass(Class{Name: "managed", Priority: 50, Disk: true})
	m.RegisterExternal("media/hls", func() ExternalStats {
		return ExternalStats{DiskBytes: 8, DiskEntries: 1}
	}, nil) // active/unreclaimable external usage
	t.Cleanup(m.Close)
	m.Put("managed", "key", []byte("123456"), 0)
	m.Prune()
	if m.Has("managed", "key") {
		t.Fatal("managed cache did not yield budget to external disk usage")
	}
	stats := m.Stats()
	if stats.DiskBytes != 8 || stats.DiskEntries != 1 {
		t.Fatalf("global disk stats = %+v", stats)
	}
}

func TestLoadSingleFlightDeduplicatesAndPropagatesErrors(t *testing.T) {
	m := newTestManager(t, 1<<20, 1<<20)
	var calls atomic.Int32
	release := make(chan struct{})
	var resultsMu sync.Mutex
	var results []string
	var wg sync.WaitGroup
	for range 3 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			data, err := m.Load(context.Background(), "reader/flow-manifest", "same", 0, func(context.Context) ([]byte, error) {
				calls.Add(1)
				<-release
				return []byte("one"), nil
			})
			resultsMu.Lock()
			results = append(results, string(data)+"/"+strconv.FormatBool(err == nil))
			resultsMu.Unlock()
		}()
	}
	time.Sleep(20 * time.Millisecond)
	close(release)
	wg.Wait()
	if calls.Load() != 1 {
		t.Fatalf("loader called %d times", calls.Load())
	}
	for _, r := range results {
		if r != "one/true" {
			t.Fatalf("waiter result %q", r)
		}
	}
	// 失败不缓存：下一次重新加载
	wantErr := errors.New("boom")
	if _, err := m.Load(context.Background(), "reader/flow-manifest", "bad", 0, func(context.Context) ([]byte, error) {
		return nil, wantErr
	}); !errors.Is(err, wantErr) {
		t.Fatalf("load error = %v", err)
	}
	stats := m.Stats().Classes["reader/flow-manifest"]
	if stats.LoadErrors != 1 {
		t.Fatalf("load errors = %d", stats.LoadErrors)
	}
}

func TestInvalidateByPrefixAcrossClasses(t *testing.T) {
	m := newTestManager(t, 1<<20, 1<<20)
	m.Put("reader/flow-manifest", "manifest/blobs/x", []byte("m"), 0)
	m.Put("reader/flow-chunk", "chunk/blobs/x/0", []byte("c"), 0)
	m.Put("media/subtitle", "embedded-v2:file:1", []byte("s"), 0)
	m.Invalidate("embedded-v2:file:")
	if m.Has("media/subtitle", "embedded-v2:file:1") {
		t.Fatal("prefix invalidation missed subtitle entry")
	}
	if !m.Has("reader/flow-manifest", "manifest/blobs/x") {
		t.Fatal("unrelated class entry was invalidated")
	}
}

func TestUnregisteredClassPanics(t *testing.T) {
	m := New(t.TempDir(), 0, 0)
	defer func() {
		if recover() == nil {
			t.Fatal("unregistered class should panic")
		}
	}()
	_ = m.Has("nope", "k")
}
