package server

import (
	"context"
	"errors"
	"fmt"
	"mime"
	"path"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
	"github.com/VesperGlow/revaro/internal/storage"
)

type importedDownloadFile struct {
	path, objectKey, mimeType, etag string
	size                            int64
	index                           int
	web                             *storage.WebMediaAsset
}

func (m *downloadManager) importRuntime(runtime *downloadRuntime) {
	runtime.mu.Lock()
	if runtime.importing {
		runtime.mu.Unlock()
		return
	}
	runtime.importing = true
	runtime.mu.Unlock()
	defer func() { runtime.mu.Lock(); runtime.importing = false; runtime.mu.Unlock() }()
	release, resourceErr := m.server.tasks.IO(runtime.ctx)
	if resourceErr != nil {
		m.fail(runtime.jobID, resourceErr)
		return
	}
	defer release()
	job, err := m.get(runtime.ctx, runtime.jobID, true)
	if err != nil {
		m.fail(runtime.jobID, err)
		return
	}
	// A restored import starts from the first selected file again. Content
	// addressing makes already uploaded blocks cheap to deduplicate, while a
	// reset counter keeps the displayed progress honest after a restart.
	_, _ = m.server.db.ExecContext(runtime.ctx, `UPDATE download_jobs SET imported_size=0,import_speed=0,current_file='',updated_at=? WHERE id=?`, time.Now().UTC().Format(time.RFC3339Nano), job.ID)
	requests := make([]storage.TorrentImportFile, 0)
	paths := make(map[int]downloadFile)
	for _, item := range job.Files {
		if !item.Selected {
			continue
		}
		fileName := path.Base(item.Path)
		mimeType := mime.TypeByExtension(strings.ToLower(filepath.Ext(fileName)))
		if mimeType == "" {
			mimeType = "application/octet-stream"
		}
		key := storage.BlobKey(fmt.Sprintf("bt-%s-%d", job.ID, item.Index))
		webPrefix := ""
		if strings.HasPrefix(strings.ToLower(mimeType), "video/") || videoExts[strings.ToLower(filepath.Ext(fileName))] {
			webPrefix = fmt.Sprintf("derived/media/%s/%d", job.ID, item.Index)
		}
		requests = append(requests, storage.TorrentImportFile{Index: item.Index, Key: key, MIME: mimeType, Size: item.Size, WebPrefix: webPrefix})
		paths[item.Index] = item
	}
	started := time.Now()
	_, _ = m.server.db.ExecContext(runtime.ctx, `UPDATE download_jobs SET ingest_state='probing',updated_at=? WHERE id=?`, time.Now().UTC().Format(time.RFC3339Nano), job.ID)
	m.server.jobs.Changed()
	_, _ = m.server.db.ExecContext(runtime.ctx, `UPDATE download_jobs SET ingest_state='processing',updated_at=? WHERE id=?`, time.Now().UTC().Format(time.RFC3339Nano), job.ID)
	m.server.jobs.Changed()
	results, err := m.bt.ImportTorrent(runtime.ctx, runtime.torrentID, requests)
	if err != nil {
		m.cleanupTorrentImport(requests)
		m.fail(runtime.jobID, fmt.Errorf("导入种子文件: %w", err))
		return
	}
	stored := make([]importedDownloadFile, 0, len(results))
	var imported int64
	for _, result := range results {
		item, ok := paths[result.Index]
		if !ok {
			m.cleanupTorrentImport(requests)
			m.fail(runtime.jobID, errors.New("种子导入结果不一致"))
			return
		}
		if result.Consumed {
			continue
		}
		if result.WebMedia != nil && result.WebMedia.State == "unsupported" {
			m.cleanupTorrentImport(requests)
			m.unsupported(runtime.jobID, result.Index, result.WebMedia.Error)
			return
		}
		if result.WebMedia == nil && result.Size != item.Size || result.WebMedia != nil && (result.WebMedia.State != "completed" || result.Key == "") {
			m.cleanupTorrentImport(requests)
			m.fail(runtime.jobID, errors.New("种子导入结果不一致"))
			return
		}
		mimeType := mime.TypeByExtension(strings.ToLower(filepath.Ext(item.Path)))
		if mimeType == "" {
			mimeType = "application/octet-stream"
		}
		stored = append(stored, importedDownloadFile{path: item.Path, objectKey: result.Key, size: result.Size, mimeType: mimeType, etag: result.ETag, index: result.Index, web: result.WebMedia})
		imported += result.Size
	}
	elapsed := max(time.Since(started), time.Millisecond)
	_, _ = m.server.db.ExecContext(runtime.ctx, `UPDATE download_jobs SET ingest_state='uploading',updated_at=? WHERE id=?`, time.Now().UTC().Format(time.RFC3339Nano), job.ID)
	_, _ = m.server.db.ExecContext(runtime.ctx, `UPDATE download_jobs SET imported_size=?,import_speed=?,current_file='',updated_at=? WHERE id=? AND status='importing'`, imported, int64(float64(imported)/elapsed.Seconds()), time.Now().UTC().Format(time.RFC3339Nano), job.ID)
	m.server.jobs.Changed()
	if err := m.publishImported(runtime.ctx, job, stored, len(job.Files) > 1); err != nil {
		m.cleanupTorrentImport(requests)
		m.fail(runtime.jobID, err)
		return
	}
	runtime.cancel()
	deleteCtx, cancel := context.WithTimeout(context.Background(), time.Minute)
	if err := m.bt.DeleteTorrent(deleteCtx, runtime.torrentID); err != nil {
		m.server.log.Warn("completed torrent cleanup failed", "job", job.ID, "error", err)
	}
	cancel()
	m.mu.Lock()
	delete(m.jobs, runtime.jobID)
	m.mu.Unlock()
	m.server.log.Info("built-in torrent download imported", "job", job.ID, "name", job.Name, "files", len(stored), "size", job.SelectedSize)
}

func (m *downloadManager) publishImported(ctx context.Context, job downloadJob, files []importedDownloadFile, preserveRoot bool) error {
	if err := m.commitImported(ctx, job, files, preserveRoot); err != nil {
		return err
	}
	// Publish only after the terminal transaction is visible. Without this SSE
	// notification the browser retains its last "processing" snapshot.
	m.server.jobs.Changed()
	return nil
}

func (m *downloadManager) cleanupTorrentImport(files []storage.TorrentImportFile) {
	keys := make([]string, 0, len(files))
	for _, file := range files {
		if file.Key != "" {
			keys = append(keys, file.Key)
		}
		if file.WebPrefix != "" {
			ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
			objects, err := m.server.objects.ListPrefix(ctx, file.WebPrefix+"/")
			cancel()
			if err == nil {
				for _, object := range objects {
					keys = append(keys, object.Key)
				}
			}
		}
	}
	for len(keys) > 0 {
		batch := keys
		if len(batch) > 1000 {
			batch = keys[:1000]
		}
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		err := m.server.objects.DeleteMany(ctx, batch, "torrent import rollback")
		cancel()
		if err != nil {
			m.server.log.Warn("failed torrent import object cleanup failed", "objects", len(batch), "error", err)
		}
		keys = keys[len(batch):]
	}
}

func (m *downloadManager) commitImported(ctx context.Context, job downloadJob, files []importedDownloadFile, multi bool) error {
	if len(files) == 0 {
		return errors.New("没有可导入的种子文件")
	}
	tx, err := m.server.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	now := time.Now().UTC().Format(time.RFC3339Nano)
	parentID := job.ParentID
	var targetExists int
	if err := tx.QueryRowContext(ctx, `SELECT COUNT(*) FROM files WHERE id=? AND kind='directory' AND status='ready' AND deleted_at IS NULL`, parentID).Scan(&targetExists); err != nil {
		return err
	}
	if targetExists == 0 {
		return errors.New("目标目录已被删除或不可用")
	}
	if multi {
		rootName := strings.TrimSpace(job.Name)
		if err := validateName(rootName); err != nil {
			return fmt.Errorf("种子根目录名称无效: %w", err)
		}
		rootID := ids.New()
		if _, err := tx.Exec(`INSERT INTO files(id,parent_id,name,kind,status,created_at,updated_at) VALUES(?,?,?,'directory','ready',?,?)`, rootID, parentID, rootName, now, now); err != nil {
			if isConflict(err) {
				return errors.New("目标目录中已经有同名项目")
			}
			return err
		}
		parentID = rootID
	}
	dirs := map[string]string{"": parentID}
	importedVideos := make([]File, 0)
	sort.Slice(files, func(i, j int) bool { return files[i].path < files[j].path })
	for _, file := range files {
		rel := file.path
		if !multi {
			rel = path.Base(rel)
		}
		parts := strings.Split(rel, "/")
		currentPath, currentParent := "", parentID
		for _, component := range parts[:len(parts)-1] {
			nextPath := component
			if currentPath != "" {
				nextPath = currentPath + "/" + component
			}
			if existing := dirs[nextPath]; existing != "" {
				currentParent, currentPath = existing, nextPath
				continue
			}
			dirID := ids.New()
			if _, err := tx.Exec(`INSERT INTO files(id,parent_id,name,kind,status,created_at,updated_at) VALUES(?,?,?,'directory','ready',?,?)`, dirID, currentParent, component, now, now); err != nil {
				return err
			}
			dirs[nextPath] = dirID
			currentParent, currentPath = dirID, nextPath
		}
		name := parts[len(parts)-1]
		fileID := fmt.Sprintf("bt-%s-%d", job.ID, file.index)
		if _, err := tx.Exec(`INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at) VALUES(?,?,?,'file',?,?,?,?,'ready',?,?)`, fileID, currentParent, name, file.objectKey, file.size, file.mimeType, file.etag, now, now); err != nil {
			if isConflict(err) {
				return errors.New("目标目录中存在同名文件")
			}
			return err
		}
		importedVideos = append(importedVideos, File{ID: fileID, ParentID: &currentParent, Name: name, Kind: "file", Size: file.size, MimeType: file.mimeType, ETag: file.etag, Status: "ready", CreatedAt: now, UpdatedAt: now, objectKey: file.objectKey})
		if file.web != nil {
			state := file.web.State
			if state == "" {
				state = "failed"
			}
			if _, err := tx.Exec(`INSERT INTO web_media_ingests(file_id,download_job_id,file_index,state,video_codec,audio_codec,error,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?)`, fileID, job.ID, file.index, state, file.web.VideoCodec, file.web.AudioCodec, file.web.Error, now, now); err != nil {
				return err
			}
			if state == "completed" {
				if _, err := tx.Exec(`INSERT INTO web_media_playback(file_id,object_key,size,etag,duration_ms,video_codec,audio_codec,created_at) VALUES(?,?,?,?,?,?,?,?)`, fileID, file.web.Key, file.web.Size, file.web.ETag, file.web.DurationMS, file.web.VideoCodec, file.web.AudioCodec, now); err != nil {
					return err
				}
				for _, sub := range file.web.Subtitles {
					if _, err := tx.Exec(`INSERT INTO web_media_subtitles(file_id,track_index,object_key,size,etag,language,title,is_default,is_forced) VALUES(?,?,?,?,?,?,?,?,?)`, fileID, sub.Index, sub.Key, sub.Size, sub.ETag, sub.Language, sub.Title, sub.Default, sub.Forced); err != nil {
						return err
					}
				}
			}
		}
	}
	if _, err := tx.ExecContext(ctx, `UPDATE download_jobs SET status='done',ingest_state='completed',completed_size=selected_size,download_speed=0,imported_size=selected_size,import_speed=0,current_file='',peers=0,error='',updated_at=? WHERE id=?`, now, job.ID); err != nil {
		return err
	}
	if err := tx.Commit(); err != nil {
		return err
	}
	for _, file := range importedVideos {
		m.server.scheduleVideoThumbnail(file)
	}
	return nil
}

func (m *downloadManager) pause(ctx context.Context, jobID string) error {
	m.mu.RLock()
	runtime := m.jobs[jobID]
	m.mu.RUnlock()
	if runtime == nil {
		return errors.New("下载任务未运行")
	}
	job, err := m.get(ctx, jobID, false)
	if err != nil || (job.Status != "downloading" && job.Status != "queued") {
		return errors.New("任务当前不能暂停")
	}
	if err := m.bt.PauseTorrent(ctx, runtime.torrentID); err != nil {
		return err
	}
	m.setStatus(jobID, "paused", "")
	return nil
}

func (m *downloadManager) resume(ctx context.Context, jobID string) error {
	m.mu.RLock()
	runtime := m.jobs[jobID]
	m.mu.RUnlock()
	if runtime == nil {
		return errors.New("下载任务未运行")
	}
	job, err := m.get(ctx, jobID, false)
	if err != nil || (job.Status != "paused" && job.Status != "failed") {
		return errors.New("任务当前不能继续")
	}
	if job.Status == "failed" && job.CompletedSize >= job.SelectedSize && job.SelectedSize > 0 {
		runtime.mu.Lock()
		finishing := runtime.importing
		runtime.mu.Unlock()
		if finishing {
			return errors.New("上一次导入仍在收尾，请稍后重试")
		}
		_, err := m.server.db.ExecContext(ctx, `UPDATE download_jobs SET status='importing',ingest_state='probing',imported_size=0,import_speed=0,current_file='',download_speed=0,peers=0,error='',updated_at=? WHERE id=? AND status='failed'`, time.Now().UTC().Format(time.RFC3339Nano), jobID)
		if err != nil {
			return err
		}
		m.server.jobs.Changed()
		if !m.runBackground(func() { m.importRuntime(runtime) }) {
			return context.Canceled
		}
		return nil
	}
	runtime.mu.Lock()
	active := runtime.starting
	runtime.mu.Unlock()
	if active {
		if err := m.applySelectedPriorities(runtime, true); err != nil {
			return err
		}
		m.setStatus(jobID, "downloading", "")
		return nil
	}
	return m.startRuntime(runtime)
}

func (m *downloadManager) remove(ctx context.Context, jobID string) error {
	job, err := m.get(ctx, jobID, true)
	if err != nil {
		return err
	}
	m.mu.Lock()
	runtime := m.jobs[jobID]
	delete(m.jobs, jobID)
	m.mu.Unlock()
	if runtime != nil {
		if runtime.cancel != nil {
			runtime.cancel()
		}
		if job.Status != "done" {
			m.cleanupTorrentImport(m.importRequests(job))
		}
		if err := m.bt.DeleteTorrent(ctx, runtime.torrentID); err != nil {
			return err
		}
	}
	if _, err := m.server.db.ExecContext(ctx, `DELETE FROM download_jobs WHERE id=?`, jobID); err != nil {
		return err
	}
	return nil
}

func (m *downloadManager) importRequests(job downloadJob) []storage.TorrentImportFile {
	requests := make([]storage.TorrentImportFile, 0, len(job.Files))
	for _, item := range job.Files {
		if !item.Selected {
			continue
		}
		fileName := path.Base(item.Path)
		mimeType := mime.TypeByExtension(strings.ToLower(filepath.Ext(fileName)))
		if mimeType == "" {
			mimeType = "application/octet-stream"
		}
		request := storage.TorrentImportFile{Index: item.Index, MIME: mimeType, Size: item.Size}
		if strings.HasPrefix(strings.ToLower(mimeType), "video/") || videoExts[strings.ToLower(filepath.Ext(fileName))] {
			request.WebPrefix = fmt.Sprintf("derived/media/%s/%d", job.ID, item.Index)
		} else {
			request.Key = storage.BlobKey(fmt.Sprintf("bt-%s-%d", job.ID, item.Index))
		}
		requests = append(requests, request)
	}
	return requests
}

func (m *downloadManager) get(ctx context.Context, jobID string, withFiles bool) (downloadJob, error) {
	var job downloadJob
	err := m.server.db.QueryRowContext(ctx, `SELECT id,parent_id,source_type,COALESCE(info_hash,''),name,status,ingest_state,selected_size,completed_size,download_speed,imported_size,import_speed,current_file,peers,error,created_at,updated_at FROM download_jobs WHERE id=?`, jobID).Scan(&job.ID, &job.ParentID, &job.SourceType, &job.InfoHash, &job.Name, &job.Status, &job.IngestState, &job.SelectedSize, &job.CompletedSize, &job.DownloadSpeed, &job.ImportedSize, &job.ImportSpeed, &job.CurrentFile, &job.Peers, &job.Error, &job.CreatedAt, &job.UpdatedAt)
	if err != nil {
		return job, err
	}
	if !withFiles {
		return job, nil
	}
	rows, err := m.server.db.QueryContext(ctx, `SELECT file_index,path,size,selected FROM download_files WHERE job_id=? ORDER BY file_index`, jobID)
	if err != nil {
		return job, err
	}
	defer rows.Close()
	for rows.Next() {
		var file downloadFile
		if err := rows.Scan(&file.Index, &file.Path, &file.Size, &file.Selected); err != nil {
			return job, err
		}
		job.Files = append(job.Files, file)
	}
	return job, rows.Err()
}

func (m *downloadManager) setStatus(jobID, status, jobError string) {
	_, _ = m.server.db.Exec(`UPDATE download_jobs SET status=?,error=?,download_speed=CASE WHEN ?='downloading' THEN download_speed ELSE 0 END,import_speed=CASE WHEN ?='importing' THEN import_speed ELSE 0 END,current_file=CASE WHEN ?='importing' THEN current_file ELSE '' END,updated_at=? WHERE id=?`, status, jobError, status, status, status, time.Now().UTC().Format(time.RFC3339Nano), jobID)
	m.server.jobs.Changed()
}

func (m *downloadManager) fail(jobID string, err error) {
	_, _ = m.server.db.Exec(`UPDATE download_jobs SET ingest_state='failed' WHERE id=?`, jobID)
	m.setStatus(jobID, "failed", err.Error())
	m.mu.RLock()
	runtime := m.jobs[jobID]
	m.mu.RUnlock()
	if runtime != nil {
		// A failed processing/uploading attempt is retryable. Pause and retain
		// librqbit's verified files; only success, user removal/cancellation, or
		// the configured stale cleanup path may delete torrent staging.
		ctx, cancel := context.WithTimeout(context.Background(), time.Minute)
		if pauseErr := m.bt.PauseTorrent(ctx, runtime.torrentID); pauseErr != nil {
			m.server.log.Warn("failed torrent pause for retained staging", "job", jobID, "error", pauseErr)
		}
		cancel()
	}
	m.server.log.Error("built-in torrent task failed; staging retained for retry", "job", jobID, "error", err)
}

func (m *downloadManager) unsupported(jobID string, fileIndex int, message string) {
	now := time.Now().UTC().Format(time.RFC3339Nano)
	tx, err := m.server.db.Begin()
	if err == nil {
		_, err = tx.Exec(`INSERT INTO web_media_ingests(download_job_id,file_index,file_id,state,error,created_at,updated_at) VALUES(?,?,NULL,'unsupported',?,?,?) ON CONFLICT(download_job_id,file_index) DO UPDATE SET file_id=NULL,state='unsupported',error=excluded.error,updated_at=excluded.updated_at`, jobID, fileIndex, message, now, now)
		if err == nil {
			_, err = tx.Exec(`UPDATE download_jobs SET status='failed',ingest_state='unsupported',download_speed=0,import_speed=0,peers=0,error=?,updated_at=? WHERE id=?`, message, now, jobID)
		}
		if err == nil {
			err = tx.Commit()
		} else {
			_ = tx.Rollback()
		}
	}
	if err != nil {
		m.server.log.Error("could not persist unsupported BT media state", "job", jobID, "error", err)
	}
	m.server.jobs.Changed()
	m.server.log.Warn("built-in torrent media unsupported; staging retained", "job", jobID, "file_index", fileIndex, "error", message)
}

func (m *downloadManager) cleanupPass(ctx context.Context) error {
	cutoff := time.Now().UTC().Add(-m.server.cfg.BTStaleAfter).Format(time.RFC3339Nano)
	rows, err := m.server.db.QueryContext(ctx, `SELECT id FROM download_jobs WHERE status IN ('failed','cancelled') AND updated_at<?`, cutoff)
	if err != nil {
		return err
	}
	defer rows.Close()
	var ids []string
	for rows.Next() {
		var id string
		if rows.Scan(&id) == nil {
			ids = append(ids, id)
		}
	}
	for _, id := range ids {
		if err := m.remove(ctx, id); err != nil {
			return err
		}
	}
	return rows.Err()
}
