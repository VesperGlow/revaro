package server

import (
	"encoding/json"
	"net/http"
	"testing"
)

func TestSystemStatusRequiresAuthenticationAndReportsComponents(t *testing.T) {
	app := newTestApp(t)
	if response := app.request(http.MethodGet, "/api/system/status", nil, false); response.Code != http.StatusUnauthorized {
		t.Fatalf("unauthenticated status = %d", response.Code)
	}
	response := app.request(http.MethodGet, "/api/system/status", nil, true)
	if response.Code != http.StatusOK {
		t.Fatalf("status = %d: %s", response.Code, response.Body.String())
	}
	var status systemStatusResponse
	if err := json.Unmarshal(response.Body.Bytes(), &status); err != nil {
		t.Fatal(err)
	}
	if status.Database.Status != "ok" || status.Database.Bytes <= 0 || status.Storage.Status != "ok" {
		t.Fatalf("unexpected status: %+v", status)
	}
	if status.Tasks.Running != 0 || status.ObjectCleanup.Pending != 0 {
		t.Fatalf("unexpected counters: %+v", status)
	}
	if status.LocalDisk.Status != "ok" && status.LocalDisk.Status != "degraded" && status.LocalDisk.Status != "critical" {
		t.Fatalf("unexpected local disk health: %+v", status.LocalDisk)
	}
	if status.LocalDisk.TotalBytes <= 0 || status.LocalDisk.AvailableBytes <= 0 || status.LocalDisk.UsedPercent < 0 || status.LocalDisk.UsedPercent > 100 {
		t.Fatalf("unexpected local disk status: %+v", status.LocalDisk)
	}
}

func TestLocalFilesystemUsageDoesNotExposePaths(t *testing.T) {
	total, free, available, err := localFilesystemUsage(t.TempDir())
	if err != nil || total <= 0 || free <= 0 || available <= 0 || available > free || free > total {
		t.Fatalf("usage = total %d free %d available %d err %v", total, free, available, err)
	}
}
