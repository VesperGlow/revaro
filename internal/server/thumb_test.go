package server

import (
	"context"
	"sync/atomic"
	"testing"
	"time"
)

func TestThumbnailSchedulerDeduplicatesAndLimitsConcurrency(t *testing.T) {
	queue := newThumbnailScheduler(1)
	defer queue.close()
	release := make(chan struct{})
	started := make(chan struct{}, 2)
	var running atomic.Int32
	var maximum atomic.Int32
	work := func(context.Context) {
		current := running.Add(1)
		for observed := maximum.Load(); current > observed && !maximum.CompareAndSwap(observed, current); observed = maximum.Load() {
		}
		started <- struct{}{}
		<-release
		running.Add(-1)
	}
	if !queue.schedule("same-version", work) {
		t.Fatal("first thumbnail was not scheduled")
	}
	if queue.schedule("same-version", work) {
		t.Fatal("duplicate thumbnail was scheduled")
	}
	if !queue.schedule("other-version", work) {
		t.Fatal("second thumbnail was not queued")
	}
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("thumbnail worker did not start")
	}
	select {
	case <-started:
		t.Fatal("concurrency limit was exceeded")
	case <-time.After(20 * time.Millisecond):
	}
	release <- struct{}{}
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("queued thumbnail did not start")
	}
	release <- struct{}{}
	if maximum.Load() != 1 {
		t.Fatalf("maximum concurrency = %d", maximum.Load())
	}
}
