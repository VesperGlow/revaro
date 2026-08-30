package server

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"mime"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
	"github.com/VesperGlow/revaro/internal/storage"
	"github.com/go-chi/chi/v5"
	"golang.org/x/sys/unix"
)

const (
	maxArchiveEntries       = 100000
	maxArchiveExpandedBytes = int64(64 << 30)
	archivePasswordWaitTTL  = 30 * time.Minute
)

var (
	errArchivePasswordRequired = errors.New("archive password is required")
	errArchiveWrongPassword    = errors.New("archive password is incorrect")
)

type archiveJob struct {
	mu sync.RWMutex
	// The staged object is transient and only survives while this in-memory job
	// is alive. It lets password retries continue without downloading from S3
	// again; the password itself is never assigned to the job.
	tempDir          string
	archivePath      string
	passwordDeadline time.Time
	changed          func()
	cancel           context.CancelFunc

	ID         string `json:"id"`
	FileID     string `json:"file_id"`
	ParentID   string `json:"parent_id"`
	Name       string `json:"name"`
	Status     string `json:"status"`
	Progress   int    `json:"progress"`
	Message    string `json:"message"`
	OutputID   string `json:"output_id,omitempty"`
	OutputName string `json:"output_name,omitempty"`
	Error      string `json:"error,omitempty"`
	CreatedAt  string `json:"created_at"`
	UpdatedAt  string `json:"updated_at"`
}

func (job *archiveJob) update(status string, progress int, message string) {
	job.mu.Lock()
	job.Status, job.Progress, job.Message = status, max(job.Progress, min(progress, 100)), message
	job.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
	job.mu.Unlock()
	if job.changed != nil {
		job.changed()
	}
}

func (job *archiveJob) fail(message string) {
	job.mu.Lock()
	job.Status, job.Error, job.Message = "failed", message, message
	job.passwordDeadline = time.Time{}
	job.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
	job.mu.Unlock()
	if job.changed != nil {
		job.changed()
	}
}

func (job *archiveJob) needsPassword(message string) {
	job.mu.Lock()
	job.Status, job.Error, job.Message = "waiting_password", message, message
	job.passwordDeadline = time.Now().Add(archivePasswordWaitTTL)
	job.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
	job.mu.Unlock()
	if job.changed != nil {
		job.changed()
	}
}

func (job *archiveJob) resumeWithPassword() bool {
	job.mu.Lock()
	defer job.mu.Unlock()
	if job.Status != "waiting_password" {
		return false
	}
	job.Status, job.Error, job.Message = "checking", "", "正在验证密码"
	job.passwordDeadline = time.Time{}
	job.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
	if job.changed != nil {
		defer job.changed()
	}
	return true
}

func (job *archiveJob) setStaged(tempDir, archivePath string) {
	job.mu.Lock()
	job.tempDir, job.archivePath = tempDir, archivePath
	job.mu.Unlock()
}

func (job *archiveJob) staged() (string, string) {
	job.mu.RLock()
	defer job.mu.RUnlock()
	return job.tempDir, job.archivePath
}

func (job *archiveJob) takeStagedDir() string {
	job.mu.Lock()
	defer job.mu.Unlock()
	tempDir := job.tempDir
	job.tempDir, job.archivePath = "", ""
	return tempDir
}

func (job *archiveJob) expirePasswordWait(now time.Time) bool {
	job.mu.Lock()
	defer job.mu.Unlock()
	if job.Status != "waiting_password" || job.passwordDeadline.IsZero() || now.Before(job.passwordDeadline) {
		return false
	}
	message := "解压失败：30 分钟内未输入密码，任务已超时"
	job.Status, job.Error, job.Message = "failed", message, message
	job.passwordDeadline = time.Time{}
	job.UpdatedAt = now.UTC().Format(time.RFC3339Nano)
	return true
}

func (job *archiveJob) snapshot() archiveJob {
	job.mu.RLock()
	defer job.mu.RUnlock()
	return archiveJob{ID: job.ID, FileID: job.FileID, ParentID: job.ParentID, Name: job.Name, Status: job.Status, Progress: job.Progress, Message: job.Message, OutputID: job.OutputID, OutputName: job.OutputName, Error: job.Error, CreatedAt: job.CreatedAt, UpdatedAt: job.UpdatedAt}
}

func isArchiveName(name string) bool {
	lower := strings.ToLower(name)
	for _, suffix := range []string{".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tgz", ".tbz", ".tbz2", ".txz", ".tzst", ".zip", ".7z", ".rar", ".tar", ".gz", ".bz2", ".xz", ".zst"} {
		if strings.HasSuffix(lower, suffix) {
			return true
		}
	}
	return false
}

func archiveBaseName(name string) string {
	lower := strings.ToLower(name)
	for _, suffix := range []string{".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tgz", ".tbz2", ".tbz", ".txz", ".tzst", ".zip", ".7z", ".rar", ".tar", ".gz", ".bz2", ".xz", ".zst"} {
		if strings.HasSuffix(lower, suffix) {
			return name[:len(name)-len(suffix)]
		}
	}
	return strings.TrimSuffix(name, filepath.Ext(name))
}

func (s *Server) startArchiveExtract(w http.ResponseWriter, r *http.Request) {
	f, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || f.Kind != "file" || f.Status != "ready" || !isArchiveName(f.Name) {
		problem(w, http.StatusNotFound, "ready archive file not found")
		return
	}
	if _, ok := s.storage.(storage.ArchiveExtractor); !ok {
		problem(w, http.StatusServiceUnavailable, "online extraction is unavailable")
		return
	}
	parentID := RootID
	if f.ParentID != nil {
		parentID = *f.ParentID
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	job := &archiveJob{ID: ids.New(), FileID: f.ID, ParentID: parentID, Name: f.Name, Status: "queued", Progress: 0, Message: "等待解压", CreatedAt: now, UpdatedAt: now, changed: s.jobs.Changed}
	s.archiveMu.Lock()
	s.archiveJobs[job.ID] = job
	s.archiveMu.Unlock()
	jobCtx, jobCancel := context.WithCancel(s.audioHLSCtx)
	job.cancel = jobCancel
	if !s.runBackground(func() { s.runArchiveExtract(jobCtx, f, parentID, job, "") }) {
		jobCancel()
		job.fail("service is shutting down")
		s.archiveMu.Lock()
		delete(s.archiveJobs, job.ID)
		s.archiveMu.Unlock()
		problem(w, http.StatusServiceUnavailable, "service is shutting down")
		return
	}
	writeJSON(w, http.StatusAccepted, job.snapshot())
}

func (s *Server) resumeArchiveExtract(w http.ResponseWriter, r *http.Request) {
	var in struct {
		Password string `json:"password"`
	}
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	if in.Password == "" || len(in.Password) > 1024 {
		problem(w, http.StatusBadRequest, "archive password is required and must be at most 1024 bytes")
		return
	}
	s.archiveMu.RLock()
	job := s.archiveJobs[chi.URLParam(r, "id")]
	s.archiveMu.RUnlock()
	if job == nil {
		problem(w, http.StatusNotFound, "archive job not found")
		return
	}
	if !job.resumeWithPassword() {
		problem(w, http.StatusConflict, "archive job is not waiting for a password")
		return
	}
	f, err := s.readableFile(r.Context(), job.FileID)
	if err != nil || f.Kind != "file" || f.Status != "ready" || !isArchiveName(f.Name) {
		s.failArchiveJob(job, errors.New("archive source is no longer available"))
		problem(w, http.StatusConflict, "archive source is no longer available")
		return
	}
	jobCtx, jobCancel := context.WithCancel(s.audioHLSCtx)
	job.cancel = jobCancel
	if !s.runBackground(func() { s.runArchiveExtract(jobCtx, f, job.ParentID, job, in.Password) }) {
		jobCancel()
		job.fail("service is shutting down")
		problem(w, http.StatusServiceUnavailable, "service is shutting down")
		return
	}
	writeJSON(w, http.StatusAccepted, job.snapshot())
}

func (s *Server) getArchiveExtract(w http.ResponseWriter, r *http.Request) {
	s.archiveMu.RLock()
	job := s.archiveJobs[chi.URLParam(r, "id")]
	s.archiveMu.RUnlock()
	if job == nil {
		problem(w, http.StatusNotFound, "archive job not found")
		return
	}
	writeJSON(w, http.StatusOK, job.snapshot())
}

func (s *Server) listArchiveExtracts(w http.ResponseWriter, _ *http.Request) {
	s.archiveMu.RLock()
	jobs := make([]archiveJob, 0, len(s.archiveJobs))
	for _, job := range s.archiveJobs {
		jobs = append(jobs, job.snapshot())
	}
	s.archiveMu.RUnlock()
	sort.Slice(jobs, func(i, j int) bool { return jobs[i].CreatedAt > jobs[j].CreatedAt })
	writeJSON(w, http.StatusOK, map[string]any{"items": jobs})
}

func archiveJobTerminal(status string) bool {
	return status == "done" || status == "failed"
}

func (s *Server) cleanupArchiveJobStaging(job *archiveJob) {
	if tempDir := job.takeStagedDir(); tempDir != "" {
		if err := os.RemoveAll(tempDir); err != nil {
			s.log.Warn("archive staging cleanup failed", "file", job.FileID, "job", job.ID, "path", tempDir, "error", err)
		}
	}
}

func (s *Server) deleteArchiveExtract(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	s.archiveMu.Lock()
	job := s.archiveJobs[id]
	if job != nil {
		snapshot := job.snapshot()
		if !archiveJobTerminal(snapshot.Status) {
			s.archiveMu.Unlock()
			problem(w, http.StatusConflict, "active archive job cannot be removed")
			return
		}
		delete(s.archiveJobs, id)
	}
	s.archiveMu.Unlock()
	if job == nil {
		problem(w, http.StatusNotFound, "archive job not found")
		return
	}
	s.cleanupArchiveJobStaging(job)
	w.WriteHeader(http.StatusNoContent)
}

func validateArchivePath(path string) error {
	path = filepath.ToSlash(strings.TrimSpace(path))
	if path == "" || strings.HasPrefix(path, "/") || filepath.IsAbs(path) || strings.ContainsRune(path, 0) {
		return errors.New("archive contains an invalid absolute path")
	}
	clean := filepath.ToSlash(filepath.Clean(path))
	if clean == "." || clean == ".." || strings.HasPrefix(clean, "../") || (len(clean) >= 2 && clean[1] == ':') {
		return errors.New("archive contains a path outside its root")
	}
	for _, component := range strings.Split(clean, "/") {
		if err := validateName(component); err != nil {
			return fmt.Errorf("archive path %q is not supported: %w", path, err)
		}
	}
	return nil
}

func archiveExpandedLimit(archiveSize int64) int64 {
	limit := maxArchiveExpandedBytes
	if archiveSize > 0 && archiveSize < limit/100 {
		limit = max(int64(4<<30), archiveSize*100)
	}
	return limit
}

func archiveDiskAvailable(path string) (int64, error) {
	var stat unix.Statfs_t
	if err := unix.Statfs(path, &stat); err != nil {
		return 0, err
	}
	available := uint64(stat.Bavail) * uint64(stat.Bsize)
	if available > uint64(^uint64(0)>>1) {
		return int64(^uint64(0) >> 1), nil
	}
	return int64(available), nil
}

func archiveFailureMessage(err error) string {
	if err == nil {
		return "解压失败：未知错误"
	}
	lower := strings.ToLower(err.Error())
	if errors.Is(err, unix.ENOSPC) || errors.Is(err, unix.EDQUOT) || strings.Contains(lower, "no space left") || strings.Contains(lower, "disk space is insufficient") || strings.Contains(lower, "disk quota") {
		return "解压失败：临时磁盘空间不足，请释放服务器磁盘空间后重试"
	}
	if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
		return "解压失败：任务已取消或处理超时"
	}
	return "解压失败：" + err.Error()
}

func (s *Server) failArchiveJob(job *archiveJob, err error) {
	snapshot := job.snapshot()
	if err == nil {
		err = errors.New("unknown archive error")
	}
	job.fail(archiveFailureMessage(err))
	s.log.Warn("archive job failed", "file", job.FileID, "job", job.ID, "status", snapshot.Status, "error", err)
	s.cleanupArchiveJobStaging(job)
}

type extractedObject struct {
	rel      string
	path     string
	size     int64
	key      string
	etag     string
	mimeType string
}

func (s *Server) runArchiveExtract(ctx context.Context, f File, parentID string, job *archiveJob, password string) {
	fail := func(err error) { s.failArchiveJob(job, err) }
	requestPassword := func(err error) {
		message := "压缩包已加密，请输入密码后继续"
		if errors.Is(err, storage.ErrArchiveWrongPassword) {
			message = "压缩包密码错误，请重新输入"
		}
		job.needsPassword(message)
		s.log.Info("archive job waiting for password", "file", job.FileID, "job", job.ID)
	}
	select {
	case s.archiveSlots <- struct{}{}:
		defer func() { <-s.archiveSlots }()
	case <-ctx.Done():
		fail(ctx.Err())
		return
	}
	extractor, ok := s.storage.(storage.ArchiveExtractor)
	if !ok {
		fail(errors.New("Rust archive engine is unavailable"))
		return
	}
	expectedRoot := filepath.Join(s.cfg.WorkDir, "revaro-extract-"+job.ID)
	job.setStaged(expectedRoot, filepath.Join(expectedRoot, "source.archive"))
	job.update("checking", 2, "正在准备压缩包")
	pollCtx, pollCancel := context.WithCancel(ctx)
	pollDone := make(chan struct{})
	go func() {
		defer close(pollDone)
		s.pollArchiveProgress(pollCtx, job, extractor, f.Size)
	}()
	outputDir, err := extractor.ExtractArchive(ctx, f.objectKey, job.ID, f.Size, password)
	pollCancel()
	<-pollDone
	if err != nil {
		if errors.Is(err, storage.ErrArchivePasswordRequired) || errors.Is(err, storage.ErrArchiveWrongPassword) {
			requestPassword(err)
			return
		}
		fail(err)
		return
	}
	if err := s.importExtractedArchive(ctx, f, parentID, job, outputDir); err != nil {
		fail(err)
	}
}

// pollArchiveProgress mirrors the Rust data-plane extraction progress into the
// in-memory job while ExtractArchive is blocked. The Rust side reports two
// phases: "downloading" (source staged from S3) and "extracting" (entries
// written to the workspace).
func (s *Server) pollArchiveProgress(ctx context.Context, job *archiveJob, extractor storage.ArchiveExtractor, archiveSize int64) {
	ticker := time.NewTicker(400 * time.Millisecond)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			progress, err := extractor.ArchiveProgress(ctx, job.ID)
			if err != nil {
				return
			}
			switch progress.Phase {
			case "downloading":
				percent := 2
				if archiveSize > 0 {
					percent = 2 + int(min(progress.DownloadedBytes, archiveSize)*6/archiveSize)
				}
				job.update("downloading", min(percent, 8), "正在下载压缩包")
			case "extracting":
				job.update("extracting", 25, "正在解压")
			}
		}
	}
}

func (s *Server) cancelArchiveExtract(w http.ResponseWriter, r *http.Request) {
	s.archiveMu.RLock()
	job := s.archiveJobs[chi.URLParam(r, "id")]
	s.archiveMu.RUnlock()
	if job == nil {
		problem(w, http.StatusNotFound, "archive job not found")
		return
	}
	snapshot := job.snapshot()
	if archiveJobTerminal(snapshot.Status) {
		problem(w, http.StatusConflict, "archive job is already finished")
		return
	}
	extractor, ok := s.storage.(storage.ArchiveExtractor)
	if ok {
		if err := extractor.CancelArchive(r.Context(), job.ID); err != nil {
			s.log.Warn("archive cancel request failed", "file", job.FileID, "job", job.ID, "error", err)
		}
	}
	job.mu.Lock()
	if snapshot.Status == "waiting_password" {
		job.Status, job.Message, job.Error = "failed", "已取消", "解压任务已取消"
	} else {
		// Keep the job non-terminal until its worker has unwound. Otherwise a
		// follow-up DELETE could remove the workspace while it is still in use.
		job.Message, job.Error = "正在取消解压任务", ""
	}
	job.passwordDeadline = time.Time{}
	job.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
	cancel := job.cancel
	job.mu.Unlock()
	if cancel != nil {
		cancel()
	}
	if job.changed != nil {
		job.changed()
	}
	// A running worker owns its workspace and cleans it after cancellation has
	// unwound. Password-waiting jobs have no worker, so the handler owns cleanup.
	if snapshot.Status == "waiting_password" {
		s.cleanupArchiveJobStaging(job)
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) importExtractedArchive(ctx context.Context, f File, parentID string, job *archiveJob, outputDir string) error {
	var paths []string
	var expanded int64
	expandedLimit := archiveExpandedLimit(f.Size)
	err := filepath.WalkDir(outputDir, func(path string, entry os.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		if path == outputDir {
			return nil
		}
		rel, err := filepath.Rel(outputDir, path)
		if err != nil {
			return err
		}
		if err := validateArchivePath(rel); err != nil {
			return err
		}
		info, err := entry.Info()
		if err != nil {
			return err
		}
		if info.Mode()&os.ModeSymlink != 0 || (!info.Mode().IsRegular() && !info.IsDir()) {
			return errors.New("archive contains unsupported links or special files")
		}
		if info.Mode().IsRegular() {
			if info.Size() < 0 || expanded > expandedLimit-info.Size() {
				return errors.New("extracted data exceeds the safety limit")
			}
			expanded += info.Size()
		}
		paths = append(paths, path)
		return nil
	})
	if err != nil {
		return fmt.Errorf("validate extracted files: %w", err)
	}
	if len(paths) > maxArchiveEntries {
		return fmt.Errorf("archive contains more than %d entries", maxArchiveEntries)
	}
	job.update("importing", 35, "正在写入网盘")
	objects := make([]extractedObject, 0)
	for _, path := range paths {
		info, err := os.Stat(path)
		if err != nil || !info.Mode().IsRegular() {
			continue
		}
		file, err := os.Open(path)
		if err != nil {
			return fmt.Errorf("open extracted file %q: %w", filepath.Base(path), err)
		}
		mimeType := mime.TypeByExtension(strings.ToLower(filepath.Ext(path)))
		if mimeType == "" {
			mimeType = "application/octet-stream"
		}
		key, stored, storeErr := s.storeBlob(ctx, file, info.Size(), mimeType)
		_ = file.Close()
		if storeErr != nil {
			return fmt.Errorf("upload extracted file %q to S3: %w", filepath.Base(path), storeErr)
		}
		if stored.Size != info.Size() {
			return fmt.Errorf("upload extracted file %q: stored size %d does not match local size %d", filepath.Base(path), stored.Size, info.Size())
		}
		rel, _ := filepath.Rel(outputDir, path)
		objects = append(objects, extractedObject{rel: filepath.ToSlash(rel), path: path, size: info.Size(), key: key, etag: stored.ETag, mimeType: mimeType})
		job.update("importing", 35+int(float64(len(objects))/float64(max(1, len(paths)))*58), "正在写入网盘")
	}
	rootID, rootName, err := s.commitExtractedArchive(ctx, parentID, archiveBaseName(f.Name), outputDir, paths, objects)
	if err != nil {
		return fmt.Errorf("commit extracted files: %w", err)
	}
	job.mu.Lock()
	job.Status, job.Progress, job.Message, job.OutputID, job.OutputName = "done", 100, "解压完成", rootID, rootName
	job.passwordDeadline = time.Time{}
	job.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
	job.mu.Unlock()
	s.cleanupArchiveJobStaging(job)
	s.log.Info("archive job completed", "file", job.FileID, "job", job.ID, "output", rootID, "name", rootName)
	return nil
}

func (s *Server) cleanupArchiveJobs() {
	ticker := time.NewTicker(time.Minute)
	defer ticker.Stop()
	for {
		select {
		case <-s.audioHLSCtx.Done():
			return
		case now := <-ticker.C:
			s.archiveMu.RLock()
			jobs := make([]*archiveJob, 0, len(s.archiveJobs))
			for _, job := range s.archiveJobs {
				jobs = append(jobs, job)
			}
			s.archiveMu.RUnlock()
			for _, job := range jobs {
				if job.expirePasswordWait(now) {
					s.log.Warn("archive password wait expired", "file", job.FileID, "job", job.ID, "timeout", archivePasswordWaitTTL)
					s.cleanupArchiveJobStaging(job)
				}
			}
		}
	}
}

func availableArchiveName(ctx context.Context, tx *sql.Tx, parentID, preferred string) (string, error) {
	if validateName(preferred) != nil {
		preferred = "解压文件"
	}
	for index := 0; index < 10000; index++ {
		candidate := preferred
		if index > 0 {
			candidate += " (" + strconv.Itoa(index+1) + ")"
		}
		var exists int
		if err := tx.QueryRowContext(ctx, `SELECT EXISTS(SELECT 1 FROM files WHERE parent_id=? AND name=? AND deleted_at IS NULL)`, parentID, candidate).Scan(&exists); err != nil {
			return "", err
		}
		if exists == 0 {
			return candidate, nil
		}
	}
	return "", errors.New("could not choose extraction folder name")
}

func (s *Server) commitExtractedArchive(ctx context.Context, parentID, preferredName, outputDir string, paths []string, objects []extractedObject) (string, string, error) {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return "", "", err
	}
	defer tx.Rollback()
	rootName, err := availableArchiveName(ctx, tx, parentID, preferredName)
	if err != nil {
		return "", "", err
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	rootID := ids.New()
	if _, err = tx.ExecContext(ctx, `INSERT INTO files(id,parent_id,name,kind,status,created_at,updated_at) VALUES(?,?,?,'directory','ready',?,?)`, rootID, parentID, rootName, now, now); err != nil {
		return "", "", err
	}
	directories := map[string]string{".": rootID, "": rootID}
	var directoryPaths []string
	for _, path := range paths {
		info, statErr := os.Stat(path)
		if statErr != nil || !info.IsDir() {
			continue
		}
		rel, _ := filepath.Rel(outputDir, path)
		directoryPaths = append(directoryPaths, filepath.ToSlash(rel))
	}
	sort.Slice(directoryPaths, func(i, j int) bool {
		left, right := strings.Count(directoryPaths[i], "/"), strings.Count(directoryPaths[j], "/")
		if left == right {
			return directoryPaths[i] < directoryPaths[j]
		}
		return left < right
	})
	for _, rel := range directoryPaths {
		parentRel := filepath.ToSlash(filepath.Dir(rel))
		id := ids.New()
		if _, err = tx.ExecContext(ctx, `INSERT INTO files(id,parent_id,name,kind,status,created_at,updated_at) VALUES(?,?,?,'directory','ready',?,?)`, id, directories[parentRel], filepath.Base(rel), now, now); err != nil {
			return "", "", err
		}
		directories[rel] = id
	}
	for _, object := range objects {
		parentRel := filepath.ToSlash(filepath.Dir(object.rel))
		if _, err = tx.ExecContext(ctx, `INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at) VALUES(?,?,?,'file',?,?,?,?,'ready',?,?)`, ids.New(), directories[parentRel], filepath.Base(object.rel), object.key, object.size, object.mimeType, object.etag, now, now); err != nil {
			return "", "", err
		}
	}
	if err = tx.Commit(); err != nil {
		return "", "", err
	}
	return rootID, rootName, nil
}
