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
