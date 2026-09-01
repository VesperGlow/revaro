package server

import (
	"context"
	"testing"
	"time"
)

func TestResourceGovernorSerializesHeavyWork(t *testing.T) {
	g := newResourceGovernor()
	release, err := g.Heavy(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()
	if _, err := g.Heavy(ctx); err == nil {
		t.Fatal("second heavy task bypassed the global CPU limit")
	}
	release()
	release2, err := g.Heavy(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	release2()
}

func TestResourceGovernorBoundsSharedIO(t *testing.T) {
	g := newResourceGovernor()
	releases := make([]func(), 0, 3)
	for i := 0; i < 3; i++ {
		release, err := g.IO(context.Background())
		if err != nil {
			t.Fatal(err)
		}
		releases = append(releases, release)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()
	if _, err := g.IO(ctx); err == nil {
		t.Fatal("fourth IO task bypassed the global limit")
	}
	for _, release := range releases {
		release()
	}
}
