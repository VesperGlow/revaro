package server

import (
	"context"
	"encoding/json"
	"errors"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
)

type archivedTaskPayload struct {
	FileID   string `json:"file_id"`
	ParentID string `json:"parent_id"`
}

func (s *Server) restorePersistentTasks() {
	rows, err := s.db.Query(`SELECT id,type,status,payload_json,created_at,updated_at FROM tasks WHERE type='archive_extract' AND status IN ('queued','running','retrying','waiting_input') ORDER BY created_at`)
	if err != nil {
		return
	}
	type saved struct{ id, kind, status, payload, created, updated string }
	all := []saved{}
	for rows.Next() {
		var v saved
		if rows.Scan(&v.id, &v.kind, &v.status, &v.payload, &v.created, &v.updated) == nil {
			all = append(all, v)
		}
	}
	rows.Close()
	for _, item := range all {
		s.restoreArchiveTask(item.id, item.status, item.payload, item.created, item.updated)
	}
}

func (s *Server) restoreArchiveTask(id, status, raw, created, updated string) {
	var p archivedTaskPayload
	if json.Unmarshal([]byte(raw), &p) != nil {
		return
	}
	s.archiveMu.Lock()
	delete(s.archiveJobs, id)
	s.archiveMu.Unlock()
	f, err := s.readableFile(context.Background(), p.FileID)
	if err != nil {
		s.updateTask(context.Background(), "archive", id, JobFailed, "recovery", 0, "archive source is unavailable")
		return
	}
	job := &archiveJob{ID: id, FileID: p.FileID, ParentID: p.ParentID, Name: f.Name, Status: "queued", Message: "服务重启后恢复", CreatedAt: created, UpdatedAt: updated}
	job.changed = func() { s.persistArchiveTask(job) }
	s.archiveMu.Lock()
	s.archiveJobs[id] = job
	s.archiveMu.Unlock()
	if status == "waiting_input" {
		job.Status = "waiting_password"
		job.Message = "压缩包已加密，请重新输入密码"
		job.passwordDeadline = time.Now().Add(archivePasswordWaitTTL)
		s.persistArchiveTask(job)
		return
	}
	ctx, cancel := context.WithCancel(s.workCtx)
	job.cancel = cancel
	s.runBackground(func() { s.runArchiveExtract(ctx, f, p.ParentID, job, "") })
}

func (s *Server) startRuntimeTask(ctx context.Context, id, taskType, sourceType, fileID string) string {
	if id == "" {
		id = ids.New()
	}
	if s.createPersistentTask(ctx, id, taskType, "starting", sourceType, id, map[string]any{"file_id": fileID}) != nil {
		return ""
	}
	if fileID != "" {
		_, _ = s.db.ExecContext(ctx, `INSERT OR IGNORE INTO task_files(task_id,file_id,role) VALUES(?,?,'input')`, id, fileID)
	}
	s.updateTask(ctx, sourceType, id, JobRunning, "running", 0, "")
	return id
}
func (s *Server) finishRuntimeTask(id, sourceType string, err error) {
	if id == "" {
		return
	}
	if errors.Is(err, context.Canceled) {
		s.updateTask(context.Background(), sourceType, id, JobCancelled, "cancelled", 0, "")
	} else if err != nil {
		s.log.Error("runtime task failed", "task", id, "type", sourceType, "error", err)
		s.updateTask(context.Background(), sourceType, id, JobFailed, "failed", 0, publicError(err, "媒体处理失败，请稍后重试"))
	} else {
		s.updateTask(context.Background(), sourceType, id, JobCompleted, "completed", 100, "")
	}
}

func taskStatusForArchive(status string) string {
	switch status {
	case "queued":
		return JobQueued
	case "waiting_password":
		return "waiting_input"
	case "done":
		return JobCompleted
	case "failed":
		return JobFailed
	case "cancelled":
		return JobCancelled
	default:
		return JobRunning
	}
}

func (s *Server) createPersistentTask(ctx context.Context, id, taskType, phase, sourceType, sourceID string, payload any) error {
	return s.tasks.Create(ctx, id, taskType, phase, sourceType, sourceID, payload)
}

func (s *Server) persistArchiveTask(job *archiveJob) {
	snapshot := job.snapshot()
	status := taskStatusForArchive(snapshot.Status)
	s.tasks.UpdateID(context.Background(), snapshot.ID, status, snapshot.Status, float64(snapshot.Progress), snapshot.Error)
}
