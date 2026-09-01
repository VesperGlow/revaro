package server

import (
	"context"
	"database/sql"
	"net/http"
	"time"

	"github.com/go-chi/chi/v5"
)

type Task struct {
	ID              string  `json:"id"`
	Type            string  `json:"type"`
	Status          string  `json:"status"`
	Phase           string  `json:"phase"`
	Progress        float64 `json:"progress"`
	Speed           int64   `json:"speed"`
	ETASeconds      *int64  `json:"eta_seconds,omitempty"`
	RetryCount      int     `json:"retry_count"`
	MaxRetries      int     `json:"max_retries"`
	Error           string  `json:"error,omitempty"`
	SourceType      string  `json:"source_type,omitempty"`
	SourceID        string  `json:"source_id,omitempty"`
	CancelRequested bool    `json:"cancel_requested"`
	CreatedAt       string  `json:"created_at"`
	StartedAt       string  `json:"started_at,omitempty"`
	FinishedAt      string  `json:"finished_at,omitempty"`
	UpdatedAt       string  `json:"updated_at"`
	Name            string  `json:"name"`
}

func (s *Server) ensureTask(ctx context.Context, taskType, sourceType, sourceID, phase string) (string, error) {
	return s.tasks.Ensure(ctx, taskType, sourceType, sourceID, phase)
}

func (s *Server) updateTask(ctx context.Context, sourceType, sourceID, status, phase string, progress float64, taskErr string) {
	s.tasks.Update(ctx, sourceType, sourceID, status, phase, progress, taskErr)
}

func scanTask(row interface{ Scan(...any) error }) (Task, error) {
	var t Task
	var eta sql.NullInt64
	var sourceType, sourceID, started, finished sql.NullString
	err := row.Scan(&t.ID, &t.Type, &t.Status, &t.Phase, &t.Progress, &t.Speed, &eta, &t.RetryCount, &t.MaxRetries, &t.Error, &sourceType, &sourceID, &t.CancelRequested, &t.CreatedAt, &started, &finished, &t.UpdatedAt, &t.Name)
	if eta.Valid {
		t.ETASeconds = &eta.Int64
	}
	t.SourceType = sourceType.String
	t.SourceID = sourceID.String
	t.StartedAt = started.String
	t.FinishedAt = finished.String
	return t, err
}

const taskSelect = `SELECT tasks.id,tasks.type,tasks.status,tasks.phase,tasks.progress,tasks.speed,tasks.eta_seconds,tasks.retry_count,tasks.max_retries,tasks.error,tasks.source_type,tasks.source_id,tasks.cancel_requested,tasks.created_at,tasks.started_at,tasks.finished_at,tasks.updated_at,COALESCE((SELECT files.name FROM task_files JOIN files ON files.id=task_files.file_id WHERE task_files.task_id=tasks.id AND task_files.role='output' LIMIT 1),(SELECT download_jobs.name FROM download_jobs WHERE download_jobs.id=tasks.source_id),(SELECT url_download_jobs.name FROM url_download_jobs WHERE url_download_jobs.id=tasks.source_id),(SELECT files.name FROM task_files JOIN files ON files.id=task_files.file_id WHERE task_files.task_id=tasks.id AND task_files.role='input' LIMIT 1),tasks.type) FROM tasks`

func (s *Server) listTasks(w http.ResponseWriter, r *http.Request) {
	// Terminal successes are notification-like UI records. Keep durable rows for
	// history/recovery, but stop returning stale completed/cancelled items.
	rows, err := s.db.QueryContext(r.Context(), taskSelect+` WHERE tasks.status NOT IN ('completed','cancelled') OR julianday(tasks.finished_at) >= julianday('now','-30 minutes') ORDER BY tasks.created_at DESC LIMIT 500`)
	if err != nil {
		problem(w, 500, "could not list tasks")
		return
	}
	defer rows.Close()
	items := []Task{}
	for rows.Next() {
		t, e := scanTask(rows)
		if e != nil {
			problem(w, 500, "could not list tasks")
			return
		}
		items = append(items, t)
	}
	writeJSON(w, 200, map[string]any{"items": items})
}

func (s *Server) getTask(w http.ResponseWriter, r *http.Request) {
	t, err := scanTask(s.db.QueryRowContext(r.Context(), taskSelect+` WHERE tasks.id=?`, chi.URLParam(r, "id")))
	if err != nil {
		problem(w, 404, "task not found")
		return
	}
	writeJSON(w, 200, t)
}

func (s *Server) cancelTask(w http.ResponseWriter, r *http.Request) {
	task, lookupErr := scanTask(s.db.QueryRowContext(r.Context(), taskSelect+` WHERE tasks.id=?`, chi.URLParam(r, "id")))
	if lookupErr != nil {
		problem(w, 404, "task not found")
		return
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	res, err := s.db.ExecContext(r.Context(), `UPDATE tasks SET cancel_requested=1,status=CASE WHEN status IN ('queued','retrying','waiting_input') THEN 'cancelled' ELSE status END,finished_at=CASE WHEN status IN ('queued','retrying','waiting_input') THEN ? ELSE finished_at END,updated_at=? WHERE id=? AND status NOT IN ('completed','failed','cancelled')`, now, now, chi.URLParam(r, "id"))
	if err != nil {
		problem(w, 500, "could not cancel task")
		return
	}
	n, _ := res.RowsAffected()
	if n == 0 {
		problem(w, 409, "task is already finished")
		return
	}
	s.cancelTaskRuntime(r.Context(), task)
	s.jobs.Changed()
	w.WriteHeader(204)
}

func (s *Server) retryTask(w http.ResponseWriter, r *http.Request) {
	task, lookupErr := scanTask(s.db.QueryRowContext(r.Context(), taskSelect+` WHERE tasks.id=?`, chi.URLParam(r, "id")))
	if lookupErr != nil {
		problem(w, 404, "task not found")
		return
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	res, err := s.db.ExecContext(r.Context(), `UPDATE tasks SET status='retrying',phase='queued',retry_count=retry_count+1,error='',cancel_requested=0,started_at=NULL,finished_at=NULL,updated_at=? WHERE id=? AND status='failed' AND retry_count<max_retries`, now, chi.URLParam(r, "id"))
	if err != nil {
		problem(w, 500, "could not retry task")
		return
	}
	n, _ := res.RowsAffected()
	if n == 0 {
		problem(w, 409, "task cannot be retried")
		return
	}
	if s.downloads != nil && task.SourceType == "download" {
		if err := s.downloads.resume(r.Context(), task.SourceID); err != nil {
			s.log.Error("download retry failed", "task", task.ID, "error", err)
			s.updateTask(r.Context(), task.SourceType, task.SourceID, "failed", "retrying", task.Progress, publicError(err, "下载重试失败，请稍后再试"))
			problem(w, 409, "download task could not be retried")
			return
		}
	}
	if s.downloads != nil && task.SourceType == "url_download" {
		res, err := s.db.ExecContext(r.Context(), `UPDATE url_download_jobs SET status='queued',completed_size=0,download_speed=0,error='',updated_at=? WHERE id=? AND status='failed'`, now, task.SourceID)
		changed, _ := res.RowsAffected()
		if err != nil || changed != 1 {
			s.updateTask(r.Context(), task.SourceType, task.SourceID, "failed", "retrying", task.Progress, "download task could not be retried")
			problem(w, 409, "download task could not be retried")
			return
		}
		s.downloads.startURLDownload(task.SourceID)
	}
	if task.SourceType == "archive" || task.SourceType == "audio_merge" {
		var raw, created, updated string
		if s.db.QueryRowContext(r.Context(), `SELECT payload_json,created_at,updated_at FROM tasks WHERE id=?`, task.ID).Scan(&raw, &created, &updated) == nil {
			if task.SourceType == "archive" {
				s.restoreArchiveTask(task.ID, "retrying", raw, created, updated)
			} else {
				s.restoreAudioTask(task.ID, "retrying", raw, created, updated)
			}
		}
	}
	s.jobs.Changed()
	w.WriteHeader(202)
}

func (s *Server) cancelTaskRuntime(ctx context.Context, task Task) {
	switch task.SourceType {
	case "upload":
		u, err := s.upload(ctx, task.SourceID)
		if err == nil && u.Status == "pending" {
			_ = s.cleanupPendingUploadObject(ctx, u)
			tx, txErr := s.db.BeginTx(ctx, nil)
			if txErr == nil {
				_, txErr = tx.ExecContext(ctx, `UPDATE uploads SET status='aborted' WHERE id=?`, u.ID)
				if txErr == nil {
					_, txErr = tx.ExecContext(ctx, `DELETE FROM files WHERE id=? AND status='pending'`, u.FileID)
				}
				if txErr == nil {
					_ = tx.Commit()
				} else {
					_ = tx.Rollback()
				}
			}
		}
	case "archive":
		s.archiveMu.RLock()
		job := s.archiveJobs[task.SourceID]
		s.archiveMu.RUnlock()
		if job != nil {
			snapshot := job.snapshot()
			if extractor, ok := s.objects.Archive(); ok {
				_ = extractor.CancelArchive(ctx, job.ID)
			}
			if job.cancel != nil {
				job.cancel()
			}
			if snapshot.Status == "waiting_password" {
				job.mu.Lock()
				job.Status, job.Message, job.Error = "cancelled", "已取消", ""
				job.passwordDeadline = time.Time{}
				job.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
				job.mu.Unlock()
				s.cleanupArchiveJobStaging(job)
			}
		}
	case "audio_merge":
		s.audioMergeMu.RLock()
		job := s.audioMergeJobs[task.SourceID]
		s.audioMergeMu.RUnlock()
		if job != nil {
			snapshot := job.snapshot()
			if job.localUpload && snapshot.Status == "uploading" {
				s.audioMergeMu.Lock()
				delete(s.audioMergeJobs, job.ID)
				s.audioMergeMu.Unlock()
				job.cleanupStaging(s.log)
				job.releaseUploadSlot(s)
				_, _ = s.db.ExecContext(ctx, `DELETE FROM files WHERE id=? AND status='pending'`, job.OutputFileID)
				job.finish("cancelled", "合并已取消", "")
			} else if job.cancel != nil {
				job.cancel()
			}
		}
	case "video_hls":
		s.removeVideoHLSSession(task.SourceID)
	case "audio_hls":
		s.removeAudioHLSSession(task.SourceID)
	case "video_fmp4":
		s.removeVideoFMP4Session(task.SourceID)
	case "download":
		if s.downloads != nil {
			s.downloads.mu.Lock()
			runtime := s.downloads.jobs[task.SourceID]
			delete(s.downloads.jobs, task.SourceID)
			s.downloads.mu.Unlock()
			if runtime != nil {
				runtime.cancel()
				job, _ := s.downloads.get(context.Background(), task.SourceID, true)
				s.downloads.cleanupTorrentImport(s.downloads.importRequests(job))
				cleanupCtx, cancel := context.WithTimeout(context.Background(), time.Minute)
				_ = s.downloads.bt.DeleteTorrent(cleanupCtx, runtime.torrentID)
				cancel()
			}
			_, _ = s.db.ExecContext(ctx, `UPDATE download_jobs SET status='cancelled',ingest_state='cancelled',download_speed=0,import_speed=0,peers=0,updated_at=? WHERE id=?`, time.Now().UTC().Format(time.RFC3339Nano), task.SourceID)
		}
	case "url_download":
		if s.downloads != nil {
			s.downloads.urlMu.Lock()
			runtime := s.downloads.urlJobs[task.SourceID]
			delete(s.downloads.urlJobs, task.SourceID)
			s.downloads.urlMu.Unlock()
			if runtime != nil {
				runtime.cancel()
			}
			_, _ = s.db.ExecContext(ctx, `UPDATE url_download_jobs SET status='cancelled',download_speed=0,updated_at=? WHERE id=?`, time.Now().UTC().Format(time.RFC3339Nano), task.SourceID)
		}
	}
}

func (s *Server) taskInput(w http.ResponseWriter, r *http.Request) {
	task, err := scanTask(s.db.QueryRowContext(r.Context(), taskSelect+` WHERE tasks.id=?`, chi.URLParam(r, "id")))
	if err != nil || task.Status != "waiting_input" {
		problem(w, 409, "task is not waiting for input")
		return
	}
	if task.SourceType != "archive" {
		problem(w, 400, "task does not accept input")
		return
	}
	var in struct {
		Password string `json:"password"`
	}
	if decodeJSON(w, r, &in) != nil {
		return
	}
	if in.Password == "" || len(in.Password) > 1024 {
		problem(w, 400, "archive password is required")
		return
	}
	s.archiveMu.RLock()
	job := s.archiveJobs[task.SourceID]
	s.archiveMu.RUnlock()
	if job == nil || !job.resumeWithPassword() {
		problem(w, 409, "archive task cannot continue")
		return
	}
	f, fileErr := s.readableFile(r.Context(), job.FileID)
	if fileErr != nil {
		s.failArchiveJob(job, fileErr)
		problem(w, 409, "archive source is unavailable")
		return
	}
	ctx, cancel := context.WithCancel(s.audioHLSCtx)
	job.cancel = cancel
	if !s.runBackground(func() { s.runArchiveExtract(ctx, f, job.ParentID, job, in.Password) }) {
		cancel()
		problem(w, 503, "service is shutting down")
		return
	}
	writeJSON(w, 202, job.snapshot())
}

func (s *Server) deleteTask(w http.ResponseWriter, r *http.Request) {
	task, err := scanTask(s.db.QueryRowContext(r.Context(), taskSelect+` WHERE tasks.id=?`, chi.URLParam(r, "id")))
	if err != nil {
		problem(w, 404, "task not found")
		return
	}
	if task.Status != JobCompleted && task.Status != JobFailed && task.Status != JobCancelled {
		problem(w, 409, "active task cannot be removed")
		return
	}
	if s.downloads != nil && (task.SourceType == "download" || task.SourceType == "url_download") {
		if err := s.downloads.removeAny(r.Context(), task.SourceID); err != nil {
			problem(w, 500, "could not remove download task")
			return
		}
	} else {
		if task.SourceType == "archive" {
			s.archiveMu.Lock()
			job := s.archiveJobs[task.SourceID]
			delete(s.archiveJobs, task.SourceID)
			s.archiveMu.Unlock()
			if job != nil {
				s.cleanupArchiveJobStaging(job)
			}
		}
		if task.SourceType == "audio_merge" {
			s.audioMergeMu.Lock()
			job := s.audioMergeJobs[task.SourceID]
			delete(s.audioMergeJobs, task.SourceID)
			s.audioMergeMu.Unlock()
			if job != nil {
				job.cleanupStaging(s.log)
				job.releaseUploadSlot(s)
			}
		}
		_, err = s.db.ExecContext(r.Context(), `DELETE FROM tasks WHERE id=?`, task.ID)
		if err != nil {
			problem(w, 500, "could not remove task")
			return
		}
	}
	s.jobs.Changed()
	w.WriteHeader(204)
}

func (s *Server) RecoverTasks(ctx context.Context) {
	now := time.Now().UTC().Format(time.RFC3339Nano)
	_, _ = s.db.ExecContext(ctx, `UPDATE tasks SET status='cancelled',phase='service_restarted',error='',finished_at=?,updated_at=? WHERE status='running' AND type IN ('video_hls','audio_hls','video_fmp4','subtitle')`, now, now)
	_, _ = s.db.ExecContext(ctx, `UPDATE tasks SET status='retrying',phase='recovered',retry_count=retry_count+1,error='recovered after service restart',started_at=NULL,heartbeat_at=NULL,updated_at=? WHERE status='running' AND retry_count<max_retries`, now)
	_, _ = s.db.ExecContext(ctx, `UPDATE tasks SET status='failed',error='retry limit reached during restart recovery',finished_at=?,updated_at=? WHERE status='running'`, now, now)
}
