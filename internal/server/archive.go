package server

import (
	"bufio"
	"context"
	"database/sql"
	"errors"
	"fmt"
	"io"
	"mime"
	"net/http"
	"os"
	"os/exec"
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
	maxArchiveListingBytes  = 64 << 20
	archiveDiskReserve      = int64(64 << 20)
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
}

func (job *archiveJob) fail(message string) {
	job.mu.Lock()
	job.Status, job.Error, job.Message = "failed", message, message
	job.passwordDeadline = time.Time{}
	job.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
	job.mu.Unlock()
}

func (job *archiveJob) needsPassword(message string) {
	job.mu.Lock()
	job.Status, job.Error, job.Message = "waiting_password", message, message
	job.passwordDeadline = time.Now().Add(archivePasswordWaitTTL)
	job.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
	job.mu.Unlock()
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

func sevenZipExecutable() (string, error) {
	if path, err := exec.LookPath("7zz"); err == nil {
		return path, nil
	}
	if path, err := exec.LookPath("7z"); err == nil {
		return path, nil
	}
	return "", errors.New("7-Zip is unavailable")
}

func (s *Server) startArchiveExtract(w http.ResponseWriter, r *http.Request) {
	f, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || f.Kind != "file" || f.Status != "ready" || !isArchiveName(f.Name) {
		problem(w, http.StatusNotFound, "ready archive file not found")
		return
	}
	if _, err := sevenZipExecutable(); err != nil {
		problem(w, http.StatusServiceUnavailable, "online extraction is unavailable")
		return
	}
	parentID := RootID
	if f.ParentID != nil {
		parentID = *f.ParentID
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	job := &archiveJob{ID: ids.New(), FileID: f.ID, ParentID: parentID, Name: f.Name, Status: "queued", Progress: 0, Message: "等待解压", CreatedAt: now, UpdatedAt: now}
	s.archiveMu.Lock()
	s.archiveJobs[job.ID] = job
	s.archiveMu.Unlock()
	go s.runArchiveExtract(s.audioHLSCtx, f, parentID, job, "")
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
	go s.runArchiveExtract(s.audioHLSCtx, f, job.ParentID, job, in.Password)
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

type archiveEntry struct {
	Path      string
	Size      int64
	IsDir     bool
	Encrypted bool
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

func ensureArchiveDiskSpace(path string, required int64) error {
	if required < 0 || required > int64(^uint64(0)>>1)-archiveDiskReserve {
		return errors.New("temporary disk space requirement is invalid")
	}
	available, err := archiveDiskAvailable(path)
	if err != nil {
		return fmt.Errorf("check temporary disk space: %w", err)
	}
	needed := required + archiveDiskReserve
	if available < needed {
		return fmt.Errorf("temporary disk space is insufficient: need at least %d MiB, only %d MiB is available", (needed+(1<<20)-1)>>20, available>>20)
	}
	return nil
}

func archiveExpandedSize(entries []archiveEntry) (int64, error) {
	var total int64
	for _, entry := range entries {
		if entry.IsDir {
			continue
		}
		if entry.Size < 0 || total > int64(^uint64(0)>>1)-entry.Size {
			return 0, errors.New("archive expanded size is invalid")
		}
		total += entry.Size
	}
	return total, nil
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

func archiveAttributesUnsafe(attributes string) bool {
	lower := strings.ToLower(attributes)
	if strings.Contains(lower, "reparse") {
		return true
	}
	for _, field := range strings.Fields(lower) {
		if len(field) >= 10 && field[0] == 'l' {
			return true
		}
	}
	return false
}

func archivePasswordArg(password string) string {
	if password == "" {
		return "-p-"
	}
	return "-p" + password
}

func archivePasswordFailure(detail string, supplied bool) error {
	lower := strings.ToLower(detail)
	for _, marker := range []string{"wrong password", "password is incorrect", "incorrect password", "can not open encrypted archive", "cannot open encrypted archive"} {
		if strings.Contains(lower, marker) {
			if supplied {
				return errArchiveWrongPassword
			}
			return errArchivePasswordRequired
		}
	}
	return nil
}

func inspectArchive(ctx context.Context, executable, archivePath string, archiveSize int64, password string) ([]archiveEntry, error) {
	stdout := &limitedBuffer{limit: maxArchiveListingBytes}
	stderr := &limitedBuffer{limit: 64 << 10}
	cmd := exec.CommandContext(ctx, executable, "l", "-slt", "-ba", archivePasswordArg(password), archivePath)
	cmd.Stdout, cmd.Stderr = stdout, stderr
	if err := cmd.Run(); err != nil {
		detail := strings.TrimSpace(stderr.String() + "\n" + stdout.String())
		if passwordErr := archivePasswordFailure(detail, password != ""); passwordErr != nil {
			return nil, fmt.Errorf("%w: %s", passwordErr, detail)
		}
		return nil, mediaCommandError("7-Zip archive inspection", err, ctx.Err(), detail)
	}
	if stdout.buf.Len() >= maxArchiveListingBytes {
		return nil, errors.New("archive file list is too large")
	}
	return parseArchiveListing(stdout.String(), archiveSize, password != "")
}

func parseArchiveListing(listing string, archiveSize int64, passwordSupplied bool) ([]archiveEntry, error) {
	var entries []archiveEntry
	var path, attributes string
	var linked, encrypted bool
	var size int64
	flush := func() error {
		if path == "" {
			return nil
		}
		if err := validateArchivePath(path); err != nil {
			return err
		}
		if linked || archiveAttributesUnsafe(attributes) {
			return errors.New("archive links are not allowed")
		}
		entries = append(entries, archiveEntry{Path: filepath.ToSlash(filepath.Clean(path)), Size: size, IsDir: strings.Contains(attributes, "D"), Encrypted: encrypted})
		path, attributes, size, linked, encrypted = "", "", 0, false, false
		return nil
	}
	scanner := bufio.NewScanner(strings.NewReader(listing))
	scanner.Buffer(make([]byte, 64<<10), 2<<20)
	for scanner.Scan() {
		line := scanner.Text()
		if line == "" {
			if err := flush(); err != nil {
				return nil, err
			}
			continue
		}
		key, value, ok := strings.Cut(line, " = ")
		if !ok {
			continue
		}
		switch key {
		case "Path":
			path = value
		case "Size":
			size, _ = strconv.ParseInt(value, 10, 64)
		case "Attributes":
			attributes = value
		case "Encrypted":
			encrypted = value == "+"
		case "Symbolic Link", "Hard Link", "Reparse Point":
			linked = linked || value != ""
		}
	}
	if err := scanner.Err(); err != nil {
		return nil, err
	}
	if err := flush(); err != nil {
		return nil, err
	}
	if len(entries) == 0 {
		return nil, errors.New("archive is empty")
	}
	if !passwordSupplied {
		for _, entry := range entries {
			if entry.Encrypted {
				return nil, errArchivePasswordRequired
			}
		}
	}
	if len(entries) > maxArchiveEntries {
		return nil, fmt.Errorf("archive contains more than %d entries", maxArchiveEntries)
	}
	limit := archiveExpandedLimit(archiveSize)
	var total int64
	for _, entry := range entries {
		if entry.Size < 0 || total > limit-entry.Size {
			return nil, fmt.Errorf("archive expands beyond the %d GiB safety limit", limit>>30)
		}
		total += entry.Size
	}
	return entries, nil
}

type extractedObject struct {
	rel      string
	path     string
	size     int64
	key      string
	etag     string
	mimeType string
}

func (s *Server) openArchiveSource(ctx context.Context, f File) (io.ReadCloser, error) {
	if storage.IsManifestKey(f.objectKey) {
		return s.storage.Open(storage.WithDynamicReadAhead(ctx), f.objectKey)
	}
	return s.storage.OpenRaw(ctx, f.objectKey)
}

func (s *Server) runArchiveExtract(ctx context.Context, f File, parentID string, job *archiveJob, password string) {
	fail := func(err error) { s.failArchiveJob(job, err) }
	requestPassword := func(err error) {
		message := "压缩包已加密，请输入密码后继续"
		if errors.Is(err, errArchiveWrongPassword) {
			message = "压缩包密码错误，请重新输入"
		}
		job.needsPassword(message)
		s.log.Info("archive job waiting for password", "file", job.FileID, "job", job.ID, "status", "waiting_password", "error", err)
	}
	select {
	case s.archiveSlots <- struct{}{}:
		defer func() { <-s.archiveSlots }()
	case <-ctx.Done():
		fail(ctx.Err())
		return
	}
	tempDir, archivePath := job.staged()
	if tempDir == "" || archivePath == "" {
		var err error
		tempDir, err = os.MkdirTemp("", "revaro-extract-")
		if err != nil {
			fail(fmt.Errorf("create temporary extraction directory: %w", err))
			return
		}
		archivePath = filepath.Join(tempDir, "source"+filepath.Ext(f.Name))
		job.setStaged(tempDir, archivePath)
	}
	outputDir := filepath.Join(tempDir, "output")
	if _, statErr := os.Stat(archivePath); errors.Is(statErr, os.ErrNotExist) {
		if err := ensureArchiveDiskSpace(tempDir, f.Size); err != nil {
			fail(err)
			return
		}
		job.update("checking", 2, "正在检测压缩包是否加密")
		source, err := s.openArchiveSource(ctx, f)
		if err != nil {
			fail(fmt.Errorf("read archive from S3 while checking encryption: %w", err))
			return
		}
		out, err := os.OpenFile(archivePath, os.O_CREATE|os.O_WRONLY|os.O_EXCL, 0o600)
		if err == nil {
			_, err = io.Copy(out, source)
			closeErr := out.Close()
			if err == nil {
				err = closeErr
			}
		}
		_ = source.Close()
		if err != nil {
			fail(fmt.Errorf("stage archive for encryption check: %w", err))
			return
		}
	} else if statErr != nil {
		fail(fmt.Errorf("inspect staged archive: %w", statErr))
		return
	}
	executable, err := sevenZipExecutable()
	if err != nil {
		fail(err)
		return
	}
	job.update("checking", 18, "正在验证密码与压缩包安全性")
	entries, inspectErr := inspectArchive(ctx, executable, archivePath, f.Size, password)
	if inspectErr != nil {
		if errors.Is(inspectErr, errArchivePasswordRequired) || errors.Is(inspectErr, errArchiveWrongPassword) {
			requestPassword(inspectErr)
			return
		}
		fail(inspectErr)
		return
	}
	expandedEstimate, sizeErr := archiveExpandedSize(entries)
	if sizeErr != nil {
		fail(sizeErr)
		return
	}
	if err = ensureArchiveDiskSpace(tempDir, expandedEstimate); err != nil {
		fail(err)
		return
	}
	// A wrong password can leave a partial output tree. The staged source stays
	// cached, but every extraction attempt gets a fresh output directory.
	if err = os.RemoveAll(outputDir); err != nil {
		fail(fmt.Errorf("reset extraction output directory: %w", err))
		return
	}
	if err = os.Mkdir(outputDir, 0o700); err != nil {
		fail(fmt.Errorf("create extraction output directory: %w", err))
		return
	}
	job.update("extracting", 25, "正在解压")
	stderr := &limitedBuffer{limit: 128 << 10}
	cmd := exec.CommandContext(ctx, executable, "x", "-y", "-bd", "-bb0", archivePasswordArg(password), "-o"+outputDir, archivePath)
	cmd.Stdout, cmd.Stderr = io.Discard, stderr
	if err = cmd.Run(); err != nil {
		if passwordErr := archivePasswordFailure(stderr.String(), password != ""); passwordErr != nil {
			requestPassword(passwordErr)
			return
		}
		fail(mediaCommandError("7-Zip extraction", err, ctx.Err(), stderr.String()))
		return
	}
	var paths []string
	var expanded int64
	expandedLimit := archiveExpandedLimit(f.Size)
	err = filepath.WalkDir(outputDir, func(path string, entry os.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		if path == outputDir {
			return nil
		}
		rel, relErr := filepath.Rel(outputDir, path)
		if relErr != nil {
			return relErr
		}
		if err := validateArchivePath(rel); err != nil {
			return err
		}
		info, infoErr := entry.Info()
		if infoErr != nil {
			return infoErr
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
		fail(fmt.Errorf("validate extracted files: %w", err))
		return
	}
	if len(paths) > maxArchiveEntries {
		fail(fmt.Errorf("archive contains more than %d entries", maxArchiveEntries))
		return
	}
	job.update("importing", 35, "正在写入网盘")
	objects := make([]extractedObject, 0)
	for _, path := range paths {
		info, statErr := os.Stat(path)
		if statErr != nil || !info.Mode().IsRegular() {
			continue
		}
		file, openErr := os.Open(path)
		if openErr != nil {
			fail(fmt.Errorf("open extracted file %q: %w", filepath.Base(path), openErr))
			return
		}
		mimeType := mime.TypeByExtension(strings.ToLower(filepath.Ext(path)))
		if mimeType == "" {
			mimeType = "application/octet-stream"
		}
		key, stored, storeErr := s.storeBlob(ctx, file, info.Size(), mimeType)
		_ = file.Close()
		if storeErr != nil {
			fail(fmt.Errorf("upload extracted file %q to S3: %w", filepath.Base(path), storeErr))
			return
		}
		if stored.Size != info.Size() {
			fail(fmt.Errorf("upload extracted file %q: stored size %d does not match local size %d", filepath.Base(path), stored.Size, info.Size()))
			return
		}
		rel, _ := filepath.Rel(outputDir, path)
		objects = append(objects, extractedObject{rel: filepath.ToSlash(rel), path: path, size: info.Size(), key: key, etag: stored.ETag, mimeType: mimeType})
		job.update("importing", 35+int(float64(len(objects))/float64(max(1, len(paths)))*58), "正在写入网盘")
	}
	rootID, rootName, err := s.commitExtractedArchive(ctx, parentID, archiveBaseName(f.Name), outputDir, paths, objects)
	if err != nil {
		fail(fmt.Errorf("commit extracted files: %w", err))
		return
	}
	job.mu.Lock()
	job.Status, job.Progress, job.Message, job.OutputID, job.OutputName = "done", 100, "解压完成", rootID, rootName
	job.passwordDeadline = time.Time{}
	job.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
	job.mu.Unlock()
	s.cleanupArchiveJobStaging(job)
	s.log.Info("archive job completed", "file", job.FileID, "job", job.ID, "output", rootID, "name", rootName)
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
