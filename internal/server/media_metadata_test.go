package server

import (
	"context"
	"sync/atomic"
	"testing"
	"time"
)

func TestMediaAnalysisSchedulerLimitsConcurrencyAndDeduplicatesFileIDs(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	queue := newMediaAnalysisScheduler(2)
	started := make(chan string, 4)
	release := make(chan struct{})
	finished := make(chan struct{}, 4)
	var running, maximum atomic.Int32
	work := func(id string) func(context.Context) {
		return func(context.Context) {
			current := running.Add(1)
			for observed := maximum.Load(); current > observed && !maximum.CompareAndSwap(observed, current); observed = maximum.Load() {
			}
			started <- id
			<-release
			running.Add(-1)
			finished <- struct{}{}
		}
	}
	if !queue.schedule(ctx, "file-a", work("file-a")) {
		t.Fatal("first file was not scheduled")
	}
	if queue.schedule(ctx, "file-a", work("duplicate")) {
		t.Fatal("duplicate file ID was scheduled")
	}
	if !queue.schedule(ctx, "file-b", work("file-b")) || !queue.schedule(ctx, "file-c", work("file-c")) {
		t.Fatal("distinct files were not scheduled")
	}
	for range 2 {
		select {
		case <-started:
		case <-time.After(time.Second):
			t.Fatal("two analysis workers did not start")
		}
	}
	select {
	case id := <-started:
		t.Fatalf("third analysis %q started before a slot was released", id)
	case <-time.After(50 * time.Millisecond):
	}
	close(release)
	for range 3 {
		select {
		case <-finished:
		case <-time.After(time.Second):
			t.Fatal("scheduled analysis did not finish")
		}
	}
	if got := maximum.Load(); got != 2 {
		t.Fatalf("maximum concurrent analyses=%d, want 2", got)
	}
}
