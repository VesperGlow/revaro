package storage

import "testing"

func TestBlockLRUEvictsLeastRecentlyUsedWithinByteLimit(t *testing.T) {
	cache := newBlockLRU(6)
	cache.put("first", []byte("aaa"))
	cache.put("second", []byte("bbb"))
	if _, ok := cache.get("first"); !ok {
		t.Fatal("first block was not cached")
	}
	cache.put("third", []byte("ccc"))
	if _, ok := cache.get("second"); ok {
		t.Fatal("least recently used block was not evicted")
	}
	if _, ok := cache.get("first"); !ok {
		t.Fatal("recently used block was evicted")
	}
	if _, ok := cache.get("third"); !ok {
		t.Fatal("new block was not cached")
	}
	if cache.size > cache.capacity {
		t.Fatalf("cache size=%d exceeds capacity=%d", cache.size, cache.capacity)
	}
}

func TestBlockLRUSkipsBlockLargerThanCapacity(t *testing.T) {
	cache := newBlockLRU(2)
	cache.put("large", []byte("abc"))
	if _, ok := cache.get("large"); ok || cache.size != 0 {
		t.Fatalf("oversized block was cached: size=%d", cache.size)
	}
}
