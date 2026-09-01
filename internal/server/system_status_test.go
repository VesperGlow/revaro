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
	if status.Status != "ok" || status.Database.Status != "ok" || status.Database.Bytes <= 0 || status.Storage.Status != "ok" {
		t.Fatalf("unexpected status: %+v", status)
	}
	if status.Tasks.Running != 0 || status.ObjectCleanup.Pending != 0 {
		t.Fatalf("unexpected counters: %+v", status)
	}
}
