package server

import (
	"context"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"
)

func (s *Server) executeLocalAudioMerge(ctx context.Context, job *audioMergeJob) error {
	release, err := s.tasks.Heavy(ctx)
	if err != nil {
		return err
	}
	defer release()
	select {
	case s.audioMergeSlots <- struct{}{}:
		defer func() { <-s.audioMergeSlots }()
	case <-ctx.Done():
		return ctx.Err()
	}
	job.mu.Lock()
	workDir := job.stagingDir
	job.mu.Unlock()
	job.update("preparing", 50, "正在整理本地素材")
	if err := s.assembleLocalMerge(ctx, job, workDir); err != nil {
		return err
	}
	inputs := make([]File, 0, len(job.audioOrder))
	paths := make([]string, 0, len(job.audioOrder))
	for position, fileIndex := range job.audioOrder {
		if err := ctx.Err(); err != nil {
			return err
		}
		file := job.files[fileIndex]
		ext := strings.ToLower(filepath.Ext(file.Name))
		if !audioSourceExts[ext] {
			ext = ".audio"
		}
		path := filepath.Join(workDir, fmt.Sprintf("input-%04d%s", position, ext))
		inputs = append(inputs, File{Name: file.Name, Size: file.Size, Kind: "file", Status: "ready"})
		paths = append(paths, path)
	}
	cover, err := s.localMergeCover(ctx, job, workDir)
	if err != nil {
		return err
	}
	subtitleSources := make([]*File, len(inputs))
	openLocal := func(_ context.Context, f File) (io.ReadCloser, error) {
		return os.Open(f.objectKey)
	}
	for position, fileIndex := range job.subtitleFor {
		if fileIndex < 0 {
			continue
		}
		file := job.files[fileIndex]
		source := File{Name: file.Name, Size: file.Size, Kind: "file", Status: "ready", objectKey: filepath.Join(workDir, fmt.Sprintf("subtitle-%04d.vtt", position))}
		subtitleSources[position] = &source
	}
	profile, _ := audioOutput("alac")
	return s.encodeMergedAudio(ctx, job, profile, workDir, inputs, paths, subtitleSources, openLocal, cover)
}

func (s *Server) localMergeCover(ctx context.Context, job *audioMergeJob, workDir string) ([]byte, error) {
	if job.coverIndex < 0 {
		return nil, nil
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	raw, err := os.ReadFile(filepath.Join(workDir, "cover.raw"))
	if err != nil {
		return nil, fmt.Errorf("read local cover: %w", err)
	}
	normalized, err := normalizeAudioCover(raw)
	if err != nil {
		return nil, audioMergeUserError{message: fmt.Sprintf("封面「%s」不是有效的图片", job.files[job.coverIndex].Name)}
	}
	return normalized, nil
}

// assembleLocalMerge concatenates the uploaded chunks into the input-* /
// subtitle-* / cover.raw staging files the shared pipeline consumes.
func (s *Server) assembleLocalMerge(ctx context.Context, job *audioMergeJob, workDir string) error {
	chunksDir := filepath.Join(workDir, "chunks")
	var totalBytes int64
	for _, file := range job.files {
		totalBytes += file.Size
	}
	var assembled int64
	for position, fileIndex := range job.audioOrder {
		if err := ctx.Err(); err != nil {
			return err
		}
		file := job.files[fileIndex]
		ext := strings.ToLower(filepath.Ext(file.Name))
		if !audioSourceExts[ext] {
			ext = ".audio"
		}
		dest := filepath.Join(workDir, fmt.Sprintf("input-%04d%s", position, ext))
		n, err := concatLocalMergeChunks(chunksDir, fileIndex, file.Chunks, dest)
		if err != nil {
			return fmt.Errorf("assemble audio %s: %w", file.Name, err)
		}
		if n != file.Size {
			return fmt.Errorf("assemble audio %s: size mismatch: got %d, want %d", file.Name, n, file.Size)
		}
		assembled += n
		if totalBytes > 0 {
			job.update("preparing", 50+int(assembled*8/totalBytes), fmt.Sprintf("正在整理第 %d / %d 段", position+1, len(job.audioOrder)))
		}
	}
	for position, fileIndex := range job.subtitleFor {
		if fileIndex < 0 {
			continue
		}
		file := job.files[fileIndex]
		dest := filepath.Join(workDir, fmt.Sprintf("subtitle-%04d.vtt", position))
		n, err := concatLocalMergeChunks(chunksDir, fileIndex, file.Chunks, dest)
		if err != nil {
			return fmt.Errorf("assemble subtitle %s: %w", file.Name, err)
		}
		if n != file.Size {
			return fmt.Errorf("assemble subtitle %s: size mismatch", file.Name)
		}
	}
	if job.coverIndex >= 0 {
		file := job.files[job.coverIndex]
		dest := filepath.Join(workDir, "cover.raw")
		n, err := concatLocalMergeChunks(chunksDir, job.coverIndex, file.Chunks, dest)
		if err != nil {
			return fmt.Errorf("assemble cover %s: %w", file.Name, err)
		}
		if n != file.Size {
			return fmt.Errorf("assemble cover %s: size mismatch", file.Name)
		}
	}
	return os.RemoveAll(chunksDir)
}

func concatLocalMergeChunks(chunksDir string, fileIndex, chunkCount int, dest string) (int64, error) {
	out, err := os.OpenFile(dest, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o600)
	if err != nil {
		return 0, err
	}
	var total int64
	buf := make([]byte, 256<<10)
	for chunk := 0; chunk < chunkCount; chunk++ {
		src, openErr := os.Open(filepath.Join(chunksDir, fmt.Sprintf("f%d-c%d.part", fileIndex, chunk)))
		if openErr != nil {
			out.Close()
			return total, openErr
		}
		n, copyErr := io.CopyBuffer(out, src, buf)
		closeErr := src.Close()
		if copyErr != nil {
			out.Close()
			return total, copyErr
		}
		if closeErr != nil {
			out.Close()
			return total, closeErr
		}
		total += n
	}
	return total, out.Close()
}

// CleanupExpiredLocalMerges abandons upload-phase local merge jobs whose
// creator disappeared, freeing their staging directories and slots.
func (s *Server) CleanupExpiredLocalMerges(ctx context.Context) {
	cutoff := time.Now().UTC().Add(-s.cfg.UploadExpires)
	s.audioMergeMu.Lock()
	var stale []*audioMergeJob
	for _, job := range s.audioMergeJobs {
		if !job.localUpload {
			continue
		}
		job.mu.Lock()
		uploading := job.Status == "uploading"
		job.mu.Unlock()
		if !uploading {
			continue
		}
		if created, err := time.Parse(time.RFC3339Nano, job.CreatedAt); err == nil && created.Before(cutoff) {
			stale = append(stale, job)
		}
	}
	for _, job := range stale {
		delete(s.audioMergeJobs, job.ID)
		job.cleanupStaging(s.log)
		job.releaseUploadSlot(s)
	}
	s.audioMergeMu.Unlock()
	for _, job := range stale {
		_, _ = s.db.ExecContext(ctx, `DELETE FROM files WHERE id=? AND status='pending'`, job.OutputFileID)
		s.log.Info("expired local merge upload cleaned", "job", job.ID, "output", job.OutputName)
	}
}
