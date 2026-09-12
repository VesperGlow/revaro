package server

import (
	"context"
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
	if status.Status != "ok" || status.Database.Status != "ok" || status.Database.Bytes <= 0 || status.Storage.Status != "ok" {
		t.Fatalf("unexpected status: %+v", status)
	}
	if status.Storage.Bytes != 0 || status.Storage.TrashBytes != 0 || status.Storage.FileCount != 0 {
		t.Fatalf("unexpected counters: %+v", status)
	}
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(response.Body.Bytes(), &fields); err != nil {
		t.Fatal(err)
	}
	for _, removed := range []string{"backup", "tasks", "object_cleanup"} {
		if _, exists := fields[removed]; exists {
			t.Fatalf("removed component %s is exposed in status", removed)
		}
	}
}

func TestSystemStorageUsageCountsReadyFilesIncludingTrash(t *testing.T) {
	a, _ := localTestApp(t)
	a.srv.cleanup.Close()
	f := uploadLocalFile(t, a, "usage.txt")
	copyResponse := a.request("POST", "/api/files/"+f.ID+"/copy", map[string]any{"parent_id": RootID}, true)
	if copyResponse.Code != 201 {
		t.Fatal(copyResponse.Body.String())
	}
	copy := decode[File](t, copyResponse)
	// Unfinished uploads reserve metadata, not occupied file space.
	a.createUpload(t, "pending.txt", 1000)
	check := func(bytes, trash, count int64) {
		t.Helper()
		usage := a.srv.collectSystemStatus(context.Background()).Storage
		if usage.Bytes != bytes || usage.TrashBytes != trash || usage.FileCount != count {
			t.Fatalf("usage=%+v, want bytes=%d trash=%d count=%d", usage, bytes, trash, count)
		}
	}
	check(14, 0, 2)
	a.request("DELETE", "/api/files/"+f.ID, nil, true)
	check(14, 7, 2)
	a.request("DELETE", "/api/trash", nil, true)
	check(7, 0, 1)
	a.request("DELETE", "/api/files/"+copy.ID, nil, true)
	a.request("DELETE", "/api/trash", nil, true)
	check(0, 0, 0)
}
