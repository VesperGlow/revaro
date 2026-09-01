package server

import (
	"context"
	"net/http"
	"time"

	"github.com/go-chi/chi/v5"
)

func (s *Server) completeLocalAudioMerge(w http.ResponseWriter, r *http.Request) {
	s.audioMergeMu.RLock()
	job := s.audioMergeJobs[chi.URLParam(r, "id")]
	s.audioMergeMu.RUnlock()
	if job == nil || !job.localUpload {
		problem(w, http.StatusNotFound, "local merge job not found")
		return
	}
	job.mu.Lock()
	if job.Status != "uploading" {
		job.mu.Unlock()
		problem(w, http.StatusConflict, "this merge is no longer accepting uploads")
		return
	}
	for fileIndex := range job.files {
		for chunk := 0; chunk < job.files[fileIndex].Chunks; chunk++ {
			if !job.files[fileIndex].chunkDone[chunk] {
				job.mu.Unlock()
				problem(w, http.StatusBadRequest, "还有素材分块没有上传完成")
				return
			}
		}
	}
	var audioBytes int64
	for _, fileIndex := range job.audioOrder {
		audioBytes += job.files[fileIndex].Size
	}
	job.mu.Unlock()

	// Merging roughly doubles the staging footprint (source + ALAC output);
	// require the audio payload to fit alongside the safety margin.
	if err := s.ensureMergeDisk(audioBytes); err != nil {
		s.abortLocalMergeUpload(job, err.Error())
		problem(w, http.StatusInsufficientStorage, err.Error())
		return
	}
	job.mu.Lock()
	job.Status = "queued"
	job.Progress = localMergeUploadProgressEnd
	job.Message = "素材上传完成，等待合并"
	job.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
	job.mu.Unlock()
	job.releaseUploadSlot(s)
	if !s.runBackground(func() { s.runLocalAudioMerge(job) }) {
		s.abortLocalMergeUpload(job, "服务正在关闭")
		problem(w, http.StatusServiceUnavailable, "service is shutting down")
		return
	}
	writeJSON(w, http.StatusOK, job.snapshot())
}

// abortLocalMergeUpload drops an upload-phase job and every trace of it.
func (s *Server) abortLocalMergeUpload(job *audioMergeJob, message string) {
	s.audioMergeMu.Lock()
	delete(s.audioMergeJobs, job.ID)
	s.audioMergeMu.Unlock()
	job.finish("failed", "合并失败", message)
	job.cleanupStaging(s.log)
	job.releaseUploadSlot(s)
	cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	_, _ = s.db.ExecContext(cleanupCtx, `DELETE FROM files WHERE id=? AND status='pending'`, job.OutputFileID)
	cancel()
	s.log.Warn("local merge upload aborted", "job", job.ID, "output", job.OutputName, "error", message)
}

func (s *Server) runLocalAudioMerge(job *audioMergeJob) {
	defer job.cancel()
	profile, _ := audioOutput("alac")
	err := s.executeLocalAudioMerge(job.mergeCtx, job)
	s.audioMergeFinished(job, err, profile, job.InputCount)
}
