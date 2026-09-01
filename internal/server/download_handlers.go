package server

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"io"
	"mime"
	"net/http"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/go-chi/chi/v5"
)

func (s *Server) createDownload(w http.ResponseWriter, r *http.Request) {
	if s.downloads == nil {
		problem(w, http.StatusServiceUnavailable, "内置离线下载不可用")
		return
	}
	var in struct {
		ParentID      string `json:"parent_id"`
		Magnet        string `json:"magnet"`
		TorrentBase64 string `json:"torrent_base64"`
		URL           string `json:"url"`
	}
	if decodeJSONLimit(w, r, &in, maxJSONBody) != nil {
		return
	}
	var job downloadJob
	var err error
	if strings.TrimSpace(in.URL) != "" {
		if strings.TrimSpace(in.Magnet) != "" || strings.TrimSpace(in.TorrentBase64) != "" {
			problem(w, http.StatusBadRequest, "每次只能提交一种下载来源")
			return
		}
		job, err = s.downloads.createURL(r.Context(), in.ParentID, in.URL)
	} else {
		job, err = s.downloads.create(r.Context(), in.ParentID, in.Magnet, in.TorrentBase64)
	}
	if err != nil {
		problem(w, http.StatusBadRequest, err.Error())
		return
	}
	writeJSON(w, http.StatusAccepted, job)
}
func (s *Server) getDownload(w http.ResponseWriter, r *http.Request) {
	if s.downloads == nil {
		problem(w, 503, "内置离线下载不可用")
		return
	}
	job, err := s.downloads.getAny(r.Context(), chi.URLParam(r, "id"), true)
	if errors.Is(err, sql.ErrNoRows) {
		problem(w, 404, "离线下载任务不存在")
		return
	}
	if err != nil {
		problem(w, 500, "无法读取离线下载任务")
		return
	}
	writeJSON(w, 200, job)
}
func (s *Server) startDownload(w http.ResponseWriter, r *http.Request) {
	if s.downloads == nil {
		problem(w, 503, "内置离线下载不可用")
		return
	}
	var in struct {
		FileIndices []int `json:"file_indices"`
	}
	if decodeJSON(w, r, &in) != nil {
		return
	}
	job, err := s.downloads.getAny(r.Context(), chi.URLParam(r, "id"), false)
	if err != nil || job.SourceType == "url" {
		problem(w, 400, "直链下载不需要选择文件")
		return
	}
	if err := s.downloads.start(r.Context(), chi.URLParam(r, "id"), in.FileIndices); err != nil {
		problem(w, 400, err.Error())
		return
	}
	job, _ = s.downloads.get(r.Context(), chi.URLParam(r, "id"), true)
	writeJSON(w, 200, job)
}
func (s *Server) pauseDownload(w http.ResponseWriter, r *http.Request) {
	if s.downloads == nil {
		problem(w, 503, "内置离线下载不可用")
		return
	}
	if err := s.downloads.pauseAny(r.Context(), chi.URLParam(r, "id")); err != nil {
		problem(w, 409, err.Error())
		return
	}
	w.WriteHeader(204)
}
func (s *Server) resumeDownload(w http.ResponseWriter, r *http.Request) {
	if s.downloads == nil {
		problem(w, 503, "内置离线下载不可用")
		return
	}
	jobID := chi.URLParam(r, "id")
	// A failed BT task may be retried into another directory. An empty body keeps
	// the long-standing pause/resume API behavior unchanged.
	if r.ContentLength != 0 {
		var in struct {
			ParentID string `json:"parent_id"`
		}
		if decodeJSONLimit(w, r, &in, maxJSONBody) != nil {
			return
		}
		if in.ParentID != "" {
			var kind, status, sourceType string
			if err := s.db.QueryRowContext(r.Context(), `SELECT kind FROM files WHERE id=? AND deleted_at IS NULL`, in.ParentID).Scan(&kind); err != nil || kind != "directory" {
				problem(w, http.StatusBadRequest, "保存目录不存在")
				return
			}
			if err := s.db.QueryRowContext(r.Context(), `SELECT status,source_type FROM download_jobs WHERE id=?`, jobID).Scan(&status, &sourceType); err != nil || status != "failed" || sourceType == "url" {
				problem(w, http.StatusConflict, "任务当前不能更改保存目录")
				return
			}
			if _, err := s.db.ExecContext(r.Context(), `UPDATE download_jobs SET parent_id=? WHERE id=?`, in.ParentID, jobID); err != nil {
				problem(w, http.StatusInternalServerError, "无法更新保存目录")
				return
			}
		}
	}
	if err := s.downloads.resumeAny(r.Context(), jobID); err != nil {
		problem(w, 409, err.Error())
		return
	}
	w.WriteHeader(204)
}

// parseByteRange parses a single HTTP byte range (`bytes=start-end`,
// `bytes=start-` or `bytes=-suffix`) against the known size. Media players
// only ever request a single range, so a multi-range header is ignored.
func parseByteRange(header string, size int64) (start, end int64, ok bool) {
	if size <= 0 {
		return 0, 0, false
	}
	if header == "" {
		return 0, size - 1, true
	}
	const prefix = "bytes="
	if !strings.HasPrefix(header, prefix) {
		return 0, 0, false
	}
	spec := strings.TrimPrefix(header, prefix)
	if comma := strings.IndexByte(spec, ','); comma >= 0 {
		return 0, 0, false
	}
	before, after, found := strings.Cut(spec, "-")
	if !found {
		return 0, 0, false
	}
	before, after = strings.TrimSpace(before), strings.TrimSpace(after)
	if before == "" {
		suffix, err := strconv.ParseInt(after, 10, 64)
		if err != nil || suffix <= 0 {
			return 0, 0, false
		}
		if suffix > size {
			suffix = size
		}
		return size - suffix, size - 1, true
	}
	start, err := strconv.ParseInt(before, 10, 64)
	if err != nil || start < 0 || start >= size {
		return 0, 0, false
	}
	end = size - 1
	if after != "" {
		parsed, parseErr := strconv.ParseInt(after, 10, 64)
		if parseErr != nil {
			return 0, 0, false
		}
		end = parsed
	}
	if end >= size {
		end = size - 1
	}
	if end < start {
		return 0, 0, false
	}
	return start, end, true
}

var (
	errDownloadRange = errors.New("invalid download byte range")
	errDownloadFile  = errors.New("download file unavailable")
	errDownloadState = errors.New("download is not streamable")
)

// openFileStream returns a stream over one selected torrent file. It forwards
// the requested byte range to the Rust torrent engine, whose librqbit stream
// prioritizes the pieces covering the seek offset while the file is still
// downloading.
func (m *downloadManager) openFileStream(ctx context.Context, jobID string, fileIndex int, rangeHeader string) (io.ReadCloser, int64, int64, int64, string, error) {
	m.mu.RLock()
	runtime := m.jobs[jobID]
	m.mu.RUnlock()
	if runtime == nil {
		return nil, 0, 0, 0, "", fmt.Errorf("%w: 下载任务未运行", errDownloadState)
	}
	job, err := m.get(ctx, jobID, true)
	if err != nil {
		return nil, 0, 0, 0, "", err
	}
	if job.Status != "downloading" && job.Status != "paused" && job.Status != "importing" {
		return nil, 0, 0, 0, "", fmt.Errorf("%w: 任务当前不能边下边播", errDownloadState)
	}
	var file downloadFile
	found := false
	for _, item := range job.Files {
		if item.Index == fileIndex && item.Selected {
			file, found = item, true
			break
		}
	}
	if !found {
		return nil, 0, 0, 0, "", fmt.Errorf("%w: 下载文件不存在或未选择", errDownloadFile)
	}
	if file.Size == 0 && rangeHeader == "" {
		return io.NopCloser(strings.NewReader("")), 0, -1, 0, "application/octet-stream", nil
	}
	start, end, ok := parseByteRange(rangeHeader, file.Size)
	if !ok {
		return nil, 0, 0, file.Size, "", fmt.Errorf("%w: 无效的字节范围", errDownloadRange)
	}
	body, err := m.bt.StreamTorrent(ctx, runtime.torrentID, fileIndex, start, end)
	if err != nil {
		return nil, 0, 0, 0, "", err
	}
	mimeType := mime.TypeByExtension(strings.ToLower(filepath.Ext(file.Path)))
	if mimeType == "" {
		mimeType = "application/octet-stream"
	}
	return body, start, end, file.Size, mimeType, nil
}

func (s *Server) streamDownloadFile(w http.ResponseWriter, r *http.Request) {
	if s.downloads == nil {
		problem(w, http.StatusServiceUnavailable, "内置离线下载不可用")
		return
	}
	index, err := strconv.Atoi(chi.URLParam(r, "index"))
	if err != nil || index < 0 {
		problem(w, http.StatusNotFound, "下载文件不存在")
		return
	}
	rangeHeader := r.Header.Get("Range")
	body, start, end, size, mimeType, err := s.downloads.openFileStream(r.Context(), chi.URLParam(r, "id"), index, rangeHeader)
	if err != nil {
		status := http.StatusInternalServerError
		switch {
		case errors.Is(err, errDownloadRange):
			status = http.StatusRequestedRangeNotSatisfiable
			w.Header().Set("Content-Range", fmt.Sprintf("bytes */%d", size))
		case errors.Is(err, errDownloadFile), errors.Is(err, sql.ErrNoRows):
			status = http.StatusNotFound
		case errors.Is(err, errDownloadState):
			status = http.StatusConflict
		}
		problem(w, status, err.Error())
		return
	}
	defer body.Close()
	w.Header().Set("Content-Type", mimeType)
	w.Header().Set("Accept-Ranges", "bytes")
	w.Header().Set("Content-Length", strconv.FormatInt(max(0, end-start+1), 10))
	if rangeHeader != "" {
		w.Header().Set("Content-Range", fmt.Sprintf("bytes %d-%d/%d", start, end, size))
		w.WriteHeader(http.StatusPartialContent)
	} else {
		w.WriteHeader(http.StatusOK)
	}
	_, _ = io.Copy(w, body)
}
