package storage

import (
	"math"
	"os"
	"path/filepath"
	"testing"
)

func TestDiskBlockCachePersistsAndEvictsLeastRecentlyUsed(t *testing.T) {
	dir := t.TempDir()
	first, second, third := []byte("aaa"), []byte("bbb"), []byte("ccc")
	firstID, secondID, thirdID := hashBytes(first), hashBytes(second), hashBytes(third)
	cache, err := newDiskBlockCache(dir, 6, 0, 16)
	if err != nil {
		t.Fatal(err)
	}
	if err := cache.put(firstID, first); err != nil {
		t.Fatal(err)
	}
	if err := cache.put(secondID, second); err != nil {
		t.Fatal(err)
	}
	if _, ok := cache.get(firstID, 3); !ok {
		t.Fatal("first block was not cached")
	}
	if err := cache.put(thirdID, third); err != nil {
		t.Fatal(err)
	}
	if _, ok := cache.get(secondID, 3); ok {
		t.Fatal("least recently used SSD block was not evicted")
	}

	reopened, err := newDiskBlockCache(dir, 6, 0, 16)
	if err != nil {
		t.Fatal(err)
	}
	if got, ok := reopened.get(firstID, 3); !ok || string(got) != "aaa" {
		t.Fatalf("persistent cache miss after reopen: %q ok=%v", got, ok)
	}
	if got, ok := reopened.get(thirdID, 3); !ok || string(got) != "ccc" {
		t.Fatalf("new SSD block missing after reopen: %q ok=%v", got, ok)
	}
}

func TestDiskBlockCacheRejectsCorruption(t *testing.T) {
	dir := t.TempDir()
	data := []byte("healthy")
	id := hashBytes(data)
	cache, err := newDiskBlockCache(dir, 1<<20, 0, 1<<20)
	if err != nil {
		t.Fatal(err)
	}
	if err := cache.put(id, data); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(dir, id[:2], id[2:])
	if err := os.WriteFile(path, []byte("corrupt"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, ok := cache.get(id, int64(len(data))); ok {
		t.Fatal("corrupt SSD block was returned")
	}
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Fatalf("corrupt SSD block was not removed: %v", err)
	}
}

func TestDiskBlockCacheHonorsMinimumFreeSpace(t *testing.T) {
	dir := t.TempDir()
	cache, err := newDiskBlockCache(dir, 1<<20, math.MaxInt64, 1<<20)
	if err != nil {
		t.Fatal(err)
	}
	data := []byte("reserved")
	id := hashBytes(data)
	if err := cache.put(id, data); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(dir, id[:2], id[2:])); !os.IsNotExist(err) {
		t.Fatalf("cache wrote through minimum-free-space guard: %v", err)
	}
}
