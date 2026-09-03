package cache

import (
	"context"
	"errors"
	"strconv"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

func newTestManager(t *testing.T, memoryLimit, diskLimit int64) *Manager {
	t.Helper()
	m := New(t.TempDir(), memoryLimit, diskLimit)
	m.RegisterClass(Class{Name: "reader/flow", Priority: 50, SoftQuota: 64, Memory: true, Disk: true})
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
	data, err := m.Load(context.Background(), "reader/flow", "a", 0, load)
	if err != nil || string(data) != "payload" || loads.Load() != 1 {
		t.Fatalf("first load: %q %v loads=%d", data, err, loads.Load())
	}
	// 第二次：L1 命中，不回源
	if data, err = m.Load(context.Background(), "reader/flow", "a", 0, load); err != nil || string(data) != "payload" || loads.Load() != 1 {
		t.Fatalf("second load should hit L1: %q %v loads=%d", data, err, loads.Load())
	}
	// 清掉 L1 后：L2（磁盘）命中并回填，仍不回源
	if err := m.Delete("reader/flow", "a"); err != nil {
		t.Fatal(err)
	}
	// Delete 同时删除了磁盘层；重新 Put 只写磁盘（模拟 L2 残留）
	if err := m.putDisk(m.classOf("reader/flow"), "a", []byte("payload"), time.Time{}); err != nil {
		t.Fatal(err)
	}
	data, err = m.Load(context.Background(), "reader/flow", "a", 0, load)
	if err != nil || string(data) != "payload" || loads.Load() != 1 {
		t.Fatalf("L2 hit expected: %q %v loads=%d", data, err, loads.Load())
	}
	stats := m.Stats().Classes["reader/flow"]
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
	if _, err := m.Load(context.Background(), "media/subtitle", "s1", 10*time.Millisecond, func(context.Context) ([]byte, error) {
		return []byte("vtt"), nil
	}); err != nil {
		t.Fatal(err)
	}
	if _, ok := m.Get("media/subtitle", "s1"); !ok {
		t.Fatal("fresh entry should hit")
	}
	time.Sleep(20 * time.Millisecond)
	if _, ok := m.Get("media/subtitle", "s1"); ok {
		t.Fatal("expired memory entry was served")
	}
	if m.Has("media/subtitle", "s1") {
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
}

func TestLoadSingleFlightDeduplicatesAndPropagatesErrors(t *testing.T) {
	m := newTestManager(t, 1<<20, 1<<20)
	var calls atomic.Int32
	release := make(chan struct{})
	var results []string
	var wg sync.WaitGroup
	for range 3 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			data, err := m.Load(context.Background(), "reader/flow", "same", 0, func(context.Context) ([]byte, error) {
				calls.Add(1)
				<-release
				return []byte("one"), nil
			})
			results = append(results, string(data)+"/"+strconv.FormatBool(err == nil))
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
	if _, err := m.Load(context.Background(), "reader/flow", "bad", 0, func(context.Context) ([]byte, error) {
		return nil, wantErr
	}); !errors.Is(err, wantErr) {
		t.Fatalf("load error = %v", err)
	}
	stats := m.Stats().Classes["reader/flow"]
	if stats.LoadErrors != 1 {
		t.Fatalf("load errors = %d", stats.LoadErrors)
	}
}

func TestInvalidateByPrefixAcrossClasses(t *testing.T) {
	m := newTestManager(t, 1<<20, 1<<20)
	m.Put("reader/flow", "manifest/blobs/x", []byte("m"), 0)
	m.Put("reader/flow", "chunk/blobs/x/0", []byte("c"), 0)
	m.Put("media/subtitle", "embedded-v2:file:1", []byte("s"), 0)
	m.Invalidate("embedded-v2:file:")
	if m.Has("media/subtitle", "embedded-v2:file:1") {
		t.Fatal("prefix invalidation missed subtitle entry")
	}
	if !m.Has("reader/flow", "manifest/blobs/x") {
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
