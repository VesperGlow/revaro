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
)

const (
	maxArchiveEntries       = 100000
	maxArchiveExpandedBytes = int64(64 << 30)
	maxArchiveListingBytes  = 64 << 20
)

type archiveJob struct {
	mu sync.RWMutex

	ID         string `json:"id"`
	FileID     string `json:"file_id"`
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

func (job *archiveJob) fail(err error) {
	job.mu.Lock()
	job.Status, job.Error, job.Message = "failed", err.Error(), "解压失败"
	job.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
	job.mu.Unlock()
}

func (job *archiveJob) snapshot() archiveJob {
	job.mu.RLock()
	defer job.mu.RUnlock()
	return archiveJob{ID: job.ID, FileID: job.FileID, Name: job.Name, Status: job.Status, Progress: job.Progress, Message: job.Message, OutputID: job.OutputID, OutputName: job.OutputName, Error: job.Error, CreatedAt: job.CreatedAt, UpdatedAt: job.UpdatedAt}
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
	job := &archiveJob{ID: ids.New(), FileID: f.ID, Name: f.Name, Status: "queued", Progress: 0, Message: "等待解压", CreatedAt: now, UpdatedAt: now}
	s.archiveMu.Lock()
	s.archiveJobs[job.ID] = job
	s.archiveMu.Unlock()
	go s.runArchiveExtract(s.audioHLSCtx, f, parentID, job)
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

type archiveEntry struct {
	Path  string
	Size  int64
	IsDir bool
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

func inspectArchive(ctx context.Context, executable, archivePath string, archiveSize int64) ([]archiveEntry, error) {
	stdout := &limitedBuffer{limit: maxArchiveListingBytes}
	stderr := &limitedBuffer{limit: 64 << 10}
	cmd := exec.CommandContext(ctx, executable, "l", "-slt", "-ba", "-p-", archivePath)
	cmd.Stdout, cmd.Stderr = stdout, stderr
	if err := cmd.Run(); err != nil {
		return nil, fmt.Errorf("7-Zip could not read this archive: %w: %s", err, strings.TrimSpace(stderr.String()))
	}
	if stdout.buf.Len() >= maxArchiveListingBytes {
		return nil, errors.New("archive file list is too large")
	}
	var entries []archiveEntry
	var path, attributes string
	var linked bool
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
		entries = append(entries, archiveEntry{Path: filepath.ToSlash(filepath.Clean(path)), Size: size, IsDir: strings.Contains(attributes, "D")})
		path, attributes, size, linked = "", "", 0, false
		return nil
	}
	scanner := bufio.NewScanner(strings.NewReader(stdout.String()))
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
	manifest storage.Manifest
	mimeType string
}

func (s *Server) runArchiveExtract(ctx context.Context, f File, parentID string, job *archiveJob) {
	select {
	case s.archiveSlots <- struct{}{}:
		defer func() { <-s.archiveSlots }()
	case <-ctx.Done():
		job.fail(ctx.Err())
		return
	}
	tempDir, err := os.MkdirTemp("", "revaro-extract-")
	if err != nil {
		job.fail(err)
		return
	}
	defer os.RemoveAll(tempDir)
	archivePath := filepath.Join(tempDir, "source"+filepath.Ext(f.Name))
	outputDir := filepath.Join(tempDir, "output")
	job.update("downloading", 2, "正在读取压缩包")
	source, err := s.storage.Open(ctx, f.objectKey)
	if err != nil {
		job.fail(fmt.Errorf("read archive: %w", err))
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
		job.fail(fmt.Errorf("stage archive: %w", err))
		return
	}
	executable, err := sevenZipExecutable()
	if err != nil {
		job.fail(err)
		return
	}
	job.update("checking", 18, "正在检查压缩包安全性")
	if _, err = inspectArchive(ctx, executable, archivePath, f.Size); err != nil {
		job.fail(err)
		return
	}
	if err = os.Mkdir(outputDir, 0o700); err != nil {
		job.fail(err)
		return
	}
	job.update("extracting", 25, "正在解压")
	stderr := &limitedBuffer{limit: 128 << 10}
	cmd := exec.CommandContext(ctx, executable, "x", "-y", "-bd", "-bb0", "-p-", "-o"+outputDir, archivePath)
	cmd.Stdout, cmd.Stderr = io.Discard, stderr
	if err = cmd.Run(); err != nil {
		job.fail(fmt.Errorf("7-Zip extraction failed: %w: %s", err, strings.TrimSpace(stderr.String())))
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
		job.fail(err)
		return
	}
	if len(paths) > maxArchiveEntries {
		job.fail(fmt.Errorf("archive contains more than %d entries", maxArchiveEntries))
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
			job.fail(openErr)
			return
		}
		key, manifest, storeErr := s.storage.Store(ctx, file)
		_ = file.Close()
		if storeErr != nil {
			job.fail(fmt.Errorf("store extracted file: %w", storeErr))
			return
		}
		rel, _ := filepath.Rel(outputDir, path)
		mimeType := mime.TypeByExtension(strings.ToLower(filepath.Ext(path)))
		if mimeType == "" {
			mimeType = "application/octet-stream"
		}
		objects = append(objects, extractedObject{rel: filepath.ToSlash(rel), path: path, size: info.Size(), key: key, manifest: manifest, mimeType: mimeType})
		job.update("importing", 35+int(float64(len(objects))/float64(max(1, len(paths)))*58), "正在写入网盘")
	}
	rootID, rootName, err := s.commitExtractedArchive(ctx, parentID, archiveBaseName(f.Name), outputDir, paths, objects)
	if err != nil {
		job.fail(err)
		return
	}
	job.mu.Lock()
	job.Status, job.Progress, job.Message, job.OutputID, job.OutputName = "done", 100, "解压完成", rootID, rootName
	job.UpdatedAt = time.Now().UTC().Format(time.RFC3339Nano)
	job.mu.Unlock()
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
		if _, err = tx.ExecContext(ctx, `INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at) VALUES(?,?,?,'file',?,?,?,?,'ready',?,?)`, ids.New(), directories[parentRel], filepath.Base(object.rel), object.key, object.manifest.Size, object.mimeType, object.manifest.ID(), now, now); err != nil {
			return "", "", err
		}
	}
	if err = tx.Commit(); err != nil {
		return "", "", err
	}
	return rootID, rootName, nil
}
