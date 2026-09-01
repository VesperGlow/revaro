package server

import (
	"context"
	"database/sql"
	"encoding/json"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
)

// TaskManager owns durable lifecycle mutations and shared resource admission.
// Feature adapters only execute domain work and report phases through it.
type TaskManager struct {
	db        *sql.DB
	events    *JobManager
	resources *ResourceGovernor
}

func newTaskManager(db *sql.DB, events *JobManager, resources *ResourceGovernor) *TaskManager {
	return &TaskManager{db: db, events: events, resources: resources}
}

func (m *TaskManager) Ensure(ctx context.Context, taskType, sourceType, sourceID, phase string) (string, error) {
	now := time.Now().UTC().Format(time.RFC3339Nano)
	id := ids.New()
	_, err := m.db.ExecContext(ctx, `INSERT INTO tasks(id,type,status,phase,source_type,source_id,created_at,updated_at) VALUES(?,?,'queued',?,?,?,?,?) ON CONFLICT DO NOTHING`, id, taskType, phase, sourceType, sourceID, now, now)
	if err != nil {
		return "", err
	}
	err = m.db.QueryRowContext(ctx, `SELECT id FROM tasks WHERE source_type=? AND source_id=?`, sourceType, sourceID).Scan(&id)
	return id, err
}
func (m *TaskManager) Update(ctx context.Context, sourceType, sourceID, status, phase string, progress float64, taskErr string) {
	now := time.Now().UTC().Format(time.RFC3339Nano)
	_, _ = m.db.ExecContext(ctx, `UPDATE tasks SET status=?,phase=?,progress=?,error=?,started_at=CASE WHEN ?='running' THEN COALESCE(started_at,?) ELSE started_at END,finished_at=CASE WHEN ? IN ('completed','failed','cancelled') THEN ? ELSE NULL END,heartbeat_at=CASE WHEN ?='running' THEN ? ELSE heartbeat_at END,updated_at=? WHERE source_type=? AND source_id=?`, status, phase, progress, taskErr, status, now, status, now, status, now, now, sourceType, sourceID)
	m.events.Changed()
}
func (m *TaskManager) Create(ctx context.Context, id, taskType, phase, sourceType, sourceID string, payload any) error {
	raw, err := json.Marshal(payload)
	if err != nil {
		return err
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	_, err = m.db.ExecContext(ctx, `INSERT INTO tasks(id,type,status,phase,source_type,source_id,payload_json,created_at,updated_at) VALUES(?,?,'queued',?,?,?,?,?,?) ON CONFLICT DO UPDATE SET payload_json=excluded.payload_json,updated_at=excluded.updated_at`, id, taskType, phase, sourceType, sourceID, string(raw), now, now)
	return err
}
func (m *TaskManager) UpdateID(ctx context.Context, id, status, phase string, progress float64, taskErr string) {
	now := time.Now().UTC().Format(time.RFC3339Nano)
	_, _ = m.db.ExecContext(ctx, `UPDATE tasks SET status=?,phase=?,progress=?,error=?,started_at=CASE WHEN ?='running' THEN COALESCE(started_at,?) ELSE started_at END,finished_at=CASE WHEN ? IN ('completed','failed','cancelled') THEN ? ELSE NULL END,heartbeat_at=CASE WHEN ?='running' THEN ? ELSE heartbeat_at END,updated_at=? WHERE id=?`, status, phase, progress, taskErr, status, now, status, now, status, now, now, id)
	m.events.Changed()
}
func (m *TaskManager) Heavy(ctx context.Context) (func(), error) { return m.resources.Heavy(ctx) }
func (m *TaskManager) IO(ctx context.Context) (func(), error)    { return m.resources.IO(ctx) }
