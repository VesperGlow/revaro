package server

import (
	"context"
	"errors"
	"net/http"
	"path/filepath"
	"strings"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
)

func (s *Server) createAudioMerge(w http.ResponseWriter, r *http.Request) {
	var in struct {
		ParentID  string   `json:"parent_id"`
		Name      string   `json:"name"`
		FileIDs   []string `json:"file_ids"`
		Format    string   `json:"format"`
		CoverJPEG string   `json:"cover_jpeg"`
		CoverFile string   `json:"cover_file_id"`
	}
	if decodeJSON(w, r, &in) != nil {
		return
	}
	if len(in.FileIDs) < 2 || len(in.FileIDs) > maxAudioMergeInputs {
		problem(w, http.StatusBadRequest, "select between 2 and 256 audio files")
		return
	}
	profile, ok := audioOutput(in.Format)
	if !ok {
		problem(w, http.StatusBadRequest, "audio merge format must be flac, alac, or aac")
		return
	}
	if filepath.Ext(in.Name) == "" {
		in.Name += profile.Extension
	}
	if err := validateName(in.Name); err != nil {
		problem(w, http.StatusBadRequest, err.Error())
		return
	}
	if !strings.EqualFold(filepath.Ext(in.Name), profile.Extension) {
		problem(w, http.StatusBadRequest, "merged audio filename does not match the selected format")
		return
	}
	parent, err := s.file(r.Context(), in.ParentID)
	if err != nil || parent.Kind != "directory" || parent.Status != "ready" {
		problem(w, http.StatusBadRequest, "parent directory is invalid")
		return
	}
	seen := make(map[string]struct{}, len(in.FileIDs))
	inputs := make([]File, 0, len(in.FileIDs))
	var totalSize int64
	for _, id := range in.FileIDs {
		if _, duplicate := seen[id]; duplicate {
			problem(w, http.StatusBadRequest, "an audio file was selected more than once")
			return
		}
		seen[id] = struct{}{}
		f, err := s.file(r.Context(), id)
		if err != nil || !isAudioSource(f) {
			problem(w, http.StatusUnsupportedMediaType, "every selected item must be a ready audio file")
			return
		}
		if f.Size > maxLogicalFileSize-totalSize {
			problem(w, http.StatusRequestEntityTooLarge, "selected audio is too large")
			return
		}
		totalSize += f.Size
		inputs = append(inputs, f)
	}
	if in.CoverJPEG != "" && in.CoverFile != "" {
		problem(w, http.StatusBadRequest, "choose either an uploaded cover or a directory image")
		return
	}
	cover, err := decodeAudioCover(in.CoverJPEG)
	if err == nil && in.CoverFile != "" {
		cover, err = s.audioCoverFromFile(r.Context(), in.ParentID, in.CoverFile)
	}
	if err != nil {
		problem(w, http.StatusBadRequest, err.Error())
		return
	}
	subtitles, err := s.findAudioSubtitles(r.Context(), inputs)
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not match audio subtitles")
		return
	}

	now := time.Now().UTC().Format(time.RFC3339Nano)
	outputID := ids.New()
	_, err = s.db.ExecContext(r.Context(), `INSERT INTO files(id,parent_id,name,kind,size,mime_type,status,created_at,updated_at) VALUES(?,?,?,?,0,?,'pending',?,?)`, outputID, in.ParentID, in.Name, "file", profile.MimeType, now, now)
	if isConflict(err) {
		problem(w, http.StatusConflict, "an item with that name already exists")
		return
	}
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not reserve merged audio")
		return
	}

	ctx, cancel := context.WithTimeout(s.audioHLSCtx, audioMergeTimeout)
	job := &audioMergeJob{
		ID: ids.New(), Status: "queued", Progress: 1, Message: "等待合并任务开始",
		OutputName: in.Name, OutputFormat: profile.Format, OutputFileID: outputID, ParentID: in.ParentID, InputCount: len(inputs),
		Source: "revaro", CreatedAt: now, UpdatedAt: now, cancel: cancel,
	}
	inputIDs := make([]string, len(inputs))
	for i := range inputs {
		inputIDs[i] = inputs[i].ID
	}
	if err := s.createPersistentTask(r.Context(), job.ID, "audio_merge", "queued", "audio_merge", job.ID, map[string]any{"input_ids": inputIDs, "output_file_id": outputID, "parent_id": in.ParentID, "output_name": in.Name, "format": profile.Format, "cover": cover}); err != nil {
		cancel()
		_, _ = s.db.ExecContext(r.Context(), `DELETE FROM files WHERE id=?`, outputID)
		problem(w, 500, "could not persist audio merge task")
		return
	}
	job.changed = func() { s.persistAudioTask(job) }
	for _, input := range inputs {
		_, _ = s.db.ExecContext(r.Context(), `INSERT OR IGNORE INTO task_files(task_id,file_id,role) VALUES(?,?,'input')`, job.ID, input.ID)
	}
	_, _ = s.db.ExecContext(r.Context(), `INSERT OR IGNORE INTO task_files(task_id,file_id,role) VALUES(?,?,'output')`, job.ID, outputID)
	s.audioMergeMu.Lock()
	s.audioMergeJobs[job.ID] = job
	s.audioMergeMu.Unlock()
	if !s.runBackground(func() { s.runAudioMerge(ctx, job, inputs, subtitles, profile, cover) }) {
		cancel()
		s.audioMergeFinished(job, context.Canceled, profile, len(inputs))
		s.audioMergeMu.Lock()
		delete(s.audioMergeJobs, job.ID)
		s.audioMergeMu.Unlock()
		problem(w, http.StatusServiceUnavailable, "service is shutting down")
		return
	}
	writeJSON(w, http.StatusAccepted, job.snapshot())
}

func (s *Server) runAudioMerge(ctx context.Context, job *audioMergeJob, inputs []File, subtitles []*File, profile audioOutputProfile, cover []byte) {
	defer job.cancel()
	err := s.executeAudioMerge(ctx, job, inputs, subtitles, profile, cover)
	s.audioMergeFinished(job, err, profile, len(inputs))
}

// audioMergeFinished records the outcome of a merge, removes the pending
// output placeholder on failure, releases shared resources and schedules the
// terminal job removal. Shared by object-storage and local-directory merges.
func (s *Server) audioMergeFinished(job *audioMergeJob, err error, profile audioOutputProfile, inputCount int) {
	if err == nil {
		job.finish("done", "合并完成", "")
		s.log.Info("audio merge completed", "job", job.ID, "file", job.OutputFileID, "format", profile.Format, "inputs", inputCount)
	} else {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		_, _ = s.db.ExecContext(cleanupCtx, `DELETE FROM files WHERE id=? AND status='pending'`, job.OutputFileID)
		cancel()
		if errors.Is(err, context.Canceled) {
			job.finish("cancelled", "合并已取消", "")
		} else if errors.Is(err, context.DeadlineExceeded) {
			job.finish("failed", "合并失败", "合并任务运行时间超过 24 小时")
		} else {
			var userError audioMergeUserError
			if errors.As(err, &userError) {
				job.finish("failed", "合并失败", userError.Error())
			} else {
				job.finish("failed", "合并失败", "音频格式不兼容、文件损坏或临时空间不足")
			}
		}
		s.log.Error("audio merge failed", "job", job.ID, "output", job.OutputName, "error", err)
	}
	job.cleanupStaging(s.log)
	job.releaseUploadSlot(s)
	s.scheduleAudioMergeRemoval(job, 6*time.Hour)
}

func (s *Server) scheduleAudioMergeRemoval(job *audioMergeJob, after time.Duration) bool {
	return s.runBackground(func() {
		timer := time.NewTimer(after)
		defer timer.Stop()
		select {
		case <-s.audioHLSCtx.Done():
			return
		case <-timer.C:
		}
		s.audioMergeMu.Lock()
		delete(s.audioMergeJobs, job.ID)
		s.audioMergeMu.Unlock()
		job.cleanupStaging(s.log)
		job.releaseUploadSlot(s)
	})
}
