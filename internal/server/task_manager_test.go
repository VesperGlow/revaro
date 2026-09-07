package server

import (
	"context"
	"testing"
	"time"
)

func TestTaskManagerUpdateSelectorsShareLifecycleSemantics(t *testing.T) {
	app := newTestApp(t)
	ctx := context.Background()
	for _, task := range []struct{ id, sourceID string }{{"task-by-source", "source-a"}, {"task-by-id", "source-b"}} {
		if err := app.srv.tasks.Create(ctx, task.id, "test", "queued", "test-source", task.sourceID, nil); err != nil {
			t.Fatal(err)
		}
	}
	app.srv.tasks.Update(ctx, "test-source", "source-a", "running", "working", 25, "")
	app.srv.tasks.UpdateID(ctx, "task-by-id", "running", "working", 25, "")

	for _, id := range []string{"task-by-source", "task-by-id"} {
		var status, phase, started, heartbeat string
		var progress float64
		if err := app.db.QueryRow(`SELECT status,phase,progress,COALESCE(started_at,''),COALESCE(heartbeat_at,'') FROM tasks WHERE id=?`, id).Scan(&status, &phase, &progress, &started, &heartbeat); err != nil {
			t.Fatal(err)
		}
		if status != "running" || phase != "working" || progress != 25 || started == "" || heartbeat == "" {
			t.Fatalf("running lifecycle for %s: status=%q phase=%q progress=%v started=%q heartbeat=%q", id, status, phase, progress, started, heartbeat)
		}
	}

	app.srv.tasks.Update(ctx, "test-source", "source-a", "completed", "done", 100, "")
	app.srv.tasks.UpdateID(ctx, "task-by-id", "completed", "done", 100, "")
	for _, id := range []string{"task-by-source", "task-by-id"} {
		var status, finished string
		if err := app.db.QueryRow(`SELECT status,COALESCE(finished_at,'') FROM tasks WHERE id=?`, id).Scan(&status, &finished); err != nil {
			t.Fatal(err)
		}
		if status != "completed" || finished == "" {
			t.Fatalf("terminal lifecycle for %s: status=%q finished=%q", id, status, finished)
		}
	}
}

func TestURLTaskRetryHandlesDatabaseFailure(t *testing.T) {
	a := newTestApp(t)
	now := time.Now().UTC().Format(time.RFC3339Nano)
	if _, err := a.db.Exec(`INSERT INTO url_download_jobs(id,parent_id,source_url,name,status,created_at,updated_at) VALUES('retry-error',?,'https://example.test/file','file','failed',?,?)`, RootID, now, now); err != nil {
		t.Fatal(err)
	}
	// Project a failed source through the existing update trigger.
	if _, err := a.db.Exec(`UPDATE url_download_jobs SET status='failed' WHERE id='retry-error'`); err != nil {
		t.Fatal(err)
	}
	if _, err := a.db.Exec(`CREATE TRIGGER reject_url_retry BEFORE UPDATE ON url_download_jobs BEGIN SELECT RAISE(ABORT,'injected write failure'); END`); err != nil {
		t.Fatal(err)
	}
	a.srv.downloads = &downloadManager{}
	defer func() { a.srv.downloads = nil }()
	rr := a.request("POST", "/api/tasks/retry-error/retry", nil, true)
	if rr.Code != 409 {
		t.Fatalf("retry=%d: %s", rr.Code, rr.Body.String())
	}
}
