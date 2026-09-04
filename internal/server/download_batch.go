package server

import (
	"archive/zip"
	"context"
	"database/sql"
	"errors"
	"fmt"
	"io"
	"net/http"
	"path"
	"strings"
	"time"

	"github.com/go-chi/chi/v5"
)

const (
	maxBatchDownloadFiles  = 1000
	batchDownloadTokenTTL  = 2 * time.Minute
	maxBatchDownloadTokens = 256
)

type batchDownloadInput struct {
	IDs []string `json:"ids"`
}

type batchDownloadEntry struct {
	file File
	name string
}

type batchDownloadToken struct {
	user      string
	entries   []batchDownloadEntry
	expiresAt time.Time
}

type batchDownloadValidationError struct {
	status  int
	message string
}

func (e *batchDownloadValidationError) Error() string { return e.message }

var errBatchDownloadTokenLimit = errors.New("batch download token store is full")

func decodeBatchDownloadIDs(w http.ResponseWriter, r *http.Request) ([]string, bool) {
	var in batchDownloadInput
	if decodeJSON(w, r, &in) != nil {
		return nil, false
	}
	return in.IDs, true
}

func (s *Server) resolveBatchDownloadEntries(ctx context.Context, ids []string) ([]batchDownloadEntry, error) {
	if len(ids) == 0 {
		return nil, &batchDownloadValidationError{status: http.StatusBadRequest, message: "at least one file id is required"}
	}
	if len(ids) > maxBatchDownloadFiles {
		return nil, &batchDownloadValidationError{status: http.StatusBadRequest, message: fmt.Sprintf("a maximum of %d files can be downloaded at once", maxBatchDownloadFiles)}
	}

	entries := make([]batchDownloadEntry, 0, len(ids))
	seenIDs := make(map[string]struct{}, len(ids))
	usedNames := make(map[string]struct{}, len(ids))
	for _, id := range ids {
		if !validBatchDownloadID(id) {
			return nil, &batchDownloadValidationError{status: http.StatusBadRequest, message: "invalid file id"}
		}
		if _, exists := seenIDs[id]; exists {
			return nil, &batchDownloadValidationError{status: http.StatusBadRequest, message: "duplicate file id"}
		}
		seenIDs[id] = struct{}{}

		file, err := s.readableFile(ctx, id)
		if errors.Is(err, sql.ErrNoRows) {
			return nil, &batchDownloadValidationError{status: http.StatusNotFound, message: "file not found"}
		}
		if err != nil {
			if ctx.Err() != nil {
				return nil, ctx.Err()
			}
			s.log.Error("batch download metadata read failed", "error", err)
			return nil, &batchDownloadValidationError{status: http.StatusInternalServerError, message: "could not read file metadata"}
		}
		if file.Kind != "file" {
			return nil, &batchDownloadValidationError{status: http.StatusBadRequest, message: "directories cannot be downloaded in a batch"}
		}
		if file.Status != "ready" {
			return nil, &batchDownloadValidationError{status: http.StatusConflict, message: "file is not ready for download"}
		}
		if file.objectKey == "" {
			// A ready file without an object key is corrupt metadata. Keep this
			// failure before response headers so it cannot create a partial ZIP.
			s.log.Error("batch download file has no object key", "file_id", file.ID)
			return nil, &batchDownloadValidationError{status: http.StatusInternalServerError, message: "file content is unavailable"}
		}

		name := uniqueBatchZipName(safeBatchZipName(file.Name), usedNames)
		usedNames[strings.ToLower(name)] = struct{}{}
		entries = append(entries, batchDownloadEntry{file: file, name: name})
	}
	return entries, nil
}

func writeBatchDownloadError(w http.ResponseWriter, err error) {
	var validationErr *batchDownloadValidationError
	if errors.As(err, &validationErr) {
		problem(w, validationErr.status, validationErr.message)
		return
	}
	problem(w, http.StatusInternalServerError, "could not prepare batch download")
}

func (s *Server) prepareBatchDownload(w http.ResponseWriter, r *http.Request) {
	ids, ok := decodeBatchDownloadIDs(w, r)
	if !ok {
		return
	}
	entries, err := s.resolveBatchDownloadEntries(r.Context(), ids)
	if err != nil {
		if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
			return
		}
		writeBatchDownloadError(w, err)
		return
	}

	user, _ := r.Context().Value(userKey{}).(string)
	token, err := s.issueBatchDownloadToken(user, entries)
	if err != nil {
		if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
			return
		}
		if errors.Is(err, errBatchDownloadTokenLimit) {
			problem(w, http.StatusServiceUnavailable, "batch download preparation is busy; try again shortly")
			return
		}
		s.log.Error("batch download token creation failed", "error", err)
		problem(w, http.StatusInternalServerError, "could not prepare batch download")
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"token": token})
}

func (s *Server) issueBatchDownloadToken(user string, entries []batchDownloadEntry) (string, error) {
	now := time.Now()
	s.batchTokensMu.Lock()
	defer s.batchTokensMu.Unlock()
	if s.batchTokens == nil {
		s.batchTokens = make(map[string]batchDownloadToken)
	}
	for token, item := range s.batchTokens {
		if !now.Before(item.expiresAt) {
			delete(s.batchTokens, token)
		}
	}
	if len(s.batchTokens) >= maxBatchDownloadTokens {
		return "", errBatchDownloadTokenLimit
	}
	for {
		token, err := newShareToken()
		if err != nil {
			return "", err
		}
		if _, exists := s.batchTokens[token]; exists {
			continue
		}
		s.batchTokens[token] = batchDownloadToken{
			user: user, entries: append([]batchDownloadEntry(nil), entries...), expiresAt: now.Add(batchDownloadTokenTTL),
		}
		return token, nil
	}
}

func (s *Server) consumeBatchDownloadToken(user, token string) ([]batchDownloadEntry, bool) {
	if len(token) < 32 || len(token) > 128 {
		return nil, false
	}
	now := time.Now()
	s.batchTokensMu.Lock()
	defer s.batchTokensMu.Unlock()
	for key, item := range s.batchTokens {
		if !now.Before(item.expiresAt) {
			delete(s.batchTokens, key)
		}
	}
	item, ok := s.batchTokens[token]
	if !ok || item.user != user {
		return nil, false
	}
	delete(s.batchTokens, token)
	return item.entries, true
}

// batchDownload consumes an authenticated, short-lived token exactly once.
// The token contains only server-side metadata; clients can submit logical file
// IDs during preparation but never object keys or storage paths.
func (s *Server) batchDownload(w http.ResponseWriter, r *http.Request) {
	user, _ := r.Context().Value(userKey{}).(string)
	entries, ok := s.consumeBatchDownloadToken(user, chi.URLParam(r, "token"))
	if !ok {
		problem(w, http.StatusNotFound, "batch download token not found or expired")
		return
	}

	w.Header().Set("Content-Type", "application/zip")
	w.Header().Set("Content-Disposition", `attachment; filename="revaro-download.zip"`)
	w.WriteHeader(http.StatusOK)
	if err := s.writeBatchZip(r.Context(), w, entries); err != nil {
		if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) || r.Context().Err() != nil {
			s.log.Info("batch download cancelled", "error", err)
			return
		}
		s.log.Error("batch download stream failed", "error", err)
	}
}

func validBatchDownloadID(id string) bool {
	if id == "" || id == "." || id == ".." || len(id) > 128 || strings.TrimSpace(id) != id {
		return false
	}
	for _, r := range id {
		if r < 32 || r == 127 || r == '/' || r == '\\' {
			return false
		}
	}
	return true
}

// safeBatchZipName turns even legacy or manually-corrupted names into a
// single safe ZIP entry. Current file-name validation already rejects path
// separators, but this boundary must not rely on that invariant.
func safeBatchZipName(name string) string {
	name = path.Base(strings.ReplaceAll(name, `\`, `/`))
	if name == "." || name == ".." || name == "/" || name == "" || strings.ContainsAny(name, `/\`) {
		return "file"
	}
	var cleaned strings.Builder
	for _, r := range name {
		if r < 32 || r == 127 {
			cleaned.WriteByte('_')
			continue
		}
		cleaned.WriteRune(r)
	}
	name = cleaned.String()
	if name == "." || name == ".." || name == "" {
		return "file"
	}
	// A drive prefix is not a safe filename when the ZIP is extracted on
	// Windows, even after path.Base has removed ordinary path components.
	if len(name) >= 2 && ((name[0] >= 'a' && name[0] <= 'z') || (name[0] >= 'A' && name[0] <= 'Z')) && name[1] == ':' {
		name = "_" + name[2:]
		if name == "_" {
			return "file"
		}
	}
	return name
}

func uniqueBatchZipName(name string, used map[string]struct{}) string {
	if _, exists := used[strings.ToLower(name)]; !exists {
		return name
	}
	ext := path.Ext(name)
	if ext == name {
		ext = ""
	}
	stem := strings.TrimSuffix(name, ext)
	for index := 2; ; index++ {
		candidate := fmt.Sprintf("%s (%d)%s", stem, index, ext)
		if _, exists := used[strings.ToLower(candidate)]; !exists {
			return candidate
		}
	}
}

type batchDownloadContextReader struct {
	ctx context.Context
	r   io.Reader
}

func (r batchDownloadContextReader) Read(p []byte) (int, error) {
	select {
	case <-r.ctx.Done():
		return 0, r.ctx.Err()
	default:
		return r.r.Read(p)
	}
}

func (s *Server) writeBatchZip(ctx context.Context, w io.Writer, entries []batchDownloadEntry) (err error) {
	archive := zip.NewWriter(w)
	defer func() {
		if closeErr := archive.Close(); err == nil {
			err = closeErr
		}
	}()

	for _, item := range entries {
		if err := ctx.Err(); err != nil {
			return err
		}
		source, err := s.objects.Open(ctx, item.file.objectKey)
		if err != nil {
			return err
		}
		entry, err := archive.Create(item.name)
		if err != nil {
			_ = source.Close()
			return err
		}
		_, copyErr := io.Copy(entry, batchDownloadContextReader{ctx: ctx, r: source})
		closeErr := source.Close()
		if copyErr != nil {
			return copyErr
		}
		if closeErr != nil {
			return closeErr
		}
	}
	return nil
}
