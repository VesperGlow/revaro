package server

import (
	"context"
	"database/sql"
	"encoding/base64"
	"errors"
	"fmt"
	"io"
	"mime"
	"net"
	"net/http"
	"net/netip"
	"path"
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

const maxTorrentMetadataBytes = 4 << 20

type downloadManager struct {
	server *Server
	bt     storage.TorrentEngine
	http   *http.Client
	ctx    context.Context
	cancel context.CancelFunc

	mu               sync.RWMutex
	jobs             map[string]*downloadRuntime
	urlMu            sync.Mutex
	urlJobs          map[string]*urlDownloadRuntime
	urlSlots         chan struct{}
	lifecycleMu      sync.Mutex
	lifecycleClosing bool
	lifecycleWG      sync.WaitGroup
}

type downloadRuntime struct {
	mu        sync.Mutex
	jobID     string
	torrentID int
	ctx       context.Context
	cancel    context.CancelFunc
	lastRead  int64
	lastTick  time.Time
	starting  bool
	importing bool
}

type downloadJob struct {
	ID            string         `json:"id"`
	ParentID      string         `json:"parent_id"`
	SourceType    string         `json:"source_type"`
	InfoHash      string         `json:"info_hash,omitempty"`
	Name          string         `json:"name"`
	Status        string         `json:"status"`
	IngestState   string         `json:"ingest_state"`
	SelectedSize  int64          `json:"selected_size"`
	CompletedSize int64          `json:"completed_size"`
	DownloadSpeed int64          `json:"download_speed"`
	ImportedSize  int64          `json:"imported_size"`
	ImportSpeed   int64          `json:"import_speed"`
	CurrentFile   string         `json:"current_file,omitempty"`
	Peers         int            `json:"peers"`
	Error         string         `json:"error,omitempty"`
	CreatedAt     string         `json:"created_at"`
	UpdatedAt     string         `json:"updated_at"`
	Files         []downloadFile `json:"files,omitempty"`
}

type downloadFile struct {
	Index    int    `json:"index"`
	Path     string `json:"path"`
	Size     int64  `json:"size"`
	Selected bool   `json:"selected"`
}

func newDownloadManager(s *Server) (*downloadManager, error) {
	engine, ok := s.storage.(storage.TorrentEngine)
	if !ok {
		return nil, errors.New("Rust torrent engine is unavailable")
	}
	ctx, cancel := context.WithCancel(context.Background())
	m := &downloadManager{
		server: s, bt: engine, http: newURLDownloadClient(),
		ctx: ctx, cancel: cancel, jobs: make(map[string]*downloadRuntime),
		urlJobs: make(map[string]*urlDownloadRuntime), urlSlots: make(chan struct{}, 2),
	}
	m.runBackground(m.restore)
	m.runBackground(m.restoreURLDownloads)
	m.runBackground(m.cleanupLoop)
	m.runBackground(m.cleanupURLLoop)
	return m, nil
}

func (m *downloadManager) runBackground(work func()) bool {
	m.lifecycleMu.Lock()
	defer m.lifecycleMu.Unlock()
	if m.lifecycleClosing {
		return false
	}
	m.lifecycleWG.Add(1)
	go func() {
		defer m.lifecycleWG.Done()
		work()
	}()
	return true
}

func (m *downloadManager) Close() {
	m.lifecycleMu.Lock()
	m.lifecycleClosing = true
	m.lifecycleMu.Unlock()
	m.cancel()
	m.mu.Lock()
	for _, runtime := range m.jobs {
		if runtime.cancel != nil {
			runtime.cancel()
		}
	}
	m.jobs = make(map[string]*downloadRuntime)
	m.mu.Unlock()
	m.urlMu.Lock()
	for _, runtime := range m.urlJobs {
		runtime.cancel()
	}
	m.urlJobs = make(map[string]*urlDownloadRuntime)
	m.urlMu.Unlock()
	m.lifecycleWG.Wait()
}

func publicDialContext(ctx context.Context, network, address string) (net.Conn, error) {
	host, port, err := net.SplitHostPort(address)
	if err != nil {
		return nil, err
	}
	resolved, err := net.DefaultResolver.LookupIPAddr(ctx, host)
	if err != nil {
		return nil, err
	}
	var lastDialErr error
	for _, candidate := range resolved {
		ip := candidate.IP
		if !isPublicDownloadIP(ip) {
			continue
		}
		connection, dialErr := (&net.Dialer{Timeout: 20 * time.Second}).DialContext(ctx, network, net.JoinHostPort(ip.String(), port))
		if dialErr == nil {
			return connection, nil
		}
		lastDialErr = dialErr
	}
	if lastDialErr != nil {
		return nil, lastDialErr
	}
	return nil, errors.New("destination resolves only to a blocked address")
}

func isPublicDownloadIP(ip net.IP) bool {
	addr, ok := netip.AddrFromSlice(ip)
	if !ok {
		return false
	}
	addr = addr.Unmap()
	if !addr.IsGlobalUnicast() || addr.IsPrivate() || addr.IsLoopback() || addr.IsLinkLocalUnicast() {
		return false
	}
	// Go's IsGlobalUnicast intentionally includes special-use ranges that are
	// not valid Internet download destinations (documentation, benchmarking,
	// carrier NAT, protocol assignment and discard-only networks).
	for _, prefix := range blockedDownloadPrefixes {
		if prefix.Contains(addr) {
			return false
		}
	}
	return true
}

var blockedDownloadPrefixes = func() []netip.Prefix {
	raw := []string{
		"0.0.0.0/8", "100.64.0.0/10", "192.0.0.0/24", "192.0.2.0/24",
		"198.18.0.0/15", "198.51.100.0/24", "203.0.113.0/24", "240.0.0.0/4",
		"2001:db8::/32", "2001:2::/48", "2001:10::/28", "100::/64",
	}
	prefixes := make([]netip.Prefix, 0, len(raw))
	for _, value := range raw {
		prefixes = append(prefixes, netip.MustParsePrefix(value))
	}
	return prefixes
}()

func (m *downloadManager) restore() {
	rows, err := m.server.db.QueryContext(m.ctx, `SELECT id,source_type,source,metainfo,status FROM download_jobs WHERE status IN ('metadata','waiting','queued','downloading','paused','importing','failed') ORDER BY created_at`)
	if err != nil {
		m.server.log.Error("download task restore scan failed", "error", err)
		return
	}
	type saved struct {
		id, sourceType, source, status string
		metainfo                       []byte
	}
	var jobs []saved
	for rows.Next() {
		var item saved
		if rows.Scan(&item.id, &item.sourceType, &item.source, &item.metainfo, &item.status) == nil {
			jobs = append(jobs, item)
		}
	}
	rows.Close()
	for _, item := range jobs {
		if err := m.attach(item.id, item.sourceType, item.source, item.metainfo, item.status); err != nil {
			m.fail(item.id, fmt.Errorf("restore download: %w", err))
		}
	}
}

func (m *downloadManager) attach(jobID, sourceType, source string, encodedMeta []byte, previousStatus string) error {
	if source == "" && len(encodedMeta) > 0 {
		sourceType, source = "torrent", base64.StdEncoding.EncodeToString(encodedMeta)
	}
	result, err := m.bt.AddTorrent(m.ctx, sourceType, source, nil, true)
	if err != nil {
		return err
	}
	ctx, cancel := context.WithCancel(m.ctx)
	runtime := &downloadRuntime{jobID: jobID, torrentID: result.ID, ctx: ctx, cancel: cancel, lastTick: time.Now()}
	m.mu.Lock()
	m.jobs[jobID] = runtime
	m.mu.Unlock()
	if err := m.persistMetadata(jobID, result.Details); err != nil {
		cancel()
		return err
	}
	switch previousStatus {
	case "queued", "downloading":
		return m.startRuntime(runtime)
	case "paused":
		m.setStatus(jobID, "paused", "")
	case "importing":
		if !m.runBackground(func() { m.importRuntime(runtime) }) {
			cancel()
		}
	case "failed":
		// Failed downloads and ingests retain their staging. Reattach them in a
		// paused state so a user retry can continue without fetching the payload
		// again; removal and stale-task cleanup remain explicit deletion paths.
		_ = m.bt.PauseTorrent(runtime.ctx, runtime.torrentID)
	default:
		m.setStatus(jobID, "waiting", "")
	}
	return nil
}

func (m *downloadManager) persistMetadata(jobID string, details storage.TorrentDetails) error {
	files := details.Files
	if len(files) == 0 || len(files) > m.server.cfg.BTMaxFiles {
		return fmt.Errorf("种子文件数必须在 1 到 %d 之间", m.server.cfg.BTMaxFiles)
	}
	var total int64
	validated := make([]downloadFile, 0, len(files))
	for index, file := range files {
		if file.Length < 0 || total > m.server.cfg.BTMaxTotalSize-file.Length {
			return fmt.Errorf("种子总大小超过 %d 字节限制", m.server.cfg.BTMaxTotalSize)
		}
		total += file.Length
		rel, err := safeTorrentPath(strings.Join(file.Components, "/"))
		if err != nil {
			return fmt.Errorf("种子包含不安全的文件路径: %w", err)
		}
		validated = append(validated, downloadFile{Index: index, Path: rel, Size: file.Length})
	}
	if len(files) > 1 {
		if err := validateName(strings.TrimSpace(details.Name)); err != nil {
			return fmt.Errorf("种子根目录名称无效: %w", err)
		}
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	tx, err := m.server.db.BeginTx(m.ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if _, err = tx.ExecContext(m.ctx, `UPDATE download_jobs SET info_hash=?,name=?,updated_at=? WHERE id=?`, details.InfoHash, strings.TrimSpace(details.Name), now, jobID); err != nil {
		return err
	}
	var existing int
	if err = tx.QueryRowContext(m.ctx, `SELECT COUNT(*) FROM download_files WHERE job_id=?`, jobID).Scan(&existing); err != nil {
		return err
	}
	if existing == 0 {
		for _, file := range validated {
			if _, err = tx.ExecContext(m.ctx, `INSERT INTO download_files(job_id,file_index,path,size,selected) VALUES(?,?,?,?,0)`, jobID, file.Index, file.Path, file.Size); err != nil {
				return err
			}
		}
	} else if existing != len(validated) {
		return errors.New("种子文件列表与已保存任务不一致")
	}
	for _, file := range validated {
		var savedPath string
		var savedSize int64
		if err = tx.QueryRowContext(m.ctx, `SELECT path,size FROM download_files WHERE job_id=? AND file_index=?`, jobID, file.Index).Scan(&savedPath, &savedSize); err != nil {
			return err
		}
		if savedPath != file.Path || savedSize != file.Size {
			return errors.New("种子文件列表与已保存任务不一致")
		}
	}
	return tx.Commit()
}

func safeTorrentPath(raw string) (string, error) {
	raw = strings.ReplaceAll(raw, "\\", "/")
	clean := path.Clean(raw)
	if clean == "." || clean == "" || strings.HasPrefix(clean, "/") || clean == ".." || strings.HasPrefix(clean, "../") || strings.ContainsRune(clean, 0) || len(clean) > 4096 {
		return "", errors.New("invalid relative path")
	}
	parts := strings.Split(clean, "/")
	for _, part := range parts {
		if err := validateName(part); err != nil {
			return "", err
		}
	}
	return strings.Join(parts, "/"), nil
}

func (m *downloadManager) create(ctx context.Context, parentID, magnet, torrentBase64 string) (downloadJob, error) {
	if (magnet == "") == (torrentBase64 == "") {
		return downloadJob{}, errors.New("请提供磁力链接或 .torrent 文件")
	}
	parent, err := m.server.file(ctx, parentID)
	if err != nil || parent.Kind != "directory" || parent.Status != "ready" {
		return downloadJob{}, errors.New("目标目录无效")
	}
	sourceType, source := "magnet", strings.TrimSpace(magnet)
	knownHash := ""
	if magnet != "" {
		if len(source) > 16384 || !strings.HasPrefix(strings.ToLower(source), "magnet:?") {
			return downloadJob{}, errors.New("磁力链接无效或过长")
		}
	} else {
		sourceType, source = "torrent", torrentBase64
		decoded, err := base64.StdEncoding.DecodeString(source)
		if err != nil || len(decoded) == 0 || len(decoded) > maxTorrentMetadataBytes {
			return downloadJob{}, errors.New(".torrent 文件无效或超过 4 MiB")
		}
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	jobID := ids.New()
	if _, err := m.server.db.ExecContext(ctx, `INSERT INTO download_jobs(id,parent_id,source_type,source,info_hash,status,created_at,updated_at) VALUES(?,?,?,?,?,'metadata',?,?)`, jobID, parentID, sourceType, source, knownHash, now, now); err != nil {
		if isConflict(err) {
			return downloadJob{}, errors.New("相同种子的任务已存在，请先删除旧任务")
		}
		return downloadJob{}, err
	}
	if err := m.attach(jobID, sourceType, source, nil, "metadata"); err != nil {
		_, _ = m.server.db.ExecContext(ctx, `DELETE FROM download_jobs WHERE id=?`, jobID)
		return downloadJob{}, err
	}
	return m.get(ctx, jobID, true)
}

func (m *downloadManager) start(ctx context.Context, jobID string, indices []int) error {
	if len(indices) == 0 {
		return errors.New("至少选择一个文件")
	}
	m.mu.RLock()
	runtime := m.jobs[jobID]
	m.mu.RUnlock()
	if runtime == nil {
		return errors.New("下载任务未运行")
	}
	job, err := m.get(ctx, jobID, true)
	if err != nil || (job.Status != "waiting" && job.Status != "paused") {
		return errors.New("任务尚未准备好")
	}
	available := make(map[int]downloadFile, len(job.Files))
	for _, file := range job.Files {
		available[file.Index] = file
	}
	unique := make(map[int]bool)
	var total int64
	for _, index := range indices {
		file, ok := available[index]
		if !ok {
			return errors.New("选择的种子文件不存在")
		}
		if !unique[index] {
			unique[index] = true
			total += file.Size
		}
	}
	if total <= 0 || total > m.server.cfg.BTMaxTotalSize {
		return errors.New("选择的文件总大小无效")
	}
	tx, err := m.server.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if _, err = tx.ExecContext(ctx, `UPDATE download_files SET selected=0 WHERE job_id=?`, jobID); err != nil {
		return err
	}
	for index := range unique {
		if _, err = tx.ExecContext(ctx, `UPDATE download_files SET selected=1 WHERE job_id=? AND file_index=?`, jobID, index); err != nil {
			return err
		}
	}
	if _, err = tx.ExecContext(ctx, `UPDATE download_jobs SET selected_size=?,completed_size=0,download_speed=0,imported_size=0,import_speed=0,current_file='',error='',status='queued',updated_at=? WHERE id=?`, total, time.Now().UTC().Format(time.RFC3339Nano), jobID); err != nil {
		return err
	}
	if err = tx.Commit(); err != nil {
		return err
	}
	return m.startRuntime(runtime)
}

func (m *downloadManager) startRuntime(runtime *downloadRuntime) error {
	runtime.mu.Lock()
	if runtime.starting || runtime.importing {
		runtime.mu.Unlock()
		return nil
	}
	runtime.starting = true
	runtime.lastRead = 0
	runtime.lastTick = time.Now()
	runtime.mu.Unlock()
	if err := m.applySelectedPriorities(runtime, true); err != nil {
		runtime.mu.Lock()
		runtime.starting = false
		runtime.mu.Unlock()
		return err
	}
	m.setStatus(runtime.jobID, "downloading", "")
	if !m.runBackground(func() { m.monitor(runtime) }) {
		runtime.mu.Lock()
		runtime.starting = false
		runtime.mu.Unlock()
		return context.Canceled
	}
	return nil
}

func (m *downloadManager) applySelectedPriorities(runtime *downloadRuntime, allow bool) error {
	rows, err := m.server.db.QueryContext(runtime.ctx, `SELECT file_index,selected FROM download_files WHERE job_id=? ORDER BY file_index`, runtime.jobID)
	if err != nil {
		return err
	}
	defer rows.Close()
	selected := []int{}
	for rows.Next() {
		var index int
		var yes bool
		if err := rows.Scan(&index, &yes); err != nil {
			return err
		}
		if yes {
			selected = append(selected, index)
		}
	}
	if err := rows.Err(); err != nil {
		return err
	}
	if allow {
		if err := m.bt.SelectTorrentFiles(runtime.ctx, runtime.torrentID, selected); err != nil {
			return err
		}
		return m.bt.StartTorrent(runtime.ctx, runtime.torrentID)
	} else {
		return m.bt.PauseTorrent(runtime.ctx, runtime.torrentID)
	}
}

func (m *downloadManager) monitor(runtime *downloadRuntime) {
	ticker := time.NewTicker(time.Second)
	defer ticker.Stop()
	defer func() { runtime.mu.Lock(); runtime.starting = false; runtime.mu.Unlock() }()
	for {
		select {
		case <-runtime.ctx.Done():
			return
		case <-ticker.C:
			job, err := m.get(runtime.ctx, runtime.jobID, true)
			if err != nil {
				return
			}
			if job.Status == "paused" {
				continue
			}
			if job.Status != "downloading" {
				return
			}
			stats, err := m.bt.TorrentStats(runtime.ctx, runtime.torrentID)
			if err != nil {
				m.fail(runtime.jobID, fmt.Errorf("读取下载进度: %w", err))
				return
			}
			completed := min(stats.ProgressBytes, job.SelectedSize)
			_, _ = m.server.db.ExecContext(runtime.ctx, `UPDATE download_jobs SET completed_size=?,download_speed=?,peers=?,updated_at=? WHERE id=? AND status='downloading'`, completed, max(stats.DownloadSpeed, 0), max(stats.Peers, 0), time.Now().UTC().Format(time.RFC3339Nano), runtime.jobID)
			m.server.jobs.Changed()
			if stats.Finished && job.SelectedSize > 0 {
				_ = m.bt.PauseTorrent(runtime.ctx, runtime.torrentID)
				_, _ = m.server.db.ExecContext(runtime.ctx, `UPDATE download_jobs SET status='importing',download_speed=0,peers=0,imported_size=0,import_speed=0,current_file='',error='',updated_at=? WHERE id=?`, time.Now().UTC().Format(time.RFC3339Nano), runtime.jobID)
				if !m.runBackground(func() { m.importRuntime(runtime) }) {
					return
				}
				return
			}
		}
	}
}

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
			objects, err := m.server.storage.ListPrefix(ctx, file.WebPrefix+"/")
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
		err := m.server.storage.DeleteObjects(ctx, batch)
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

func (m *downloadManager) list(ctx context.Context) ([]downloadJob, error) {
	rows, err := m.server.db.QueryContext(ctx, `SELECT id,parent_id,source_type,COALESCE(info_hash,''),name,status,ingest_state,selected_size,completed_size,download_speed,imported_size,import_speed,current_file,peers,error,created_at,updated_at FROM download_jobs ORDER BY created_at DESC`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	items := []downloadJob{}
	for rows.Next() {
		var job downloadJob
		if err := rows.Scan(&job.ID, &job.ParentID, &job.SourceType, &job.InfoHash, &job.Name, &job.Status, &job.IngestState, &job.SelectedSize, &job.CompletedSize, &job.DownloadSpeed, &job.ImportedSize, &job.ImportSpeed, &job.CurrentFile, &job.Peers, &job.Error, &job.CreatedAt, &job.UpdatedAt); err != nil {
			return nil, err
		}
		items = append(items, job)
	}
	return items, rows.Err()
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

func (m *downloadManager) cleanupLoop() {
	ticker := time.NewTicker(time.Hour)
	defer ticker.Stop()
	for {
		select {
		case <-m.ctx.Done():
			return
		case <-ticker.C:
			cutoff := time.Now().UTC().Add(-m.server.cfg.BTStaleAfter).Format(time.RFC3339Nano)
			rows, err := m.server.db.Query(`SELECT id FROM download_jobs WHERE status IN ('failed','cancelled') AND updated_at<?`, cutoff)
			if err != nil {
				continue
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
				cleanupCtx, cancel := context.WithTimeout(m.ctx, time.Minute)
				_ = m.remove(cleanupCtx, id)
				cancel()
			}
		}
	}
}

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
func (s *Server) listDownloads(w http.ResponseWriter, r *http.Request) {
	if s.downloads == nil {
		writeJSON(w, http.StatusOK, map[string]any{"items": []downloadJob{}})
		return
	}
	items, err := s.downloads.listAll(r.Context())
	if err != nil {
		problem(w, 500, "无法读取离线下载任务")
		return
	}
	writeJSON(w, 200, map[string]any{"items": items})
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
func (s *Server) deleteDownload(w http.ResponseWriter, r *http.Request) {
	if s.downloads == nil {
		problem(w, 503, "内置离线下载不可用")
		return
	}
	if err := s.downloads.removeAny(r.Context(), chi.URLParam(r, "id")); errors.Is(err, sql.ErrNoRows) {
		problem(w, 404, "离线下载任务不存在")
		return
	} else if err != nil {
		problem(w, 500, "无法删除离线下载任务")
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
