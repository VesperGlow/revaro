package server

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/VesperGlow/revaro/internal/reader"
	"github.com/VesperGlow/revaro/internal/reader/flow"
	"github.com/VesperGlow/revaro/internal/storage"
	"github.com/go-chi/chi/v5"
)

// 内置阅读器：EPUB/TXT 在服务端解析（按内容哈希缓存），前端负责分栏分页。

func isReadableFile(f File) bool {
	switch strings.ToLower(filepath.Ext(f.Name)) {
	case ".epub", ".txt":
		return true
	}
	return false
}

func (s *Server) readerFile(w http.ResponseWriter, r *http.Request) (File, bool) {
	f, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || f.Kind != "file" || f.Status != "ready" {
		problem(w, http.StatusNotFound, "ready file not found")
		return File{}, false
	}
	if !isReadableFile(f) {
		problem(w, http.StatusUnsupportedMediaType, "只支持阅读 EPUB 和 TXT 文件")
		return File{}, false
	}
	if f.Size > reader.MaxEPUB {
		problem(w, http.StatusRequestEntityTooLarge, "文件太大，请下载后离线阅读")
		return File{}, false
	}
	return f, true
}

// loadBook 返回解析后的 Book：内存 LRU（reader/books class）命中直接
// 返回；否则经 L2 缓存的书源 blob 解析（冷启动免回源 S3 下载）。
func (s *Server) loadBook(ctx context.Context, f File) (*reader.Book, error) {
	if b := s.books.Get(f.objectKey); b != nil {
		return b, nil
	}
	rc, err := s.openBookSource(ctx, f)
	if err != nil {
		return nil, err
	}
	defer rc.Close()
	book, err := reader.Parse(f.Name, rc, f.Size, fmt.Sprintf("/api/files/%s/book/assets", f.ID), f.ETag)
	if err != nil {
		return nil, err
	}
	s.books.Put(f.objectKey, book)
	return book, nil
}

func (s *Server) bookInfo(w http.ResponseWriter, r *http.Request) {
	f, ok := s.readerFile(w, r)
	if !ok {
		return
	}
	book, err := s.loadBook(r.Context(), f)
	if err != nil {
		problem(w, http.StatusUnprocessableEntity, "无法解析这本书："+err.Error())
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{
		"format": book.Format,
		"title":  book.Title,
		"name":   f.Name,
		"cover":  len(book.Cover) > 0,
		"toc":    book.TOC,
	})
}

func (s *Server) bookAsset(w http.ResponseWriter, r *http.Request) {
	f, ok := s.readerFile(w, r)
	if !ok {
		return
	}
	book, err := s.loadBook(r.Context(), f)
	if err != nil {
		problem(w, http.StatusUnprocessableEntity, "无法解析这本书："+err.Error())
		return
	}
	idx, err := strconv.Atoi(chi.URLParam(r, "index"))
	if err != nil || idx < 0 || idx >= len(book.Assets) {
		problem(w, http.StatusNotFound, "asset not found")
		return
	}
	asset := book.Assets[idx]
	w.Header().Set("Content-Type", safeDeliveryMime(asset.ContentType))
	w.Header().Set("Content-Length", strconv.Itoa(len(asset.Data)))
	// 资产内容由清单键（内容哈希）派生，天然不可变，可长期缓存
	w.Header().Set("Cache-Control", "private, max-age=31536000, immutable")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(asset.Data)
}

func (s *Server) bookCover(w http.ResponseWriter, r *http.Request) {
	f, ok := s.readerFile(w, r)
	if !ok {
		return
	}
	book, err := s.loadBook(r.Context(), f)
	if err != nil {
		problem(w, http.StatusUnprocessableEntity, "无法解析这本书："+err.Error())
		return
	}
	if len(book.Cover) == 0 {
		problem(w, http.StatusNotFound, "这本书没有内嵌封面")
		return
	}
	coverType := safeDeliveryMime(reader.AssetContentType(book.CoverExt))
	w.Header().Set("Content-Type", coverType)
	if coverType == "application/octet-stream" {
		w.Header().Set("Content-Disposition", "attachment")
	}
	w.Header().Set("Content-Length", strconv.Itoa(len(book.Cover)))
	// 封面 URL 不带版本参数，不能 immutable；短缓存即可
	w.Header().Set("Cache-Control", "private, max-age=3600")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(book.Cover)
}

// bookProgress 是阅读进度：位置用 readingAnchor（在连续 reading flow 上
// 跨字号/横竖屏/客户端分页稳定）。页码只是客户端临时显示值，不持久化。
type bookProgress struct {
	Anchor *flow.Anchor `json:"anchor,omitempty"`
}

func progressKey(fileID string) string { return "book_progress/" + fileID }

func (s *Server) bookProgress(w http.ResponseWriter, r *http.Request) {
	f, ok := s.readerFile(w, r)
	if !ok {
		return
	}
	var raw string
	err := s.db.QueryRowContext(r.Context(), `SELECT value FROM settings WHERE key=?`, progressKey(f.ID)).Scan(&raw)
	if errors.Is(err, sql.ErrNoRows) {
		writeJSON(w, http.StatusOK, bookProgress{})
		return
	}
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not read progress")
		return
	}
	var p bookProgress
	if err := json.Unmarshal([]byte(raw), &p); err != nil {
		writeJSON(w, http.StatusOK, bookProgress{})
		return
	}
	if p.Anchor != nil && !p.Anchor.Valid() {
		p.Anchor = nil // 防御：损坏/过期的锚点不返回
	}
	writeJSON(w, http.StatusOK, p)
}

func (s *Server) saveBookProgress(w http.ResponseWriter, r *http.Request) {
	f, ok := s.readerFile(w, r)
	if !ok {
		return
	}
	var in bookProgress
	if decodeJSON(w, r, &in) != nil {
		return
	}
	if in.Anchor != nil && !in.Anchor.Valid() {
		problem(w, http.StatusBadRequest, "progress anchor is invalid")
		return
	}
	raw, err := json.Marshal(in)
	if err != nil {
		problem(w, http.StatusBadRequest, "progress values are invalid")
		return
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	if _, err = s.db.ExecContext(r.Context(), `INSERT INTO settings(key,value,updated_at) VALUES(?,?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at`, progressKey(f.ID), string(raw), now); err != nil {
		problem(w, http.StatusInternalServerError, "could not save progress")
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

// openBookSource 打开书源 blob。小体积书源走 reader/source L2（内容寻址
// immutable）：重复打开与重启后不再回源 S3；大体积直连对象存储流式读取。
func (s *Server) openBookSource(ctx context.Context, f File) (io.ReadSeekCloser, error) {
	if f.Size <= maxCachedBookSource {
		if data, err := s.cache.Load(ctx, cacheClassReaderSource, f.objectKey, 0, func(ctx context.Context) ([]byte, error) {
			return s.objects.Get(ctx, f.objectKey, maxCachedBookSource)
		}); err == nil {
			return struct {
				*io.SectionReader
				io.Closer
			}{io.NewSectionReader(bytes.NewReader(data), 0, int64(len(data))), io.NopCloser(nil)}, nil
		}
		// L2 读失败（磁盘满等）降级直连，不阻断阅读
	}
	return s.objects.OpenSeek(storage.WithDynamicReadAhead(ctx), f.objectKey)
}
