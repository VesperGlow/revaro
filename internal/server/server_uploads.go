package server

import (
	"context"
	"errors"
	"mime"
	"net/http"
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

type createUploadInput struct {
	ParentID string `json:"parent_id"`
	Name     string `json:"name"`
	Size     int64  `json:"size"`
	MimeType string `json:"mime_type"`
}

const multipartUploadThreshold = int64(16 << 20)
const defaultMultipartPartSize = int64(16 << 20)

func multipartPartSize(size int64) int64 {
	partSize := defaultMultipartPartSize
	if size > partSize*10000 {
		// Keep safely below S3's 10,000-part limit and round to MiB so the
		// browser can slice without awkward byte boundaries.
		partSize = ((size+9999)/10000 + (1 << 20) - 1) / (1 << 20) * (1 << 20)
	}
	return partSize
}

func (s *Server) createUpload(w http.ResponseWriter, r *http.Request) {
	var in createUploadInput
	if decodeJSON(w, r, &in) != nil {
		return
	}
	if err := validateName(in.Name); err != nil {
		problem(w, 400, err.Error())
		return
	}
	if in.Size < 0 || in.Size > maxLogicalFileSize {
		problem(w, 400, "invalid file size")
		return
	}
	if len(in.MimeType) > 255 {
		problem(w, 400, "mime type is too long")
		return
	}
	if in.MimeType == "" {
		in.MimeType = "application/octet-stream"
	}
	if _, _, err := mime.ParseMediaType(in.MimeType); err != nil {
		problem(w, 400, "mime type is invalid")
		return
	}
	p, err := s.file(r.Context(), in.ParentID)
	if err != nil || p.Kind != "directory" || p.Status != "ready" {
		problem(w, 400, "parent directory is invalid")
		return
	}
	fileID, uploadID := ids.New(), ids.New()
	objectKey := storage.BlobKey(ids.New())
	mode := "single"
	partSize := max(in.Size, int64(1))
	var s3UploadID, uploadURL string
	var partCount int
	if in.Size >= multipartUploadThreshold {
		mode = "multipart"
		partSize = multipartPartSize(in.Size)
		partCount, err = storage.ValidMultipartPartCount(in.Size, partSize)
		if err == nil {
			s3UploadID, err = s.objects.CreateMultipart(r.Context(), objectKey, in.MimeType)
		}
	} else {
		uploadURL, err = s.objects.PresignPut(r.Context(), objectKey, in.MimeType, s.cfg.PresignExpires)
	}
	if err != nil {
		s.log.Error("blob upload initialization failed", "file", in.Name, "mode", mode, "error", err)
		problem(w, http.StatusBadGateway, "object storage could not initialize the upload")
		return
	}
	now := time.Now().UTC()
	expires := now.Add(s.cfg.UploadExpires)
	tx, err := s.db.BeginTx(r.Context(), nil)
	if err != nil {
		problem(w, 500, "database error")
		return
	}
	_, err = tx.ExecContext(r.Context(), `INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,status,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?)`, fileID, in.ParentID, in.Name, "file", objectKey, in.Size, in.MimeType, "pending", now.Format(time.RFC3339Nano), now.Format(time.RFC3339Nano))
	if err == nil {
		_, err = tx.ExecContext(r.Context(), `INSERT INTO uploads(id,file_id,mode,object_key,s3_upload_id,part_size,expected_size,mime_type,status,created_at,expires_at) VALUES(?,?,?,?,?,?,?,?, 'pending',?,?)`, uploadID, fileID, mode, objectKey, nullString(s3UploadID), partSize, in.Size, in.MimeType, now.Format(time.RFC3339Nano), expires.Format(time.RFC3339Nano))
	}
	if err != nil {
		tx.Rollback()
		if s3UploadID != "" {
			abortCtx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
			_ = s.objects.AbortMultipart(abortCtx, objectKey, s3UploadID)
			cancel()
		}
		if isConflict(err) {
			problem(w, 409, "an item with that name already exists")
		} else {
			problem(w, 500, "could not create upload")
		}
		return
	}
	if err = tx.Commit(); err != nil {
		if s3UploadID != "" {
			abortCtx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
			_ = s.objects.AbortMultipart(abortCtx, objectKey, s3UploadID)
			cancel()
		}
		problem(w, 500, "could not create upload")
		return
	}
	s.log.Info("blob upload created", "file", in.Name, "size", in.Size, "mode", mode, "object_key", objectKey, "part_size", partSize, "parts", partCount)
	if taskID, taskErr := s.ensureTask(r.Context(), "upload", "upload", uploadID, "uploading"); taskErr == nil {
		_, _ = s.db.ExecContext(r.Context(), `INSERT OR IGNORE INTO task_files(task_id,file_id,role) VALUES(?,?,'input')`, taskID, fileID)
	}
	writeJSON(w, 201, map[string]any{
		"upload_id": uploadID,
		"file_id":   fileID,
		"mode":      mode, "url": uploadURL, "part_size": partSize, "part_count": partCount,
		"expires_at": expires.Format(time.RFC3339Nano),
	})
}

func nullString(value string) any {
	if value == "" {
		return nil
	}
	return value
}

type uploadRecord struct {
	ID, FileID, Mode, ObjectKey, S3UploadID, MimeType, Status, ExpiresAt string
	PartSize, ExpectedSize                                               int64
}

func (u uploadRecord) expired(now time.Time) bool {
	t, err := time.Parse(time.RFC3339Nano, u.ExpiresAt)
	return err != nil || !t.After(now)
}

func (s *Server) upload(ctx context.Context, id string) (uploadRecord, error) {
	var u uploadRecord
	err := s.db.QueryRowContext(ctx, `SELECT id,file_id,mode,object_key,COALESCE(s3_upload_id,''),part_size,expected_size,mime_type,status,expires_at FROM uploads WHERE id=?`, id).Scan(&u.ID, &u.FileID, &u.Mode, &u.ObjectKey, &u.S3UploadID, &u.PartSize, &u.ExpectedSize, &u.MimeType, &u.Status, &u.ExpiresAt)
	return u, err
}

func (s *Server) getUpload(w http.ResponseWriter, r *http.Request) {
	u, err := s.upload(r.Context(), chi.URLParam(r, "id"))
	if err != nil {
		problem(w, http.StatusNotFound, "upload not found")
		return
	}
	rows, err := s.db.QueryContext(r.Context(), `SELECT part_number,size,etag,COALESCE(content_hash,'') FROM upload_parts WHERE upload_id=? ORDER BY part_number`, u.ID)
	if err != nil {
		problem(w, 500, "could not read upload parts")
		return
	}
	defer rows.Close()
	parts := []map[string]any{}
	for rows.Next() {
		var number int32
		var size int64
		var etag, hash string
		if rows.Scan(&number, &size, &etag, &hash) != nil {
			problem(w, 500, "could not read upload parts")
			return
		}
		parts = append(parts, map[string]any{"part_number": number, "size": size, "etag": etag, "content_hash": hash})
	}
	partCount, _ := storage.ValidMultipartPartCount(u.ExpectedSize, u.PartSize)
	var uploadURL string
	if u.Mode == "single" && u.Status == "pending" {
		uploadURL, err = s.objects.PresignPut(r.Context(), u.ObjectKey, u.MimeType, s.cfg.PresignExpires)
		if err != nil {
			problem(w, 502, "object storage could not resume the upload")
			return
		}
	}
	writeJSON(w, 200, map[string]any{"upload_id": u.ID, "file_id": u.FileID, "mode": u.Mode, "url": uploadURL, "part_size": u.PartSize, "part_count": partCount, "expected_size": u.ExpectedSize, "mime_type": u.MimeType, "status": u.Status, "expires_at": u.ExpiresAt, "parts": parts})
}

func (s *Server) recordUploadPart(w http.ResponseWriter, r *http.Request) {
	u, err := s.upload(r.Context(), chi.URLParam(r, "id"))
	if err != nil || u.Status != "pending" || u.Mode != "multipart" || u.expired(time.Now().UTC()) {
		problem(w, 404, "pending upload not found")
		return
	}
	number64, err := strconv.ParseInt(chi.URLParam(r, "part"), 10, 32)
	count, _ := storage.ValidMultipartPartCount(u.ExpectedSize, u.PartSize)
	if err != nil || number64 < 1 || number64 > int64(count) {
		problem(w, 400, "invalid multipart part number")
		return
	}
	var in struct {
		ETag        string `json:"etag"`
		Size        int64  `json:"size"`
		ContentHash string `json:"content_hash"`
	}
	if decodeJSON(w, r, &in) != nil {
		return
	}
	in.ETag = strings.TrimSpace(in.ETag)
	expected := u.PartSize
	if int(number64) == count {
		expected = u.ExpectedSize - int64(count-1)*u.PartSize
	}
	if in.ETag == "" || in.Size != expected || len(in.ContentHash) > 128 {
		problem(w, 400, "invalid uploaded part acknowledgement")
		return
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	_, err = s.db.ExecContext(r.Context(), `INSERT INTO upload_parts(upload_id,part_number,size,etag,content_hash,completed_at) VALUES(?,?,?,?,NULLIF(?,''),?) ON CONFLICT(upload_id,part_number) DO UPDATE SET size=excluded.size,etag=excluded.etag,content_hash=excluded.content_hash,completed_at=excluded.completed_at`, u.ID, number64, in.Size, in.ETag, in.ContentHash, now)
	if err != nil {
		problem(w, 500, "could not record uploaded part")
		return
	}
	s.jobs.Changed()
	w.WriteHeader(204)
}

func (s *Server) uploadParts(w http.ResponseWriter, r *http.Request) {
	u, err := s.upload(r.Context(), chi.URLParam(r, "id"))
	if err != nil || u.Status != "pending" || u.Mode != "multipart" || u.expired(time.Now().UTC()) {
		problem(w, 404, "pending upload not found")
		return
	}
	var body struct {
		PartNumbers []int32 `json:"part_numbers"`
	}
	if decodeJSON(w, r, &body) != nil {
		return
	}
	if len(body.PartNumbers) == 0 || len(body.PartNumbers) > 100 {
		problem(w, 400, "request between 1 and 100 upload parts")
		return
	}
	partCount, _ := storage.ValidMultipartPartCount(u.ExpectedSize, u.PartSize)
	seen := make(map[int32]bool, len(body.PartNumbers))
	parts := make([]map[string]any, len(body.PartNumbers))
	for i, partNumber := range body.PartNumbers {
		if partNumber < 1 || int(partNumber) > partCount || seen[partNumber] {
			problem(w, 400, "invalid multipart part number")
			return
		}
		seen[partNumber] = true
		url, signErr := s.objects.PresignPart(r.Context(), u.ObjectKey, u.S3UploadID, partNumber, s.cfg.PresignExpires)
		if signErr != nil {
			s.log.Error("multipart part signing failed", "upload", u.ID, "part", partNumber, "error", signErr)
			problem(w, 502, "object storage could not prepare upload parts")
			return
		}
		parts[i] = map[string]any{"part_number": partNumber, "url": url}
	}
	writeJSON(w, 200, map[string]any{"parts": parts})
}

func (s *Server) completeUpload(w http.ResponseWriter, r *http.Request) {
	u, err := s.upload(r.Context(), chi.URLParam(r, "id"))
	if err != nil || (u.Status != "pending" && u.Status != "completed") || (u.Status == "pending" && u.expired(time.Now().UTC())) {
		problem(w, 404, "pending upload not found")
		return
	}
	var body struct {
		Parts []storage.CompletedPart `json:"parts"`
	}
	if decodeJSONLimit(w, r, &body, 2<<20) != nil {
		return
	}
	if u.Status == "completed" {
		info, headErr := s.objects.Stat(r.Context(), u.ObjectKey)
		if headErr != nil || info.Size != u.ExpectedSize {
			problem(w, 409, "completed upload object is unavailable")
			return
		}
		f, fileErr := s.file(r.Context(), u.FileID)
		if fileErr != nil {
			problem(w, 500, "completed upload metadata is unavailable")
			return
		}
		writeJSON(w, 200, f)
		return
	}
	var info storage.ObjectInfo
	if u.Mode == "multipart" {
		partCount, _ := storage.ValidMultipartPartCount(u.ExpectedSize, u.PartSize)
		if len(body.Parts) == 0 {
			rows, qerr := s.db.QueryContext(r.Context(), `SELECT part_number,etag FROM upload_parts WHERE upload_id=? ORDER BY part_number`, u.ID)
			if qerr != nil {
				problem(w, 500, "could not read uploaded parts")
				return
			}
			defer rows.Close()
			for rows.Next() {
				var p storage.CompletedPart
				if rows.Scan(&p.PartNumber, &p.ETag) != nil {
					problem(w, 500, "could not read uploaded parts")
					return
				}
				body.Parts = append(body.Parts, p)
			}
		}
		if len(body.Parts) != partCount {
			problem(w, 400, "multipart completion list is incomplete")
			return
		}
		sort.Slice(body.Parts, func(i, j int) bool { return body.Parts[i].PartNumber < body.Parts[j].PartNumber })
		for i, part := range body.Parts {
			if part.PartNumber != int32(i+1) || strings.TrimSpace(part.ETag) == "" {
				problem(w, 400, "multipart completion list is invalid")
				return
			}
		}
		info, err = s.objects.CompleteMultipart(r.Context(), u.ObjectKey, u.S3UploadID, body.Parts)
	} else {
		if len(body.Parts) != 0 {
			problem(w, 400, "single upload must not include multipart parts")
			return
		}
		info, err = s.objects.Stat(r.Context(), u.ObjectKey)
	}
	if err != nil {
		s.log.Error("blob upload completion failed", "upload", u.ID, "mode", u.Mode, "error", err)
		problem(w, 502, "object storage could not complete the upload")
		return
	}
	if info.Size != u.ExpectedSize {
		_ = s.objects.Delete(r.Context(), u.ObjectKey, "upload size mismatch")
		problem(w, 400, "uploaded object size does not match the declared size")
		return
	}
	s.updateTask(r.Context(), "upload", u.ID, "running", "verifying", 99, "")
	releaseIO, resourceErr := s.tasks.IO(r.Context())
	if resourceErr != nil {
		s.updateTask(r.Context(), "upload", u.ID, "retrying", "verifying", 99, resourceErr.Error())
		problem(w, 499, "upload verification was cancelled")
		return
	}
	_, contentHash, hashErr := s.objects.Verify(r.Context(), u.ObjectKey, u.ExpectedSize, "")
	releaseIO()
	if hashErr != nil {
		s.updateTask(r.Context(), "upload", u.ID, "failed", "verifying", 99, hashErr.Error())
		problem(w, 502, "uploaded object integrity check failed")
		return
	}
	if err := s.finalizeUpload(r.Context(), u, info.ETag, contentHash); err != nil {
		s.log.Error("upload metadata commit failed", "upload", u.ID, "error", err)
		s.updateTask(r.Context(), "upload", u.ID, "retrying", "committing", 99, publicError(err, "文件已上传，正在等待元数据重试"))
		problem(w, 500, "object stored but metadata finalization failed")
		return
	}
	s.updateTask(r.Context(), "upload", u.ID, "completed", "completed", 100, "")
	f, _ := s.file(r.Context(), u.FileID)
	s.log.Info("blob upload completed", "file", f.Name, "mode", u.Mode, "size", info.Size, "object_key", u.ObjectKey)
	s.scheduleMediaAnalysis(f)
	s.scheduleVideoThumbnail(f)
	writeJSON(w, 200, f)
}

func (s *Server) finalizeUpload(ctx context.Context, u uploadRecord, etag, contentHash string) error {
	tx, err := s.db.BeginTx(ctx, nil)
	if err == nil {
		_, err = tx.ExecContext(ctx, `UPDATE files SET status='ready',size=?,etag=?,content_hash=?,hash_algorithm=?,updated_at=? WHERE id=? AND status='pending' AND object_key=?`, u.ExpectedSize, etag, contentHash, contentHashAlgorithm, time.Now().UTC().Format(time.RFC3339Nano), u.FileID, u.ObjectKey)
	}
	if err == nil {
		_, err = tx.ExecContext(ctx, `UPDATE uploads SET status='completed',content_hash=?,completed_at=? WHERE id=? AND status='pending'`, contentHash, time.Now().UTC().Format(time.RFC3339Nano), u.ID)
	}
	if err != nil {
		if tx != nil {
			tx.Rollback()
		}
		return err
	}
	if err = tx.Commit(); err != nil {
		return err
	}
	return nil
}
func (s *Server) abortUpload(w http.ResponseWriter, r *http.Request) {
	u, err := s.upload(r.Context(), chi.URLParam(r, "id"))
	if err != nil || u.Status != "pending" {
		problem(w, 404, "pending upload not found")
		return
	}
	if err := s.cleanupPendingUploadObject(r.Context(), u); err != nil {
		s.log.Warn("blob upload object cleanup failed", "upload", u.ID, "object_key", u.ObjectKey, "error", err)
	}
	s.updateTask(r.Context(), "upload", u.ID, "cancelled", "cancelled", 0, "")
	tx, err := s.db.BeginTx(r.Context(), nil)
	if err == nil {
		_, err = tx.ExecContext(r.Context(), `UPDATE uploads SET status='aborted' WHERE id=?`, u.ID)
	}
	if err == nil {
		_, err = tx.ExecContext(r.Context(), `DELETE FROM files WHERE id=? AND status='pending'`, u.FileID)
	}
	if err != nil {
		if tx != nil {
			tx.Rollback()
		}
		problem(w, 500, "could not clean upload metadata")
		return
	}
	if err = tx.Commit(); err != nil {
		problem(w, 500, "could not clean upload metadata")
		return
	}
	w.WriteHeader(204)
}

// cleanupPendingUploadObject handles both an active multipart upload and the
// less common case where CompleteMultipart succeeded but SQLite finalization
// did not. Deleting the key after abort is safe when no object was committed.
func (s *Server) cleanupPendingUploadObject(ctx context.Context, u uploadRecord) error {
	if u.Mode == "multipart" && u.S3UploadID != "" {
		if err := s.objects.AbortMultipart(ctx, u.ObjectKey, u.S3UploadID); err != nil {
			s.log.Debug("multipart abort skipped", "upload", u.ID, "error", err)
		}
	}
	return s.objects.Delete(ctx, u.ObjectKey, "aborted upload")
}

func (s *Server) failUpload(ctx context.Context, uploadID, fileID string) {
	_, _ = s.db.ExecContext(ctx, `UPDATE uploads SET status='failed' WHERE id=?`, uploadID)
	_, _ = s.db.ExecContext(ctx, `UPDATE files SET status='failed',updated_at=? WHERE id=?`, time.Now().UTC().Format(time.RFC3339Nano), fileID)
	s.updateTask(ctx, "upload", uploadID, "failed", "uploading", 0, "upload failed")
}

func (s *Server) CleanupExpiredUploads(ctx context.Context) {
	rows, err := s.db.QueryContext(ctx, `SELECT id FROM uploads WHERE status='pending' AND expires_at<=?`, time.Now().UTC().Format(time.RFC3339Nano))
	if err != nil {
		s.log.Error("scan stale uploads failed", "error", err)
		return
	}
	var ids []string
	for rows.Next() {
		var id string
		if rows.Scan(&id) == nil {
			ids = append(ids, id)
		}
	}
	rows.Close()
	for _, id := range ids {
		u, err := s.upload(ctx, id)
		if err != nil {
			continue
		}
		err = s.cleanupPendingUploadObject(ctx, u)
		if err != nil {
			s.log.Warn("stale blob upload cleanup failed", "upload", id, "error", err)
			continue
		}
		tx, err := s.db.BeginTx(ctx, nil)
		if err != nil {
			continue
		}
		_, err = tx.ExecContext(ctx, `UPDATE uploads SET status='aborted' WHERE id=?`, id)
		if err == nil {
			_, err = tx.ExecContext(ctx, `DELETE FROM files WHERE id=? AND status='pending'`, u.FileID)
		}
		if err == nil {
			err = tx.Commit()
		} else {
			tx.Rollback()
		}
		if err == nil {
			s.log.Info("stale upload cleaned", "upload", id)
			s.updateTask(ctx, "upload", id, "cancelled", "expired", 0, "upload session expired")
		}
	}
}

// parallel runs fn over every index concurrently, bounded by limit
// workers, and returns the first error (other workers finish their
// in-flight item before the function returns).
func parallel(indices []int, limit int, fn func(int) error) error {
	if len(indices) == 0 {
		return nil
	}
	if limit < 1 {
		limit = 1
	}
	if limit > len(indices) {
		limit = len(indices)
	}
	queue := make(chan int)
	var wg sync.WaitGroup
	var mu sync.Mutex
	var firstErr error
	for i := 0; i < limit; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for idx := range queue {
				if err := fn(idx); err != nil {
					mu.Lock()
					if firstErr == nil {
						firstErr = err
					}
					mu.Unlock()
				}
			}
		}()
	}
	for _, idx := range indices {
		queue <- idx
	}
	close(queue)
	wg.Wait()
	return firstErr
}

// referencedStorageKeys returns every content object and derived thumbnail the
// metadata can still reach, across active and trashed file states.
func (s *Server) referencedStorageKeys(ctx context.Context) (map[string]bool, map[string]bool, error) {
	rows, err := s.db.QueryContext(ctx, `SELECT object_key,name FROM files WHERE kind='file' AND object_key IS NOT NULL AND object_key <> ''`)
	if err != nil {
		return nil, nil, err
	}
	defer rows.Close()
	objects := map[string]bool{}
	thumbnails := map[string]bool{}
	for rows.Next() {
		var key, name string
		if err := rows.Scan(&key, &name); err != nil {
			return nil, nil, err
		}
		objects[key] = true
		if canHaveThumbnail(name) {
			if videoExts[strings.ToLower(filepath.Ext(name))] {
				thumbnails[videoThumbnailKey(key)] = true
			} else {
				thumbnails[imageThumbnailKey(key)] = true
			}
		}
	}
	if err := rows.Err(); err != nil {
		return nil, nil, err
	}
	if err := rows.Close(); err != nil {
		return nil, nil, err
	}
	mediaRows, err := s.db.QueryContext(ctx, `SELECT am.stream_object_key,f.object_key,am.has_cover FROM audio_media am JOIN files f ON f.id=am.file_id`)
	if err != nil {
		return nil, nil, err
	}
	defer mediaRows.Close()
	for mediaRows.Next() {
		var streamKey, masterKey string
		var hasCover bool
		if err := mediaRows.Scan(&streamKey, &masterKey, &hasCover); err != nil {
			return nil, nil, err
		}
		objects[streamKey] = true
		if hasCover {
			thumbnails[audioThumbnailKey(masterKey)] = true
		}
	}
	if err := mediaRows.Err(); err != nil {
		return nil, nil, err
	}
	return objects, thumbnails, nil
}

// CollectGarbage deletes unreferenced blobs and derived thumbnails after the
// upload grace period.
func (s *Server) CollectGarbage(ctx context.Context) {
	cutoff := time.Now().UTC().Add(-(s.cfg.UploadExpires + time.Hour))
	referenced, referencedThumbnails, err := s.referencedStorageKeys(ctx)
	if err != nil {
		s.log.Error("GC metadata scan failed", "error", err)
		return
	}
	deletedBlobs := s.collectUnreferencedPrefix(ctx, "blobs/", cutoff, referenced)
	deletedThumbnails := s.collectUnreferencedPrefix(ctx, "thumbs/", cutoff, referencedThumbnails)
	if deletedBlobs+deletedThumbnails > 0 {
		s.log.Info("garbage collection finished", "blobs", deletedBlobs, "thumbnails", deletedThumbnails)
	}
}

func (s *Server) collectUnreferencedPrefix(ctx context.Context, prefix string, cutoff time.Time, referenced map[string]bool) int {
	deleted := 0
	err := s.objects.WalkPrefix(ctx, prefix, func(objects []storage.ObjectRef) error {
		keys := make([]string, 0, len(objects))
		for _, object := range objects {
			if !referenced[object.Key] && object.LastModified.Before(cutoff) {
				keys = append(keys, object.Key)
			}
		}
		for len(keys) > 0 {
			n := min(len(keys), 1000)
			if err := s.objects.DeleteMany(ctx, keys[:n], "orphan garbage collection"); err != nil {
				return err
			}
			deleted += n
			keys = keys[n:]
		}
		return ctx.Err()
	})
	if err != nil && !errors.Is(err, context.Canceled) {
		s.log.Warn("GC streaming pass failed", "prefix", prefix, "error", err)
	}
	return deleted
}
