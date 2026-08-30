package dataplane

import (
	"context"
	"os"
	"path/filepath"
	"runtime"
	"testing"
	"time"
)

func TestStartCancellationReapsUnreadyProcess(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("process groups are Unix-specific")
	}
	binary := filepath.Join(t.TempDir(), "unready-data-plane")
	if err := os.WriteFile(binary, []byte("#!/bin/sh\ntrap 'exit 0' TERM INT\nwhile :; do sleep 1; done\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 150*time.Millisecond)
	defer cancel()
	started := time.Now()
	process, err := Start(ctx, binary, "127.0.0.1:39871", nil)
	if process != nil || err == nil {
		t.Fatalf("process=%v error=%v", process, err)
	}
	if elapsed := time.Since(started); elapsed > 3*time.Second {
		t.Fatalf("cancelled startup took %v", elapsed)
	}
}
