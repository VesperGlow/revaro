package server

import (
	"bytes"
	"context"
	"database/sql"
	"errors"
	"net/http"
	"path/filepath"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/VesperGlow/revaro/internal/ids"
	"github.com/VesperGlow/revaro/internal/storage"
	"github.com/go-chi/chi/v5"
)

func scanFile(row interface{ Scan(...any) error }) (File, error) {
	var f File
	var parent, mime, etag, contentHash, hashAlgorithm, deleted, restoreParent sql.NullString
	err := row.Scan(&f.ID, &parent, &f.Name, &f.Kind, &f.objectKey, &f.Size, &mime, &etag, &contentHash, &hashAlgorithm, &f.Status, &f.CreatedAt, &f.UpdatedAt, &deleted, &restoreParent)
	if parent.Valid {
		f.ParentID = &parent.String
	}
	f.MimeType = mime.String
	f.ETag = etag.String
	f.ContentHash = contentHash.String
	f.HashAlgorithm = hashAlgorithm.String
	f.DeletedAt = deleted.String
	if restoreParent.Valid {
		f.RestoreParentID = &restoreParent.String
	}
	return f, err
}

const fileColumns = `id,parent_id,name,kind,COALESCE(object_key,''),size,mime_type,etag,content_hash,hash_algorithm,status,created_at,updated_at,deleted_at,restore_parent_id`

func (s *Server) file(ctx context.Context, id string) (File, error) {
	return scanFile(s.db.QueryRowContext(ctx, `SELECT `+fileColumns+` FROM files WHERE id=? AND deleted_at IS NULL`, id))
}

// readableFile also resolves soft-deleted files. It is only used by
// authenticated content delivery and derived-thumbnail endpoints so items
// can still be viewed before they are restored or permanently deleted.
func (s *Server) readableFile(ctx context.Context, id string) (File, error) {
	return scanFile(s.db.QueryRowContext(ctx, `SELECT `+fileColumns+` FROM files WHERE id=?`, id))
}
func (s *Server) getFile(w http.ResponseWriter, r *http.Request) {
	f, err := s.file(r.Context(), chi.URLParam(r, "id"))
	if errors.Is(err, sql.ErrNoRows) {
		problem(w, 404, "file not found")
		return
	}
	if err != nil {
		problem(w, 500, "database error")
		return
	}
	crumbs, err := s.breadcrumbs(r.Context(), f.ID)
	if err != nil {
		problem(w, 500, "database error")
		return
	}
	writeJSON(w, 200, map[string]any{"file": f, "breadcrumbs": crumbs})
}
func (s *Server) breadcrumbs(ctx context.Context, id string) ([]File, error) {
	const qualified = `f.id,f.parent_id,f.name,f.kind,COALESCE(f.object_key,''),f.size,f.mime_type,f.etag,f.content_hash,f.hash_algorithm,f.status,f.created_at,f.updated_at,f.deleted_at,f.restore_parent_id`
	rows, err := s.db.QueryContext(ctx, `WITH RECURSIVE p(id,parent_id,name,kind,object_key,size,mime_type,etag,content_hash,hash_algorithm,status,created_at,updated_at,deleted_at,restore_parent_id,depth) AS (SELECT `+fileColumns+`,0 FROM files WHERE id=? AND deleted_at IS NULL UNION ALL SELECT `+qualified+`,p.depth+1 FROM files f JOIN p ON f.id=p.parent_id WHERE f.deleted_at IS NULL) SELECT `+fileColumns+` FROM p ORDER BY depth DESC`, id)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []File
	for rows.Next() {
		f, e := scanFile(rows)
		if e != nil {
			return nil, e
		}
		out = append(out, f)
	}
	return out, rows.Err()
}
func (s *Server) children(w http.ResponseWriter, r *http.Request) {
	parent, err := s.file(r.Context(), chi.URLParam(r, "id"))
	if err != nil || parent.Kind != "directory" {
		problem(w, 404, "directory not found")
		return
	}
	rows, err := s.db.QueryContext(r.Context(), `SELECT `+fileColumns+` FROM files WHERE parent_id=? AND deleted_at IS NULL ORDER BY kind DESC, name COLLATE NOCASE`, parent.ID)
	if err != nil {
		problem(w, 500, "database error")
		return
	}
	defer rows.Close()
	out := []File{}
	for rows.Next() {
		f, e := scanFile(rows)
		if e != nil {
			problem(w, 500, "database error")
			return
		}
		out = append(out, f)
	}
	if err := rows.Err(); err != nil {
		problem(w, 500, "database error")
		return
	}
	if err := rows.Close(); err != nil {
		problem(w, 500, "database error")
		return
	}
	coverRows, err := s.db.QueryContext(r.Context(), `SELECT am.file_id FROM audio_media am JOIN files f ON f.id=am.file_id WHERE f.parent_id=? AND f.deleted_at IS NULL AND am.has_cover=1`, parent.ID)
	if err != nil {
		problem(w, 500, "could not load audio covers")
		return
	}
	covered := make(map[string]bool)
	for coverRows.Next() {
		var fileID string
		if err := coverRows.Scan(&fileID); err != nil {
			coverRows.Close()
			problem(w, 500, "could not load audio covers")
			return
		}
		covered[fileID] = true
	}
	if err := coverRows.Err(); err != nil {
		coverRows.Close()
		problem(w, 500, "could not load audio covers")
		return
	}
	if err := coverRows.Close(); err != nil {
		problem(w, 500, "could not load audio covers")
		return
	}
	for index := range out {
		out[index].HasCover = covered[out[index].ID]
	}
	for _, file := range out {
		s.scheduleVideoThumbnail(file)
	}
	var totalBytes, fileCount int64
	if err := s.db.QueryRowContext(r.Context(), `SELECT total_bytes,file_count FROM directory_stats WHERE directory_id=?`, parent.ID).Scan(&totalBytes, &fileCount); err != nil {
		problem(w, 500, "could not calculate directory usage")
		return
	}
	writeJSON(w, 200, map[string]any{"items": out, "total_bytes": totalBytes, "file_count": fileCount})
}

func (s *Server) storageStats(w http.ResponseWriter, r *http.Request) {
	var totalBytes, fileCount int64
	if err := s.db.QueryRowContext(r.Context(), `SELECT COALESCE(SUM(size),0),COUNT(*) FROM files WHERE kind='file' AND status='ready' AND deleted_at IS NULL`).Scan(&totalBytes, &fileCount); err != nil {
		problem(w, http.StatusInternalServerError, "could not calculate storage usage")
		return
	}
	writeJSON(w, http.StatusOK, map[string]int64{"total_bytes": totalBytes, "file_count": fileCount})
}

func (s *Server) createDirectory(w http.ResponseWriter, r *http.Request) {
	var in struct {
		ParentID string `json:"parent_id"`
		Name     string `json:"name"`
	}
	if decodeJSON(w, r, &in) != nil {
		return
	}
	if err := validateName(in.Name); err != nil {
		problem(w, 400, err.Error())
		return
	}
	parent, err := s.file(r.Context(), in.ParentID)
	if err != nil || parent.Kind != "directory" || parent.Status != "ready" {
		problem(w, 400, "parent directory is invalid")
		return
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	f := File{ID: ids.New(), ParentID: &in.ParentID, Name: in.Name, Kind: "directory", Status: "ready", CreatedAt: now, UpdatedAt: now}
	_, err = s.db.ExecContext(r.Context(), `INSERT INTO files(id,parent_id,name,kind,status,created_at,updated_at) VALUES(?,?,?,?,?,?,?)`, f.ID, in.ParentID, f.Name, f.Kind, f.Status, now, now)
	if isConflict(err) {
		problem(w, 409, "an item with that name already exists")
		return
	}
	if err != nil {
		problem(w, 500, "could not create directory")
		return
	}
	writeJSON(w, 201, f)
}

type documentInput struct {
	ParentID string `json:"parent_id"`
	Name     string `json:"name"`
	Content  string `json:"content"`
	ETag     string `json:"etag"`
}

func editableDocumentName(name string) bool {
	switch strings.ToLower(filepath.Ext(name)) {
	case ".md", ".markdown", ".txt", ".yaml", ".yml", ".json", ".toml", ".ini", ".conf", ".log", ".csv":
		return true
	default:
		return false
	}
}

func documentMime(name string) string {
	switch strings.ToLower(filepath.Ext(name)) {
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
	default:
		return "text/plain; charset=utf-8"
	}
}

func validateDocument(name, content string) error {
	if err := validateName(name); err != nil {
		return err
	}
	if !editableDocumentName(name) {
		return errors.New("this file type cannot be edited as text")
	}
	if !utf8.ValidString(content) {
		return errors.New("document must contain valid UTF-8 text")
	}
	if len([]byte(content)) > maxDocumentBytes {
		return errors.New("editable documents cannot exceed 1 MiB")
	}
	return nil
}

func (s *Server) createDocument(w http.ResponseWriter, r *http.Request) {
	var in documentInput
	if decodeJSON(w, r, &in) != nil {
		return
	}
	if err := validateDocument(in.Name, in.Content); err != nil {
		problem(w, http.StatusBadRequest, err.Error())
		return
	}
	parent, err := s.file(r.Context(), in.ParentID)
	if err != nil || parent.Kind != "directory" || parent.Status != "ready" {
		problem(w, http.StatusBadRequest, "parent directory is invalid")
		return
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	content := []byte(in.Content)
	key, stored, err := s.storeBlob(r.Context(), bytes.NewReader(content), int64(len(content)), documentMime(in.Name))
	if err != nil {
		problem(w, http.StatusBadGateway, "object storage write failed")
		return
	}
	f := File{ID: ids.New(), ParentID: &in.ParentID, Name: in.Name, Kind: "file", Size: stored.Size, MimeType: documentMime(in.Name), ETag: stored.ETag, Status: "ready", CreatedAt: now, UpdatedAt: now, objectKey: key}
	_, err = s.db.ExecContext(r.Context(), `INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)`, f.ID, in.ParentID, f.Name, f.Kind, f.objectKey, f.Size, f.MimeType, f.ETag, f.Status, now, now)
	if isConflict(err) {
		s.discardBlob(key)
		problem(w, http.StatusConflict, "an item with that name already exists")
		return
	}
	if err != nil {
		s.discardBlob(key)
		problem(w, http.StatusInternalServerError, "could not create document")
		return
	}
	writeJSON(w, http.StatusCreated, f)
}

func (s *Server) getDocument(w http.ResponseWriter, r *http.Request) {
	f, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || f.Kind != "file" || f.Status != "ready" {
		problem(w, http.StatusNotFound, "ready file not found")
		return
	}
	if !editableDocumentName(f.Name) {
		problem(w, http.StatusUnsupportedMediaType, "this file type cannot be edited as text")
		return
	}
	if f.Size > maxDocumentBytes {
		problem(w, http.StatusRequestEntityTooLarge, "editable documents cannot exceed 1 MiB")
		return
	}
	data, err := s.readContent(r.Context(), f)
	if errors.Is(err, storage.ErrObjectTooLarge) {
		problem(w, http.StatusRequestEntityTooLarge, "editable documents cannot exceed 1 MiB")
		return
	}
	if err != nil {
		problem(w, http.StatusBadGateway, "object storage read failed")
		return
	}
	if !utf8.Valid(data) {
		problem(w, http.StatusUnsupportedMediaType, "file is not valid UTF-8 text")
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"content": string(data), "etag": f.ETag, "updated_at": f.UpdatedAt})
}

func (s *Server) updateDocument(w http.ResponseWriter, r *http.Request) {
	f, err := s.file(r.Context(), chi.URLParam(r, "id"))
	if err != nil || f.Kind != "file" || f.Status != "ready" {
		problem(w, http.StatusNotFound, "ready file not found")
		return
	}
	var in documentInput
	if decodeJSON(w, r, &in) != nil {
		return
	}
	if err := validateDocument(f.Name, in.Content); err != nil {
		problem(w, http.StatusBadRequest, err.Error())
		return
	}
	if in.ETag != "" && f.ETag != "" && in.ETag != f.ETag {
		problem(w, http.StatusConflict, "document changed elsewhere; reopen it before saving")
		return
	}
	content := []byte(in.Content)
	key, stored, err := s.storeBlob(r.Context(), bytes.NewReader(content), int64(len(content)), documentMime(f.Name))
	if err != nil {
		problem(w, http.StatusBadGateway, "object storage write failed")
		return
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	// 原子乐观并发控制：etag 条件放进 UPDATE 的 WHERE 子句。两个并发
	// 编辑者同时保存时，只有先提交者成功；后提交者命中 0 行并收到 409，
	// 而不是在检查与写入之间被静默覆盖（TOCTOU）。
	res, err := s.db.ExecContext(r.Context(), `UPDATE files SET object_key=?,size=?,mime_type=?,etag=?,updated_at=? WHERE id=? AND (etag=? OR ?='' OR etag='')`, key, stored.Size, documentMime(f.Name), stored.ETag, now, f.ID, in.ETag, in.ETag)
	if err != nil {
		s.discardBlob(key)
		problem(w, http.StatusInternalServerError, "document content changed but metadata update failed")
		return
	}
	if n, _ := res.RowsAffected(); n == 0 {
		s.discardBlob(key)
		problem(w, http.StatusConflict, "document changed elsewhere; reopen it before saving")
		return
	}
	updated, _ := s.file(r.Context(), f.ID)
	writeJSON(w, http.StatusOK, updated)
}

func (s *Server) patchFile(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if id == RootID {
		problem(w, 400, "root cannot be modified")
		return
	}
	var in struct {
		Name     *string `json:"name"`
		ParentID *string `json:"parent_id"`
	}
	if decodeJSON(w, r, &in) != nil {
		return
	}
	if in.Name == nil && in.ParentID == nil {
		problem(w, 400, "name or parent_id is required")
		return
	}
	f, err := s.file(r.Context(), id)
	if err != nil {
		problem(w, 404, "file not found")
		return
	}
	name := f.Name
	if in.Name != nil {
		name = *in.Name
		if err := validateName(name); err != nil {
			problem(w, 400, err.Error())
			return
		}
	}
	parent := *f.ParentID
	if in.ParentID != nil {
		parent = *in.ParentID
		p, err := s.file(r.Context(), parent)
		if err != nil || p.Kind != "directory" || p.Status != "ready" {
			problem(w, 400, "target directory is invalid")
			return
		}
		if f.Kind == "directory" {
			var cyclic int
			err = s.db.QueryRowContext(r.Context(), `WITH RECURSIVE d(id) AS (SELECT id FROM files WHERE id=? UNION ALL SELECT f.id FROM files f JOIN d ON f.parent_id=d.id) SELECT EXISTS(SELECT 1 FROM d WHERE id=?)`, id, parent).Scan(&cyclic)
			if err != nil {
				problem(w, 500, "database error")
				return
			}
			if cyclic == 1 {
				problem(w, 400, "a directory cannot be moved into itself or its descendants")
				return
			}
		}
	}
	_, err = s.db.ExecContext(r.Context(), `UPDATE files SET name=?,parent_id=?,updated_at=? WHERE id=?`, name, parent, time.Now().UTC().Format(time.RFC3339Nano), id)
	if isConflict(err) {
		problem(w, 409, "an item with that name already exists")
		return
	}
	if err != nil {
		problem(w, 500, "could not update item")
		return
	}
	updated, _ := s.file(r.Context(), id)
	writeJSON(w, 200, updated)
}

func (s *Server) copyFile(w http.ResponseWriter, r *http.Request) {
	var in struct {
		ParentID string `json:"parent_id"`
	}
	if decodeJSON(w, r, &in) != nil {
		return
	}
	source, err := s.file(r.Context(), chi.URLParam(r, "id"))
	if err != nil || source.Kind != "file" || source.Status != "ready" {
		problem(w, http.StatusNotFound, "ready file not found")
		return
	}
	parent, err := s.file(r.Context(), in.ParentID)
	if err != nil || parent.Kind != "directory" || parent.Status != "ready" {
		problem(w, http.StatusBadRequest, "target directory is invalid")
		return
	}
	tx, err := s.db.BeginTx(r.Context(), nil)
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not start copy")
		return
	}
	defer tx.Rollback()
	name, err := availableCopyName(r.Context(), tx, in.ParentID, source.Name)
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not choose a copy name")
		return
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	copyID := ids.New()
	_, err = tx.ExecContext(r.Context(), `INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)`,
		copyID, in.ParentID, name, "file", source.objectKey, source.Size, source.MimeType, source.ETag, "ready", now, now)
	if isConflict(err) {
		problem(w, http.StatusConflict, "an item with that name already exists")
		return
	}
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not copy file")
		return
	}
	if _, err = tx.ExecContext(r.Context(), `INSERT INTO audio_media(file_id,duration_ms,chapters_json,subtitles_json,stream_object_key,stream_size,stream_etag,has_cover,created_at,updated_at) SELECT ?,duration_ms,chapters_json,subtitles_json,stream_object_key,stream_size,stream_etag,has_cover,?,? FROM audio_media WHERE file_id=?`, copyID, now, now, source.ID); err != nil {
		problem(w, http.StatusInternalServerError, "could not copy audio metadata")
		return
	}
	if err = tx.Commit(); err != nil {
		problem(w, http.StatusInternalServerError, "could not finish copy")
		return
	}
	copied, err := s.file(r.Context(), copyID)
	if err != nil {
		problem(w, http.StatusInternalServerError, "copied file could not be read")
		return
	}
	writeJSON(w, http.StatusCreated, copied)
}

func availableCopyName(ctx context.Context, tx *sql.Tx, parentID, original string) (string, error) {
	var exists int
	if err := tx.QueryRowContext(ctx, `SELECT EXISTS(SELECT 1 FROM files WHERE parent_id=? AND name=? AND deleted_at IS NULL)`, parentID, original).Scan(&exists); err != nil {
		return "", err
	}
	if exists == 0 {
		return original, nil
	}
	ext := filepath.Ext(original)
	stem := strings.TrimSuffix(original, ext)
	for index := 1; index <= 9999; index++ {
		suffix := " - 副本"
		if index > 1 {
			suffix += " " + strconv.Itoa(index)
		}
		candidate := stem + suffix + ext
		if validateName(candidate) != nil {
			return "", errors.New("copy name is too long")
		}
		if err := tx.QueryRowContext(ctx, `SELECT EXISTS(SELECT 1 FROM files WHERE parent_id=? AND name=? AND deleted_at IS NULL)`, parentID, candidate).Scan(&exists); err != nil {
			return "", err
		}
		if exists == 0 {
			return candidate, nil
		}
	}
	return "", errors.New("too many copies with the same name")
}

func (s *Server) deleteFile(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if id == RootID {
		problem(w, 400, "root cannot be deleted")
		return
	}
	f, err := s.file(r.Context(), id)
	if err != nil {
		problem(w, 404, "file not found")
		return
	}
	// 未完成的文件没有可恢复内容，仍直接清掉上传记录；ready 项（包括
	// 非空目录）整棵移入回收站，内容块继续被 GC 视为存活引用。
	if f.Kind == "file" && f.Status != "ready" {
		if _, err = s.db.ExecContext(r.Context(), `DELETE FROM files WHERE id=?`, id); err != nil {
			problem(w, 500, "could not delete file")
			return
		}
		w.WriteHeader(http.StatusNoContent)
		return
	}
	tx, err := s.db.BeginTx(r.Context(), nil)
	if err != nil {
		problem(w, 500, "could not open trash")
		return
	}
	defer tx.Rollback()
	now := time.Now().UTC().Format(time.RFC3339Nano)
	if _, err = tx.ExecContext(r.Context(), `WITH RECURSIVE tree(id) AS (SELECT id FROM files WHERE id=? AND deleted_at IS NULL UNION ALL SELECT f.id FROM files f JOIN tree t ON f.parent_id=t.id WHERE f.deleted_at IS NULL) UPDATE files SET deleted_at=?,trash_root_id=? WHERE id IN tree`, id, now, id); err == nil {
		_, err = tx.ExecContext(r.Context(), `UPDATE files SET restore_parent_id=parent_id,parent_id=NULL WHERE id=?`, id)
	}
	if err != nil || tx.Commit() != nil {
		problem(w, 500, "could not move item to trash")
		return
	}
	w.WriteHeader(204)
}

func (s *Server) trash(w http.ResponseWriter, r *http.Request) {
	rows, err := s.db.QueryContext(r.Context(), `SELECT `+fileColumns+` FROM files WHERE deleted_at IS NOT NULL AND trash_root_id=id ORDER BY deleted_at DESC`)
	if err != nil {
		problem(w, 500, "could not read trash")
		return
	}
	defer rows.Close()
	items := []File{}
	for rows.Next() {
		f, scanErr := scanFile(rows)
		if scanErr != nil {
			problem(w, 500, "could not read trash")
			return
		}
		items = append(items, f)
	}
	if err := rows.Err(); err != nil {
		problem(w, 500, "could not read trash")
		return
	}
	if err := rows.Close(); err != nil {
		problem(w, 500, "could not read trash")
		return
	}
	var totalBytes, fileCount int64
	if err := s.db.QueryRowContext(r.Context(), `SELECT COALESCE(SUM(size),0),COUNT(*) FROM files WHERE kind='file' AND status='ready' AND deleted_at IS NOT NULL`).Scan(&totalBytes, &fileCount); err != nil {
		problem(w, 500, "could not calculate trash usage")
		return
	}
	writeJSON(w, 200, map[string]any{"items": items, "total_bytes": totalBytes, "file_count": fileCount})
}

func (s *Server) restoreTrash(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	f, err := scanFile(s.db.QueryRowContext(r.Context(), `SELECT `+fileColumns+` FROM files WHERE id=? AND deleted_at IS NOT NULL AND trash_root_id=id`, id))
	if errors.Is(err, sql.ErrNoRows) {
		problem(w, 404, "trash item not found")
		return
	}
	if err != nil {
		problem(w, 500, "could not read trash item")
		return
	}
	parentID := RootID
	if f.RestoreParentID != nil {
		var valid int
		if err := s.db.QueryRowContext(r.Context(), `SELECT EXISTS(SELECT 1 FROM files WHERE id=? AND kind='directory' AND status='ready' AND deleted_at IS NULL)`, *f.RestoreParentID).Scan(&valid); err != nil {
			problem(w, 500, "could not validate restore location")
			return
		}
		if valid == 1 {
			parentID = *f.RestoreParentID
		}
	}
	tx, err := s.db.BeginTx(r.Context(), nil)
	if err != nil {
		problem(w, 500, "could not restore item")
		return
	}
	defer tx.Rollback()
	if _, err = tx.ExecContext(r.Context(), `UPDATE files SET parent_id=? WHERE id=?`, parentID, id); err == nil {
		_, err = tx.ExecContext(r.Context(), `UPDATE files SET deleted_at=NULL,restore_parent_id=NULL,trash_root_id=NULL WHERE trash_root_id=?`, id)
	}
	if isConflict(err) {
		problem(w, 409, "an item with that name already exists at the restore location")
		return
	}
	if err != nil || tx.Commit() != nil {
		problem(w, 500, "could not restore item")
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) purgeTrash(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	var exists int
	if err := s.db.QueryRowContext(r.Context(), `SELECT EXISTS(SELECT 1 FROM files WHERE id=? AND deleted_at IS NOT NULL AND trash_root_id=id)`, id).Scan(&exists); err != nil {
		problem(w, 500, "could not read trash item")
		return
	}
	if exists == 0 {
		problem(w, 404, "trash item not found")
		return
	}
	tx, err := s.db.BeginTx(r.Context(), nil)
	if err != nil {
		problem(w, 500, "could not permanently delete item")
		return
	}
	defer tx.Rollback()
	_, err = tx.ExecContext(r.Context(), `PRAGMA defer_foreign_keys=ON`)
	if err == nil {
		_, err = tx.ExecContext(r.Context(), `WITH RECURSIVE tree(id) AS (SELECT id FROM files WHERE id=? UNION ALL SELECT f.id FROM files f JOIN tree t ON f.parent_id=t.id) DELETE FROM files WHERE id IN tree`, id)
	}
	if err != nil || tx.Commit() != nil {
		problem(w, 500, "could not permanently delete item")
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) emptyTrash(w http.ResponseWriter, r *http.Request) {
	tx, err := s.db.BeginTx(r.Context(), nil)
	if err != nil {
		problem(w, 500, "could not empty trash")
		return
	}
	defer tx.Rollback()
	_, err = tx.ExecContext(r.Context(), `PRAGMA defer_foreign_keys=ON`)
	if err == nil {
		_, err = tx.ExecContext(r.Context(), `DELETE FROM files WHERE deleted_at IS NOT NULL`)
	}
	if err != nil || tx.Commit() != nil {
		problem(w, 500, "could not empty trash")
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

// CleanupExpiredTrash permanently removes complete trash trees whose root has
// exceeded the configured retention period. It only removes metadata; the
// caller should request a garbage-collection pass when the return value is
// non-zero so the now-unreferenced S3 objects are reclaimed as well.
func (s *Server) CleanupExpiredTrash(ctx context.Context) int64 {
	if s.cfg.TrashRetention == 0 {
		return 0
	}
	cutoff := time.Now().UTC().Add(-s.cfg.TrashRetention).Format(time.RFC3339Nano)
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		s.log.Error("open expired trash cleanup failed", "error", err)
		return 0
	}
	defer tx.Rollback()
	if _, err = tx.ExecContext(ctx, `PRAGMA defer_foreign_keys=ON`); err != nil {
		s.log.Error("configure expired trash cleanup failed", "error", err)
		return 0
	}
	var roots int64
	if err = tx.QueryRowContext(ctx, `SELECT COUNT(*) FROM files WHERE deleted_at IS NOT NULL AND trash_root_id=id AND deleted_at<=?`, cutoff).Scan(&roots); err != nil {
		s.log.Error("scan expired trash failed", "error", err)
		return 0
	}
	if roots == 0 {
		return 0
	}
	result, err := tx.ExecContext(ctx, `DELETE FROM files WHERE trash_root_id IN (SELECT id FROM files WHERE deleted_at IS NOT NULL AND trash_root_id=id AND deleted_at<=?)`, cutoff)
	if err != nil {
		s.log.Error("expired trash cleanup failed", "error", err)
		return 0
	}
	items, _ := result.RowsAffected()
	if err = tx.Commit(); err != nil {
		s.log.Error("commit expired trash cleanup failed", "error", err)
		return 0
	}
	s.log.Info("expired trash permanently deleted", "roots", roots, "items", items, "retention", s.cfg.TrashRetention)
	return roots
}

func (s *Server) download(w http.ResponseWriter, r *http.Request) { s.streamFile(w, r, false) }
func (s *Server) preview(w http.ResponseWriter, r *http.Request)  { s.streamFile(w, r, true) }
func (s *Server) streamFile(w http.ResponseWriter, r *http.Request, inline bool) {
	f, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || f.Kind != "file" || f.Status != "ready" {
		problem(w, 404, "ready file not found")
		return
	}
	if inline && !isPreviewable(f) {
		problem(w, 415, "preview is not available for this file type")
		return
	}
	s.serveFileContent(w, r, f, inline)
}
