package server

import (
	"archive/zip"
	"context"
	"database/sql"
	"errors"
	"fmt"
	"io"
	"mime"
	"net/http"
	"path"
	"strings"
)

const maxBatchDownloadFiles = 1000

type batchDownloadInput struct {
	IDs []string `json:"ids"`
}

type batchDownloadEntry struct {
	file File
	name string
}

func decodeBatchDownloadIDs(w http.ResponseWriter, r *http.Request) ([]string, bool) {
	mediaType, _, err := mime.ParseMediaType(r.Header.Get("Content-Type"))
	if err != nil {
		problem(w, http.StatusBadRequest, "invalid content type")
		return nil, false
	}
	mediaType = strings.ToLower(mediaType)
	if mediaType == "application/x-www-form-urlencoded" {
		r.Body = http.MaxBytesReader(w, r.Body, maxJSONBody)
		if err := r.ParseForm(); err != nil {
			problem(w, http.StatusBadRequest, "invalid form request")
			return nil, false
		}
		if len(r.URL.Query()) != 0 {
			problem(w, http.StatusBadRequest, "invalid form request")
			return nil, false
		}
		for key := range r.PostForm {
			if key != "ids" {
				problem(w, http.StatusBadRequest, "invalid form request")
				return nil, false
			}
		}
		return append([]string(nil), r.PostForm["ids"]...), true
	}

	// Keep the JSON API, including the existing unknown-field protection. An
	// omitted Content-Type is accepted for compatibility with older clients.
	if mediaType != "" && mediaType != "application/json" {
		problem(w, http.StatusUnsupportedMediaType, "content type must be JSON or form encoded")
		return nil, false
	}
	var in batchDownloadInput
	if decodeJSON(w, r, &in) != nil {
		return nil, false
	}
	return in.IDs, true
}

// batchDownload handles logical file IDs only. Object keys deliberately never
// enter this request's public shape; they are resolved from the database after
// authentication and validation.
func (s *Server) batchDownload(w http.ResponseWriter, r *http.Request) {
	ids, ok := decodeBatchDownloadIDs(w, r)
	if !ok {
		return
	}
	if len(ids) == 0 {
		problem(w, http.StatusBadRequest, "at least one file id is required")
		return
	}
	if len(ids) > maxBatchDownloadFiles {
		problem(w, http.StatusBadRequest, fmt.Sprintf("a maximum of %d files can be downloaded at once", maxBatchDownloadFiles))
		return
	}

	entries := make([]batchDownloadEntry, 0, len(ids))
	seenIDs := make(map[string]struct{}, len(ids))
	usedNames := make(map[string]struct{}, len(ids))
	for _, id := range ids {
		if !validBatchDownloadID(id) {
			problem(w, http.StatusBadRequest, "invalid file id")
			return
		}
		if _, exists := seenIDs[id]; exists {
			problem(w, http.StatusBadRequest, "duplicate file id")
			return
		}
		seenIDs[id] = struct{}{}

		file, err := s.readableFile(r.Context(), id)
		if errors.Is(err, sql.ErrNoRows) {
			problem(w, http.StatusNotFound, "file not found")
			return
		}
		if err != nil {
			s.log.Error("batch download metadata read failed", "error", err)
			problem(w, http.StatusInternalServerError, "could not read file metadata")
			return
		}
		if file.Kind != "file" {
			problem(w, http.StatusBadRequest, "directories cannot be downloaded in a batch")
			return
		}
		if file.Status != "ready" {
			problem(w, http.StatusConflict, "file is not ready for download")
			return
		}
		if file.objectKey == "" {
			// A ready file without an object key is corrupt metadata. Keep this
			// failure before response headers so it cannot create a partial ZIP.
			s.log.Error("batch download file has no object key", "file_id", file.ID)
			problem(w, http.StatusInternalServerError, "file content is unavailable")
			return
		}

		name := uniqueBatchZipName(safeBatchZipName(file.Name), usedNames)
		usedNames[strings.ToLower(name)] = struct{}{}
		entries = append(entries, batchDownloadEntry{file: file, name: name})
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
