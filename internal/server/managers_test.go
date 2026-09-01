package server

import (
	"context"
	"errors"
	"io"
	"log/slog"
	"sync/atomic"
	"testing"
	"time"
)

func TestCacheManagerMemoryDiskTTLAndSingleFlight(t *testing.T) {
	c := newCacheManager(t.TempDir(), 8, 32)
	c.Put("old", []byte("12345678"), time.Minute)
	c.Put("new", []byte("abcdefgh"), time.Minute)
	if _, ok := c.Get("old"); ok {
		t.Fatal("memory LRU did not evict oldest entry")
	}
	if err := c.PutDisk("subtitle:file:1", []byte("vtt"), 10*time.Millisecond); err != nil {
		t.Fatal(err)
	}
	if got, ok := c.GetDisk("subtitle:file:1"); !ok || string(got) != "vtt" {
		t.Fatalf("disk cache miss: %q %v", got, ok)
	}
	time.Sleep(15 * time.Millisecond)
	if _, ok := c.GetDisk("subtitle:file:1"); ok {
		t.Fatal("expired disk entry was served")
	}
	var calls atomic.Int32
	release := make(chan struct{})
	results := make(chan string, 2)
	for range 2 {
		go func() {
			data, err := c.GetOrCreate(context.Background(), "same", time.Minute, func(context.Context) ([]byte, error) { calls.Add(1); <-release; return []byte("one"), nil })
			if err != nil {
				results <- err.Error()
			} else {
				results <- string(data)
			}
		}()
	}
	time.Sleep(10 * time.Millisecond)
	close(release)
	if a, b := <-results, <-results; a != "one" || b != "one" || calls.Load() != 1 {
		t.Fatalf("results=%q,%q calls=%d", a, b, calls.Load())
	}
}

func TestAppErrorKeepsCauseAndSafeMessage(t *testing.T) {
	cause := errors.New("secret storage endpoint detail")
	err := appError("object_put_failed", "文件写入失败", cause, true)
	if !errors.Is(err, cause) || publicError(err, "fallback") != "文件写入失败" || !err.Retryable {
		t.Fatalf("unexpected app error: %+v", err)
	}
}

func TestCleanupManagerRetriesFailedPass(t *testing.T) {
	manager := newCleanupManager(slog.New(slog.NewTextHandler(io.Discard, nil)), newResourceGovernor())
	defer manager.Close()
	var calls atomic.Int32
	done := make(chan struct{})
	manager.Register("retry", time.Hour, time.Second, true, func(context.Context) error {
		if calls.Add(1) == 1 {
			return errors.New("temporary")
		}
		close(done)
		return nil
	})
	// Force the retry due immediately instead of waiting for production backoff.
	manager.Start()
	time.Sleep(20 * time.Millisecond)
	manager.mu.Lock()
	manager.jobs["retry"].next = time.Now()
	manager.mu.Unlock()
	manager.wake <- struct{}{}
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("cleanup pass was not retried")
	}
}
