package server

import (
	"context"
	"sync"
	"testing"
	"time"
)

func TestDownloadManagerCloseWaitsForOwnedWork(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	m := &downloadManager{
		ctx: ctx, cancel: cancel,
		jobs: make(map[string]*downloadRuntime), urlJobs: make(map[string]*urlDownloadRuntime),
	}
	finished := make(chan struct{})
	if !m.runBackground(func() {
		<-ctx.Done()
		close(finished)
	}) {
		t.Fatal("download work was rejected before shutdown")
	}
	m.Close()
	select {
	case <-finished:
	default:
		t.Fatal("download manager returned before its worker exited")
	}
	if m.runBackground(func() {}) {
		t.Fatal("download work was admitted after shutdown")
	}
}

func TestAudioMergeRemovalUsesServerLifecycle(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	job := &audioMergeJob{ID: "terminal"}
	s := &Server{
		audioHLSCtx: ctx, audioHLSCancel: cancel,
		audioMergeJobs: map[string]*audioMergeJob{job.ID: job},
	}
	if !s.scheduleAudioMergeRemoval(job, time.Millisecond) {
		t.Fatal("terminal cleanup was rejected before shutdown")
	}
	deadline := time.Now().Add(time.Second)
	for {
		s.audioMergeMu.RLock()
		_, exists := s.audioMergeJobs[job.ID]
		s.audioMergeMu.RUnlock()
		if !exists {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("terminal audio merge was not removed")
		}
		time.Sleep(time.Millisecond)
	}
	s.Close()
}

func TestJobManagerCancelAndNotifyRace(t *testing.T) {
	manager := NewJobManager()
	var wg sync.WaitGroup
	for i := 0; i < 100; i++ {
		updates, cancel := manager.Subscribe()
		wg.Add(2)
		go func() { defer wg.Done(); manager.Changed() }()
		go func() {
			defer wg.Done()
			cancel()
			select {
			case <-updates:
			default:
			}
		}()
	}
	wg.Wait()
	manager.Close()
}

func TestJobManagerDeliversCoalescedChanges(t *testing.T) {
	manager := NewJobManager()
	updates, cancel := manager.Subscribe()
	defer cancel()
	defer manager.Close()
	manager.Changed()
	manager.Changed()
	select {
	case <-updates:
	case <-time.After(time.Second):
		t.Fatal("job update was not delivered")
	}
}
