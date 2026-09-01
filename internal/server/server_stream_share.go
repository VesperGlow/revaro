package server

import (
	"context"
	"crypto/rand"
	"database/sql"
	"encoding/base64"
	"errors"
	"io"
	"mime"
	"net/http"
	"net/url"
	"path/filepath"
	"strings"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
	"github.com/VesperGlow/revaro/internal/storage"
	"github.com/go-chi/chi/v5"
)

// serveFileContent redirects opaque blobs to S3 so Range, cancellation and
// backpressure stay end-to-end.
func (s *Server) serveFileContent(w http.ResponseWriter, r *http.Request, f File, inline bool) {
	mimeType := safeDeliveryMime(responseMime(f))
	if mimeType == "application/octet-stream" {
		inline = false
	}
	if s.cfg.ProxyTransfers {
		rc, err := s.objects.OpenSeek(storage.WithDynamicReadAhead(r.Context()), f.objectKey)
		if err != nil {
			problem(w, http.StatusBadGateway, "object storage read failed")
			return
		}
		defer rc.Close()
		w.Header().Set("Content-Type", mimeType)
		disposition := "attachment"
		if inline {
			disposition = "inline"
		}
		w.Header().Set("Content-Disposition", disposition+"; filename*=UTF-8''"+strings.ReplaceAll(url.PathEscape(f.Name), "+", "%20"))
		http.ServeContent(w, r, f.Name, time.Time{}, rc)
		return
	}
	u, err := s.objects.PresignGet(r.Context(), f.objectKey, f.Name, mimeType, inline, s.cfg.PresignExpires)
	if err != nil {
		problem(w, http.StatusBadGateway, "could not create download URL")
		return
	}
	http.Redirect(w, r, u, http.StatusFound)
}
func (s *Server) readContent(ctx context.Context, f File) ([]byte, error) {
	return s.objects.Get(ctx, f.objectKey, maxDocumentBytes)
}

func (s *Server) storeBlob(ctx context.Context, body io.Reader, size int64, mimeType string) (string, storage.ObjectInfo, error) {
	key := storage.BlobKey(ids.New())
	info, err := s.objects.Stream(ctx, key, mimeType, body, size)
	if err != nil {
		return "", storage.ObjectInfo{}, err
	}
	return key, info, nil
}

// discardBlob promptly rolls back an object uploaded before its metadata
// transaction failed. Periodic GC remains the fallback if S3 is unavailable.
func (s *Server) discardBlob(key string) {
	s.discardBlobs([]string{key})
}

func (s *Server) discardBlobs(keys []string) {
	filtered := make([]string, 0, len(keys))
	for _, key := range keys {
		if key != "" {
			filtered = append(filtered, key)
		}
	}
	if len(filtered) == 0 {
		return
	}
	s.runBackground(func() {
		parent := s.audioHLSCtx
		if parent == nil {
			parent = context.Background()
		}
		ctx, cancel := context.WithTimeout(parent, 30*time.Second)
		defer cancel()
		for len(filtered) > 0 {
			batch := filtered
			if len(batch) > 1000 {
				batch = filtered[:1000]
			}
			if err := s.objects.DeleteMany(ctx, batch, "metadata commit rollback"); err != nil {
				s.log.Warn("uncommitted blob cleanup failed", "objects", len(batch), "error", err)
				now := time.Now().UTC().Format(time.RFC3339Nano)
				for _, key := range batch {
					_, _ = s.db.ExecContext(context.Background(), `INSERT INTO object_cleanup(object_key,reason,created_at,updated_at) VALUES(?,'metadata commit rollback',?,?) ON CONFLICT(object_key) DO UPDATE SET retry_count=retry_count+1,updated_at=excluded.updated_at`, key, now, now)
				}
				return
			}
			filtered = filtered[len(batch):]
		}
	})
}

func (s *Server) CleanupObjects(ctx context.Context) {
	rows, err := s.db.QueryContext(ctx, `SELECT object_key FROM object_cleanup ORDER BY updated_at LIMIT 1000`)
	if err != nil {
		return
	}
	keys := []string{}
	for rows.Next() {
		var key string
		if rows.Scan(&key) == nil {
			keys = append(keys, key)
		}
	}
	rows.Close()
	for _, key := range keys {
		if err := s.objects.Delete(ctx, key, "deferred object cleanup"); err == nil || storage.IsNotFound(err) {
			_, _ = s.db.ExecContext(ctx, `DELETE FROM object_cleanup WHERE object_key=?`, key)
		} else {
			_, _ = s.db.ExecContext(ctx, `UPDATE object_cleanup SET retry_count=retry_count+1,updated_at=? WHERE object_key=?`, time.Now().UTC().Format(time.RFC3339Nano), key)
		}
	}
}

func (s *Server) queueObjectCleanup(ctx context.Context, key, reason string) {
	if key == "" {
		return
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	_, _ = s.db.ExecContext(ctx, `INSERT INTO object_cleanup(object_key,reason,created_at,updated_at) VALUES(?,?,?,?) ON CONFLICT(object_key) DO UPDATE SET reason=excluded.reason,retry_count=retry_count+1,updated_at=excluded.updated_at`, key, reason, now, now)
}

func responseMime(f File) string {
	if f.MimeType != "" && f.MimeType != "application/octet-stream" {
		return f.MimeType
	}
	switch strings.ToLower(filepath.Ext(f.Name)) {
	case ".jpg", ".jpeg":
		return "image/jpeg"
	case ".png":
		return "image/png"
	case ".gif":
		return "image/gif"
	case ".webp":
		return "image/webp"
	case ".avif":
		return "image/avif"
	case ".mp4":
		return "video/mp4"
	case ".webm":
		return "video/webm"
	case ".ogv":
		return "video/ogg"
	case ".mov":
		return "video/quicktime"
	case ".m4v":
		return "video/x-m4v"
	case ".mkv":
		return "video/x-matroska"
	case ".avi":
		return "video/x-msvideo"
	case ".flv":
		return "video/x-flv"
	case ".wmv":
		return "video/x-ms-wmv"
	case ".mpg", ".mpeg":
		return "video/mpeg"
	case ".ts", ".m2ts", ".mts":
		return "video/mp2t"
	case ".mp3":
		return "audio/mpeg"
	case ".wav":
		return "audio/wav"
	case ".ogg", ".oga":
		return "audio/ogg"
	case ".m4a":
		return "audio/mp4"
	case ".aac":
		return "audio/aac"
	case ".flac":
		return "audio/flac"
	case ".md", ".markdown":
		return "text/markdown; charset=utf-8"
	case ".yaml", ".yml":
		return "application/yaml; charset=utf-8"
	case ".json":
		return "application/json; charset=utf-8"
	case ".toml":
		return "application/toml; charset=utf-8"
	case ".csv":
		return "text/csv; charset=utf-8"
	case ".txt", ".conf", ".ini", ".log":
		return "text/plain; charset=utf-8"
	default:
		return f.MimeType
	}
}

// Active web formats must never be served with an executable MIME type from
// the application origin. They remain downloadable as opaque bytes.
func safeDeliveryMime(value string) string {
	mediaType, _, err := mime.ParseMediaType(value)
	if err != nil {
		return "application/octet-stream"
	}
	switch strings.ToLower(mediaType) {
	case "text/html", "application/xhtml+xml", "image/svg+xml", "application/xml", "text/xml",
		"application/javascript", "text/javascript", "application/ecmascript", "text/ecmascript":
		return "application/octet-stream"
	default:
		return value
	}
}

func newShareToken() (string, error) {
	b := make([]byte, 32)
	if _, err := rand.Read(b); err != nil {
		return "", err
	}
	return base64.RawURLEncoding.EncodeToString(b), nil
}

func (s *Server) getShare(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	var token, created string
	err := s.db.QueryRowContext(r.Context(), `SELECT s.token,s.created_at FROM shares s JOIN files f ON f.id=s.file_id WHERE s.file_id=? AND f.kind='file' AND f.status='ready' AND f.deleted_at IS NULL`, id).Scan(&token, &created)
	if errors.Is(err, sql.ErrNoRows) {
		writeJSON(w, http.StatusOK, map[string]any{"active": false})
		return
	}
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not read share")
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"active": true, "url": s.cfg.BaseURL + "/s/" + token, "created_at": created})
}

func (s *Server) createShare(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	f, err := s.file(r.Context(), id)
	if err != nil || f.Kind != "file" || f.Status != "ready" {
		problem(w, http.StatusNotFound, "ready file not found")
		return
	}
	token, err := newShareToken()
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not generate share link")
		return
	}
	created := time.Now().UTC().Format(time.RFC3339Nano)
	_, err = s.db.ExecContext(r.Context(), `INSERT INTO shares(file_id,token,created_at) VALUES(?,?,?) ON CONFLICT(file_id) DO UPDATE SET token=excluded.token,created_at=excluded.created_at`, id, token, created)
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not create share link")
		return
	}
	s.log.Info("file share created", "file_id", id)
	writeJSON(w, http.StatusCreated, map[string]any{"active": true, "url": s.cfg.BaseURL + "/s/" + token, "created_at": created})
}

func (s *Server) revokeShare(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if _, err := s.db.ExecContext(r.Context(), `DELETE FROM shares WHERE file_id=?`, id); err != nil {
		problem(w, http.StatusInternalServerError, "could not revoke share link")
		return
	}
	s.log.Info("file share revoked", "file_id", id)
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) publicShare(w http.ResponseWriter, r *http.Request) {
	token := chi.URLParam(r, "token")
	if len(token) < 32 || len(token) > 128 {
		problem(w, http.StatusNotFound, "share not found")
		return
	}
	var f File
	var parent, mime, etag sql.NullString
	err := s.db.QueryRowContext(r.Context(), `SELECT f.id,f.parent_id,f.name,f.kind,COALESCE(f.object_key,''),f.size,f.mime_type,f.etag,f.status,f.created_at,f.updated_at FROM shares s JOIN files f ON f.id=s.file_id WHERE s.token=? AND f.kind='file' AND f.status='ready' AND f.deleted_at IS NULL`, token).Scan(&f.ID, &parent, &f.Name, &f.Kind, &f.objectKey, &f.Size, &mime, &etag, &f.Status, &f.CreatedAt, &f.UpdatedAt)
	if errors.Is(err, sql.ErrNoRows) {
		problem(w, http.StatusNotFound, "share not found")
		return
	}
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not open share")
		return
	}
	f.MimeType = mime.String
	w.Header().Set("Cache-Control", "no-store")
	w.Header().Set("Referrer-Policy", "no-referrer")
	w.Header().Set("X-Robots-Tag", "noindex, nofollow, noarchive")
	w.Header().Set("Content-Security-Policy", "sandbox; default-src 'none'; base-uri 'none'; form-action 'none'")
	select {
	case s.shareSlots <- struct{}{}:
		defer func() { <-s.shareSlots }()
	default:
		w.Header().Set("Retry-After", "5")
		problem(w, http.StatusTooManyRequests, "public downloads are busy; try again shortly")
		return
	}
	// 分享 URL 等同于访问凭据，记录每次访问（token 只记前缀掩码），
	// 便于发现泄露后定位访问来源。
	s.log.Info("public share served", "file_id", f.ID, "file", f.Name, "token_prefix", token[:min(len(token), 8)])
	s.serveFileContent(w, r, f, isPreviewable(f))
}
