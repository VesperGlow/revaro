package server

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"io"
	"mime"
	"net/http"
	"net/url"
	"path"
	"sort"
	"strings"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
)

const maxURLLength = 16 << 10

var errURLDownloadTooLarge = errors.New("直链文件超过离线下载大小限制")

type urlDownloadRuntime struct {
	ctx    context.Context
	cancel context.CancelFunc
}

type urlProgressReader struct {
	reader     io.Reader
	limit      int64
	read       int64
	onProgress func(int64)
}

func (r *urlProgressReader) Read(p []byte) (int, error) {
	if len(p) == 0 {
		return 0, nil
	}
	if r.read >= r.limit {
		var probe [1]byte
		n, err := r.reader.Read(probe[:])
		if n > 0 {
			return 0, errURLDownloadTooLarge
		}
		return 0, err
	}
	if int64(len(p)) > r.limit-r.read {
		p = p[:r.limit-r.read]
	}
	n, err := r.reader.Read(p)
	if n > 0 {
		r.read += int64(n)
		if r.onProgress != nil {
			r.onProgress(r.read)
		}
	}
	return n, err
}

func newURLDownloadClient() *http.Client {
	transport := &http.Transport{
		Proxy:                 nil,
		DialContext:           publicDialContext,
		ForceAttemptHTTP2:     true,
		DisableCompression:    true,
		TLSHandshakeTimeout:   20 * time.Second,
		ResponseHeaderTimeout: 45 * time.Second,
		IdleConnTimeout:       30 * time.Second,
	}
	return &http.Client{
		Transport: transport,
		CheckRedirect: func(req *http.Request, via []*http.Request) error {
			if len(via) >= 6 {
				return errors.New("直链重定向次数过多")
			}
			_, err := validateURLDownload(req.URL.String())
			return err
		},
	}
}

func validateURLDownload(raw string) (string, error) {
	raw = strings.TrimSpace(raw)
	if raw == "" || len(raw) > maxURLLength {
		return "", errors.New("下载链接为空或过长")
	}
	u, err := url.Parse(raw)
	if err != nil || u.Host == "" || (u.Scheme != "http" && u.Scheme != "https") {
		return "", errors.New("请输入完整的 HTTP 或 HTTPS 下载链接")
	}
	if u.User != nil {
		return "", errors.New("下载链接不能包含用户名或密码")
	}
	if u.Hostname() == "" {
		return "", errors.New("下载链接缺少有效主机名")
	}
	u.Fragment = ""
	return u.String(), nil
}

func urlDownloadName(raw string) string {
	u, err := url.Parse(raw)
	if err == nil {
		name := path.Base(strings.TrimSpace(u.Path))
		if decoded, decodeErr := url.PathUnescape(name); decodeErr == nil {
			name = decoded
		}
		name = strings.TrimSpace(strings.ReplaceAll(name, "\\", "_"))
		if validateName(name) == nil {
			return name
		}
	}
	return "direct-download"
}

func responseDownloadName(response *http.Response, fallback string) string {
	if disposition := response.Header.Get("Content-Disposition"); disposition != "" {
		if _, params, err := mime.ParseMediaType(disposition); err == nil {
			name := path.Base(strings.ReplaceAll(strings.TrimSpace(params["filename"]), "\\", "/"))
			if validateName(name) == nil {
				return name
			}
		}
	}
	if name := urlDownloadName(response.Request.URL.String()); name != "direct-download" {
		return name
	}
	return fallback
}

func responseDownloadMIME(response *http.Response, name string) string {
	if value := response.Header.Get("Content-Type"); value != "" {
		if mediaType, _, err := mime.ParseMediaType(value); err == nil && mediaType != "" {
			return mediaType
		}
	}
	if mediaType := mime.TypeByExtension(strings.ToLower(path.Ext(name))); mediaType != "" {
		return mediaType
	}
	return "application/octet-stream"
}

func (m *downloadManager) restoreURLDownloads() {
	_, _ = m.server.db.ExecContext(m.ctx, `UPDATE url_download_jobs SET status='queued',download_speed=0,completed_size=0,error='',updated_at=? WHERE status='downloading'`, time.Now().UTC().Format(time.RFC3339Nano))
	rows, err := m.server.db.QueryContext(m.ctx, `SELECT id FROM url_download_jobs WHERE status='queued' ORDER BY created_at`)
	if err != nil {
		m.server.log.Error("direct download restore scan failed", "error", err)
		return
	}
	var jobIDs []string
	for rows.Next() {
		var id string
		if rows.Scan(&id) == nil {
			jobIDs = append(jobIDs, id)
		}
	}
	rows.Close()
	for _, id := range jobIDs {
		m.startURLDownload(id)
	}
}

func (m *downloadManager) createURL(ctx context.Context, parentID, rawURL string) (downloadJob, error) {
	sourceURL, err := validateURLDownload(rawURL)
	if err != nil {
		return downloadJob{}, err
	}
	parent, err := m.server.file(ctx, parentID)
	if err != nil || parent.Kind != "directory" || parent.Status != "ready" || parent.DeletedAt != "" {
		return downloadJob{}, errors.New("目标目录无效")
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	jobID := ids.New()
	name := urlDownloadName(sourceURL)
	if _, err := m.server.db.ExecContext(ctx, `INSERT INTO url_download_jobs(id,parent_id,source_url,name,status,created_at,updated_at) VALUES(?,?,?,?,'queued',?,?)`, jobID, parentID, sourceURL, name, now, now); err != nil {
		return downloadJob{}, err
	}
	m.startURLDownload(jobID)
	return m.getURL(ctx, jobID)
}

func (m *downloadManager) startURLDownload(jobID string) {
	m.urlMu.Lock()
	if _, exists := m.urlJobs[jobID]; exists {
		m.urlMu.Unlock()
		return
	}
	ctx, cancel := context.WithCancel(m.ctx)
	runtime := &urlDownloadRuntime{ctx: ctx, cancel: cancel}
	m.urlJobs[jobID] = runtime
	m.urlMu.Unlock()
	go m.runURLDownload(jobID, runtime)
}

func (m *downloadManager) runURLDownload(jobID string, runtime *urlDownloadRuntime) {
	defer func() {
		m.urlMu.Lock()
		if m.urlJobs[jobID] == runtime {
			delete(m.urlJobs, jobID)
		}
		m.urlMu.Unlock()
	}()
	select {
	case m.urlSlots <- struct{}{}:
		defer func() { <-m.urlSlots }()
	case <-runtime.ctx.Done():
		return
	}
	var sourceURL string
	if err := m.server.db.QueryRowContext(runtime.ctx, `SELECT source_url FROM url_download_jobs WHERE id=? AND status='queued'`, jobID).Scan(&sourceURL); err != nil {
		return
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	if result, err := m.server.db.ExecContext(runtime.ctx, `UPDATE url_download_jobs SET status='downloading',completed_size=0,download_speed=0,error='',updated_at=? WHERE id=? AND status='queued'`, now, jobID); err != nil {
		m.failURLDownload(jobID, err)
		return
	} else if changed, _ := result.RowsAffected(); changed != 1 {
		return
	}
	request, err := http.NewRequestWithContext(runtime.ctx, http.MethodGet, sourceURL, nil)
	if err != nil {
		m.failURLDownload(jobID, err)
		return
	}
	request.Header.Set("Accept-Encoding", "identity")
	request.Header.Set("User-Agent", "revaro-direct-download/1")
	response, err := m.http.Do(request)
	if err != nil {
		if runtime.ctx.Err() == nil {
			m.failURLDownload(jobID, fmt.Errorf("无法连接下载地址: %w", err))
		}
		return
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		m.failURLDownload(jobID, fmt.Errorf("下载服务器返回 HTTP %d", response.StatusCode))
		return
	}
	if encoding := strings.TrimSpace(response.Header.Get("Content-Encoding")); encoding != "" && !strings.EqualFold(encoding, "identity") {
		m.failURLDownload(jobID, errors.New("下载服务器未提供原始文件内容"))
		return
	}
	if response.ContentLength > m.server.cfg.BTMaxTotalSize {
		m.failURLDownload(jobID, errURLDownloadTooLarge)
		return
	}
	fallback := urlDownloadName(sourceURL)
	name := responseDownloadName(response, fallback)
	if validateName(name) != nil {
		name = fallback
	}
	expected := max(response.ContentLength, 0)
	_, _ = m.server.db.ExecContext(runtime.ctx, `UPDATE url_download_jobs SET name=?,selected_size=?,updated_at=? WHERE id=? AND status='downloading'`, name, expected, time.Now().UTC().Format(time.RFC3339Nano), jobID)
	lastBytes := int64(0)
	lastUpdate := time.Now()
	progress := func(total int64) {
		now := time.Now()
		elapsed := now.Sub(lastUpdate)
		if elapsed < 500*time.Millisecond && total-lastBytes < 2<<20 {
			return
		}
		speed := int64(0)
		if elapsed > 0 && total >= lastBytes {
			speed = int64(float64(total-lastBytes) / elapsed.Seconds())
		}
		_, _ = m.server.db.ExecContext(runtime.ctx, `UPDATE url_download_jobs SET completed_size=?,download_speed=?,updated_at=? WHERE id=? AND status='downloading'`, total, max(speed, 0), now.UTC().Format(time.RFC3339Nano), jobID)
		lastBytes, lastUpdate = total, now
	}
	reader := &urlProgressReader{reader: response.Body, limit: m.server.cfg.BTMaxTotalSize, onProgress: progress}
	key, manifest, storeErr := m.server.storage.Store(runtime.ctx, reader)
	if storeErr != nil {
		if runtime.ctx.Err() == nil {
			m.failURLDownload(jobID, fmt.Errorf("保存直链文件: %w", storeErr))
		}
		return
	}
	if response.ContentLength >= 0 && manifest.Size != response.ContentLength {
		m.failURLDownload(jobID, fmt.Errorf("下载大小 %d 与服务器声明的 %d 不一致", manifest.Size, response.ContentLength))
		return
	}
	progress(manifest.Size)
	if err := m.commitURLDownload(runtime.ctx, jobID, name, responseDownloadMIME(response, name), key, manifest.Size, manifest.ID()); err != nil {
		if runtime.ctx.Err() == nil {
			m.failURLDownload(jobID, err)
		}
		return
	}
	m.server.log.Info("direct download imported", "job", jobID, "name", name, "size", manifest.Size)
}

func (m *downloadManager) commitURLDownload(ctx context.Context, jobID, name, mimeType, objectKey string, size int64, etag string) error {
	tx, err := m.server.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	var parentID string
	if err := tx.QueryRowContext(ctx, `SELECT parent_id FROM url_download_jobs WHERE id=? AND status='downloading'`, jobID).Scan(&parentID); err != nil {
		return err
	}
	var targetExists int
	if err := tx.QueryRowContext(ctx, `SELECT COUNT(*) FROM files WHERE id=? AND kind='directory' AND status='ready' AND deleted_at IS NULL`, parentID).Scan(&targetExists); err != nil {
		return err
	}
	if targetExists == 0 {
		return errors.New("目标目录已被删除或不可用")
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	if _, err := tx.ExecContext(ctx, `INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at) VALUES(?,?,?,'file',?,?,?,?,'ready',?,?)`, ids.New(), parentID, name, objectKey, size, mimeType, etag, now, now); err != nil {
		if isConflict(err) {
			return errors.New("目标目录中已经有同名文件")
		}
		return err
	}
	if _, err := tx.ExecContext(ctx, `UPDATE url_download_jobs SET status='done',selected_size=?,completed_size=?,download_speed=0,error='',updated_at=? WHERE id=?`, size, size, now, jobID); err != nil {
		return err
	}
	return tx.Commit()
}

func (m *downloadManager) failURLDownload(jobID string, err error) {
	_, _ = m.server.db.Exec(`UPDATE url_download_jobs SET status='failed',download_speed=0,error=?,updated_at=? WHERE id=? AND status IN ('queued','downloading')`, err.Error(), time.Now().UTC().Format(time.RFC3339Nano), jobID)
	m.server.log.Error("direct download task failed", "job", jobID, "error", err)
}

func (m *downloadManager) getURL(ctx context.Context, jobID string) (downloadJob, error) {
	var job downloadJob
	job.SourceType = "url"
	err := m.server.db.QueryRowContext(ctx, `SELECT id,parent_id,name,status,selected_size,completed_size,download_speed,error,created_at,updated_at FROM url_download_jobs WHERE id=?`, jobID).Scan(&job.ID, &job.ParentID, &job.Name, &job.Status, &job.SelectedSize, &job.CompletedSize, &job.DownloadSpeed, &job.Error, &job.CreatedAt, &job.UpdatedAt)
	return job, err
}

func (m *downloadManager) listURL(ctx context.Context) ([]downloadJob, error) {
	rows, err := m.server.db.QueryContext(ctx, `SELECT id,parent_id,name,status,selected_size,completed_size,download_speed,error,created_at,updated_at FROM url_download_jobs ORDER BY created_at DESC`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	items := []downloadJob{}
	for rows.Next() {
		job := downloadJob{SourceType: "url"}
		if err := rows.Scan(&job.ID, &job.ParentID, &job.Name, &job.Status, &job.SelectedSize, &job.CompletedSize, &job.DownloadSpeed, &job.Error, &job.CreatedAt, &job.UpdatedAt); err != nil {
			return nil, err
		}
		items = append(items, job)
	}
	return items, rows.Err()
}

func (m *downloadManager) listAll(ctx context.Context) ([]downloadJob, error) {
	torrents, err := m.list(ctx)
	if err != nil {
		return nil, err
	}
	urls, err := m.listURL(ctx)
	if err != nil {
		return nil, err
	}
	items := append(torrents, urls...)
	sort.Slice(items, func(i, j int) bool { return items[i].CreatedAt > items[j].CreatedAt })
	return items, nil
}

func (m *downloadManager) getAny(ctx context.Context, jobID string, withFiles bool) (downloadJob, error) {
	job, err := m.get(ctx, jobID, withFiles)
	if !errors.Is(err, sql.ErrNoRows) {
		return job, err
	}
	return m.getURL(ctx, jobID)
}

func (m *downloadManager) pauseURL(ctx context.Context, jobID string) error {
	job, err := m.getURL(ctx, jobID)
	if err != nil || (job.Status != "queued" && job.Status != "downloading") {
		return errors.New("任务当前不能暂停")
	}
	if result, err := m.server.db.ExecContext(ctx, `UPDATE url_download_jobs SET status='paused',download_speed=0,updated_at=? WHERE id=? AND status IN ('queued','downloading')`, time.Now().UTC().Format(time.RFC3339Nano), jobID); err != nil {
		return err
	} else if changed, _ := result.RowsAffected(); changed != 1 {
		return errors.New("任务当前不能暂停")
	}
	m.urlMu.Lock()
	if runtime := m.urlJobs[jobID]; runtime != nil {
		runtime.cancel()
		delete(m.urlJobs, jobID)
	}
	m.urlMu.Unlock()
	return nil
}

func (m *downloadManager) resumeURL(ctx context.Context, jobID string) error {
	if result, err := m.server.db.ExecContext(ctx, `UPDATE url_download_jobs SET status='queued',completed_size=0,download_speed=0,error='',updated_at=? WHERE id=? AND status='paused'`, time.Now().UTC().Format(time.RFC3339Nano), jobID); err != nil {
		return err
	} else if changed, _ := result.RowsAffected(); changed != 1 {
		return errors.New("任务当前不能继续")
	}
	m.startURLDownload(jobID)
	return nil
}

func (m *downloadManager) removeURL(ctx context.Context, jobID string) error {
	if _, err := m.getURL(ctx, jobID); err != nil {
		return err
	}
	m.urlMu.Lock()
	if runtime := m.urlJobs[jobID]; runtime != nil {
		runtime.cancel()
		delete(m.urlJobs, jobID)
	}
	m.urlMu.Unlock()
	_, err := m.server.db.ExecContext(ctx, `DELETE FROM url_download_jobs WHERE id=?`, jobID)
	return err
}

func (m *downloadManager) pauseAny(ctx context.Context, jobID string) error {
	job, err := m.getAny(ctx, jobID, false)
	if err != nil {
		return err
	}
	if job.SourceType == "url" {
		return m.pauseURL(ctx, jobID)
	}
	return m.pause(ctx, jobID)
}

func (m *downloadManager) resumeAny(ctx context.Context, jobID string) error {
	job, err := m.getAny(ctx, jobID, false)
	if err != nil {
		return err
	}
	if job.SourceType == "url" {
		return m.resumeURL(ctx, jobID)
	}
	return m.resume(ctx, jobID)
}

func (m *downloadManager) removeAny(ctx context.Context, jobID string) error {
	job, err := m.getAny(ctx, jobID, false)
	if err != nil {
		return err
	}
	if job.SourceType == "url" {
		return m.removeURL(ctx, jobID)
	}
	return m.remove(ctx, jobID)
}

func (m *downloadManager) cleanupURLLoop() {
	ticker := time.NewTicker(time.Hour)
	defer ticker.Stop()
	for {
		select {
		case <-m.ctx.Done():
			return
		case <-ticker.C:
			cutoff := time.Now().UTC().Add(-m.server.cfg.BTStaleAfter).Format(time.RFC3339Nano)
			_, _ = m.server.db.ExecContext(m.ctx, `DELETE FROM url_download_jobs WHERE status IN ('failed','cancelled') AND updated_at<?`, cutoff)
		}
	}
}
