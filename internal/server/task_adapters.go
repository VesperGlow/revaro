package server

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
)

type archivedTaskPayload struct {
	FileID   string `json:"file_id"`
	ParentID string `json:"parent_id"`
}
type audioTaskPayload struct {
	InputIDs     []string `json:"input_ids"`
	OutputFileID string   `json:"output_file_id"`
	ParentID     string   `json:"parent_id"`
	OutputName   string   `json:"output_name"`
	Format       string   `json:"format"`
	Cover        []byte   `json:"cover"`
	Local        bool     `json:"local"`
	StagingDir   string   `json:"staging_dir"`
	Files        []struct {
		Name      string `json:"name"`
		Size      int64  `json:"size"`
		Kind      string `json:"kind"`
		Chunks    int    `json:"chunks"`
		Uploaded  int64  `json:"uploaded"`
		ChunkDone []bool `json:"chunk_done"`
	} `json:"files"`
	AudioOrder  []int `json:"audio_order"`
	SubtitleFor []int `json:"subtitle_for"`
	CoverIndex  int   `json:"cover_index"`
	InputCount  int   `json:"input_count"`
}

func (s *Server) restorePersistentTasks() {
	rows, err := s.db.Query(`SELECT id,type,status,payload_json,created_at,updated_at FROM tasks WHERE type IN ('archive_extract','audio_merge') AND status IN ('queued','running','retrying','waiting_input') ORDER BY created_at`)
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
		if item.kind == "archive_extract" {
			s.restoreArchiveTask(item.id, item.status, item.payload, item.created, item.updated)
		} else {
			s.restoreAudioTask(item.id, item.status, item.payload, item.created, item.updated)
		}
	}
}

func (s *Server) cleanupUnreferencedLocalMergeDirs() {
	keep := map[string]bool{}
	rows, err := s.db.Query(`SELECT payload_json FROM tasks WHERE type='audio_merge' AND status IN ('queued','running','retrying','waiting_input')`)
	if err == nil {
		for rows.Next() {
			var raw string
			var p audioTaskPayload
			if rows.Scan(&raw) == nil && json.Unmarshal([]byte(raw), &p) == nil && p.Local && p.StagingDir != "" {
				keep[filepath.Clean(p.StagingDir)] = true
			}
		}
		rows.Close()
	}
	dirs, _ := filepath.Glob(filepath.Join(s.cfg.WorkDir, "revaro-local-merge-*"))
	for _, dir := range dirs {
		if !keep[filepath.Clean(dir)] {
			_ = os.RemoveAll(dir)
		}
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
	ctx, cancel := context.WithCancel(s.audioHLSCtx)
	job.cancel = cancel
	s.runBackground(func() { s.runArchiveExtract(ctx, f, p.ParentID, job, "") })
}

func (s *Server) restoreAudioTask(id, status, raw, created, updated string) {
	var p audioTaskPayload
	if json.Unmarshal([]byte(raw), &p) != nil {
		return
	}
	s.audioMergeMu.Lock()
	delete(s.audioMergeJobs, id)
	s.audioMergeMu.Unlock()
	profile, ok := audioOutput(p.Format)
	if p.Local {
		profile, _ = audioOutput("alac")
	} else if !ok {
		s.updateTask(context.Background(), "audio_merge", id, JobFailed, "recovery", 0, "invalid audio profile")
		return
	}
	ctx, cancel := context.WithTimeout(s.audioHLSCtx, audioMergeTimeout)
	if p.OutputFileID != "" {
		var exists int
		_ = s.db.QueryRow(`SELECT COUNT(*) FROM files WHERE id=?`, p.OutputFileID).Scan(&exists)
		if exists == 0 {
			now := time.Now().UTC().Format(time.RFC3339Nano)
			_, _ = s.db.Exec(`INSERT INTO files(id,parent_id,name,kind,size,mime_type,status,created_at,updated_at) VALUES(?,?,?,'file',0,?,'pending',?,?)`, p.OutputFileID, p.ParentID, p.OutputName, profile.MimeType, now, now)
		}
	}
	job := &audioMergeJob{ID: id, Status: "queued", Progress: 1, Message: "服务重启后恢复", OutputName: p.OutputName, OutputFormat: profile.Format, OutputFileID: p.OutputFileID, ParentID: p.ParentID, InputCount: len(p.InputIDs), Source: "revaro", CreatedAt: created, UpdatedAt: updated, cancel: cancel, mergeCtx: ctx}
	job.changed = func() { s.persistAudioTask(job) }
	if p.Local {
		if _, err := os.Stat(p.StagingDir); err != nil {
			cancel()
			s.updateTask(context.Background(), "audio_merge", id, JobFailed, "recovery", 0, "local merge staging is unavailable")
			return
		}
		job.localUpload = true
		job.Source = "local"
		job.stagingDir = p.StagingDir
		job.audioOrder = p.AudioOrder
		job.subtitleFor = p.SubtitleFor
		job.coverIndex = p.CoverIndex
		job.InputCount = p.InputCount
		for _, f := range p.Files {
			job.files = append(job.files, localMergeFile{Name: f.Name, Size: f.Size, Kind: f.Kind, Chunks: f.Chunks, Uploaded: f.Uploaded, chunkDone: f.ChunkDone})
			job.uploadedBytes += f.Uploaded
		}
		if status == "waiting_input" {
			select {
			case s.localMergeJobSlots <- struct{}{}:
				job.uploadSlotHeld = true
			default:
				cancel()
				s.updateTask(context.Background(), "audio_merge", id, JobFailed, "recovery", 0, "local upload capacity unavailable")
				return
			}
		}
	}
	s.audioMergeMu.Lock()
	s.audioMergeJobs[id] = job
	s.audioMergeMu.Unlock()
	if p.Local {
		if status != "waiting_input" {
			s.runBackground(func() { s.runLocalAudioMerge(job) })
		}
		return
	}
	inputs := make([]File, 0, len(p.InputIDs))
	for _, fileID := range p.InputIDs {
		f, err := s.readableFile(context.Background(), fileID)
		if err != nil {
			cancel()
			s.audioMergeFinished(job, errors.New("audio source is unavailable"), profile, len(inputs))
			return
		}
		inputs = append(inputs, f)
	}
	subtitles, err := s.findAudioSubtitles(context.Background(), inputs)
	if err != nil {
		cancel()
		s.audioMergeFinished(job, err, profile, len(inputs))
		return
	}
	s.runBackground(func() { s.runAudioMerge(ctx, job, inputs, subtitles, profile, p.Cover) })
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

func taskStatusForAudio(status string) string {
	switch status {
	case "uploading":
		return "waiting_input"
	case "queued":
		return JobQueued
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

func (s *Server) persistAudioTask(job *audioMergeJob) {
	snapshot := job.snapshot()
	if job.localUpload {
		job.mu.Lock()
		files := make([]map[string]any, len(job.files))
		for i, f := range job.files {
			files[i] = map[string]any{"name": f.Name, "size": f.Size, "kind": f.Kind, "chunks": f.Chunks, "uploaded": f.Uploaded, "chunk_done": f.chunkDone}
		}
		payload := map[string]any{"local": true, "staging_dir": job.stagingDir, "files": files, "audio_order": job.audioOrder, "subtitle_for": job.subtitleFor, "cover_index": job.coverIndex, "output_file_id": job.OutputFileID, "parent_id": job.ParentID, "output_name": job.OutputName, "input_count": job.InputCount}
		job.mu.Unlock()
		raw, _ := json.Marshal(payload)
		_, _ = s.db.Exec(`UPDATE tasks SET payload_json=? WHERE id=?`, string(raw), job.ID)
	}
	status := taskStatusForAudio(snapshot.Status)
	s.tasks.UpdateID(context.Background(), snapshot.ID, status, snapshot.Status, float64(snapshot.Progress), snapshot.Error)
}
