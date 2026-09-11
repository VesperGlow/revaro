package server

import (
	"sync"
	"testing"
	"time"
)

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
