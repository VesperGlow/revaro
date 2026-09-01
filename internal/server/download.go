package server

import (
	"context"
	"encoding/base64"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/netip"
	"path"
	"strings"
	"sync"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
	"github.com/VesperGlow/revaro/internal/storage"
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
	engine, ok := s.objects.Torrent()
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
	s.cleanup.Register("bt-staging", time.Hour, 10*time.Minute, false, m.cleanupPass)
	s.cleanup.Register("url-download-jobs", time.Hour, time.Minute, false, m.cleanupURLPass)
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
