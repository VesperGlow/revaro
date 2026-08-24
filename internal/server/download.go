package server

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/base64"
	"errors"
	"fmt"
	"io"
	"mime"
	"net"
	"net/http"
	"path"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/VesperGlow/revaro/internal/btstore"
	"github.com/VesperGlow/revaro/internal/ids"
	"github.com/anacrolix/torrent"
	"github.com/anacrolix/torrent/iplist"
	"github.com/anacrolix/torrent/metainfo"
	"github.com/go-chi/chi/v5"
)

const maxTorrentMetadataBytes = 4 << 20

type downloadManager struct {
	server *Server
	client *torrent.Client
	pieces *btstore.Client
	http   *http.Client
	ctx    context.Context
	cancel context.CancelFunc

	mu       sync.RWMutex
	jobs     map[string]*downloadRuntime
	urlMu    sync.Mutex
	urlJobs  map[string]*urlDownloadRuntime
	urlSlots chan struct{}
}

type downloadRuntime struct {
	mu        sync.Mutex
	jobID     string
	torrent   *torrent.Torrent
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
	pieceStore, err := btstore.New(s.db, s.storage, filepath.Join(s.cfg.DataDir, "torrent-cache"), s.log)
	if err != nil {
		return nil, err
	}
	cfg := torrent.NewDefaultClientConfig()
	cfg.DefaultStorage = pieceStore
	cfg.DataDir = filepath.Join(s.cfg.DataDir, "torrent-cache")
	cfg.ListenPort = s.cfg.BTListenPort
	cfg.NoDefaultPortForwarding = true
	cfg.Seed = false
	cfg.MaxUnverifiedBytes = 128 << 20
	cfg.EstablishedConnsPerTorrent = 40
	cfg.HalfOpenConnsPerTorrent = 16
	cfg.TotalHalfOpenConns = 48
	cfg.Slogger = s.log.With("component", "bittorrent")
	cfg.IPBlocklist = privateIPBlocklist()
	cfg.HTTPDialContext = publicDialContext
	cfg.TrackerDialContext = publicDialContext
	client, err := torrent.NewClient(cfg)
	if err != nil {
		return nil, err
	}
	ctx, cancel := context.WithCancel(context.Background())
	m := &downloadManager{
		server: s, client: client, pieces: pieceStore, http: newURLDownloadClient(),
		ctx: ctx, cancel: cancel, jobs: make(map[string]*downloadRuntime),
		urlJobs: make(map[string]*urlDownloadRuntime), urlSlots: make(chan struct{}, 2),
	}
	go m.restore()
	go m.restoreURLDownloads()
	go m.cleanupCompletedPieces()
	go m.cleanupLoop()
	go m.cleanupURLLoop()
	return m, nil
}

func (m *downloadManager) Close() {
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
	if m.client != nil {
		m.client.Close()
	}
	if m.pieces != nil {
		_ = m.pieces.Close()
	}
}

type privateAddressBlocklist struct{}

func (privateAddressBlocklist) NumRanges() int { return 1 }

func (privateAddressBlocklist) Lookup(ip net.IP) (iplist.Range, bool) {
	blocked := ip == nil || ip.IsLoopback() || ip.IsPrivate() || ip.IsLinkLocalUnicast() ||
		ip.IsLinkLocalMulticast() || ip.IsUnspecified() || ip.IsMulticast()
	if v4 := ip.To4(); v4 != nil {
		// CGNAT and benchmarking ranges are not covered by net.IP.IsPrivate.
		blocked = blocked || (v4[0] == 100 && v4[1]&0xc0 == 0x40) ||
			(v4[0] == 198 && (v4[1] == 18 || v4[1] == 19)) || v4[0] >= 224
	}
	if !blocked {
		return iplist.Range{}, false
	}
	return iplist.Range{First: ip, Last: ip, Description: "non-public address"}, true
}

func privateIPBlocklist() iplist.Ranger { return privateAddressBlocklist{} }

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
		if ip == nil || ip.IsLoopback() || ip.IsPrivate() || ip.IsLinkLocalUnicast() || ip.IsLinkLocalMulticast() || ip.IsUnspecified() || ip.IsMulticast() {
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

func (m *downloadManager) restore() {
	rows, err := m.server.db.QueryContext(m.ctx, `SELECT id,source_type,source,metainfo,status FROM download_jobs WHERE status IN ('metadata','waiting','queued','downloading','paused','importing') ORDER BY created_at`)
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
	var t *torrent.Torrent
	var err error
	if len(encodedMeta) > 0 {
		mi, loadErr := metainfo.Load(bytes.NewReader(encodedMeta))
		if loadErr != nil {
			return loadErr
		}
		t, err = m.client.AddTorrent(mi)
	} else if sourceType == "magnet" {
		t, err = m.client.AddMagnet(source)
	} else {
		decoded, decodeErr := base64.StdEncoding.DecodeString(source)
		if decodeErr != nil {
			return decodeErr
		}
		mi, loadErr := metainfo.Load(bytes.NewReader(decoded))
		if loadErr != nil {
			return loadErr
		}
		t, err = m.client.AddTorrent(mi)
	}
	if err != nil {
		return err
	}
	t.DisallowDataDownload()
	ctx, cancel := context.WithCancel(m.ctx)
	runtime := &downloadRuntime{jobID: jobID, torrent: t, ctx: ctx, cancel: cancel, lastTick: time.Now()}
	m.mu.Lock()
	m.jobs[jobID] = runtime
	m.mu.Unlock()
	go m.awaitMetadata(ctx, runtime, previousStatus)
	return nil
}

func (m *downloadManager) awaitMetadata(ctx context.Context, runtime *downloadRuntime, previousStatus string) {
	timer := time.NewTimer(m.server.cfg.BTMetadataWait)
	defer timer.Stop()
	select {
	case <-runtime.torrent.GotInfo():
	case <-timer.C:
		m.fail(runtime.jobID, errors.New("获取种子元数据超时"))
		return
	case <-ctx.Done():
		return
	}
	if err := m.persistMetadata(runtime.jobID, runtime.torrent); err != nil {
		m.fail(runtime.jobID, err)
		return
	}
	switch previousStatus {
	case "queued", "downloading":
		if err := m.startRuntime(runtime); err != nil {
			m.fail(runtime.jobID, err)
		}
	case "paused":
		m.applySelectedPriorities(runtime, false)
	case "importing":
		go m.importRuntime(runtime)
	default:
		m.setStatus(runtime.jobID, "waiting", "")
	}
}

func (m *downloadManager) persistMetadata(jobID string, t *torrent.Torrent) error {
	info := t.Info()
	if info == nil {
		return errors.New("torrent metadata is unavailable")
	}
	if !info.HasV1() {
		return errors.New("目前只支持 BitTorrent v1 或包含 v1 的混合种子")
	}
	files := t.Files()
	if len(files) == 0 || len(files) > m.server.cfg.BTMaxFiles {
		return fmt.Errorf("种子文件数必须在 1 到 %d 之间", m.server.cfg.BTMaxFiles)
	}
	var total int64
	validated := make([]downloadFile, 0, len(files))
	for index, file := range files {
		if file.Length() < 0 || total > m.server.cfg.BTMaxTotalSize-file.Length() {
			return fmt.Errorf("种子总大小超过 %d 字节限制", m.server.cfg.BTMaxTotalSize)
		}
		total += file.Length()
		rel, err := safeTorrentPath(file.DisplayPath())
		if err != nil {
			return fmt.Errorf("种子包含不安全的文件路径: %w", err)
		}
		validated = append(validated, downloadFile{Index: index, Path: rel, Size: file.Length()})
	}
	if len(files) > 1 {
		if err := validateName(strings.TrimSpace(t.Name())); err != nil {
			return fmt.Errorf("种子根目录名称无效: %w", err)
		}
	}
	var meta bytes.Buffer
	mi := t.Metainfo()
	if err := mi.Write(&meta); err != nil {
		return err
	}
	if meta.Len() > maxTorrentMetadataBytes {
		return errors.New("种子元数据超过 4 MiB")
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	tx, err := m.server.db.BeginTx(m.ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if _, err = tx.ExecContext(m.ctx, `UPDATE download_jobs SET info_hash=?,name=?,metainfo=?,source='',updated_at=? WHERE id=?`, t.InfoHash().HexString(), strings.TrimSpace(t.Name()), meta.Bytes(), now, jobID); err != nil {
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
		spec, err := torrent.TorrentSpecFromMagnetUri(source)
		if err != nil {
			return downloadJob{}, errors.New("磁力链接无效")
		}
		if spec.InfoHash == (metainfo.Hash{}) {
			return downloadJob{}, errors.New("目前只支持包含 btih 的 BitTorrent v1 或混合磁力链接")
		}
		knownHash = spec.InfoHash.HexString()
	} else {
		sourceType, source = "torrent", torrentBase64
		decoded, err := base64.StdEncoding.DecodeString(source)
		if err != nil || len(decoded) == 0 || len(decoded) > maxTorrentMetadataBytes {
			return downloadJob{}, errors.New(".torrent 文件无效或超过 4 MiB")
		}
		mi, err := metainfo.Load(bytes.NewReader(decoded))
		if err != nil {
			return downloadJob{}, errors.New(".torrent 文件无法解析")
		}
		info, err := mi.UnmarshalInfo()
		if err != nil || !info.HasV1() {
			return downloadJob{}, errors.New("目前只支持 BitTorrent v1 或包含 v1 的混合种子")
		}
		knownHash = mi.HashInfoBytes().HexString()
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
	stats := runtime.torrent.Stats()
	runtime.lastRead = stats.BytesReadUsefulData.Int64()
	runtime.lastTick = time.Now()
	runtime.mu.Unlock()
	if err := m.applySelectedPriorities(runtime, true); err != nil {
		runtime.mu.Lock()
		runtime.starting = false
		runtime.mu.Unlock()
		return err
	}
	m.setStatus(runtime.jobID, "downloading", "")
	go m.monitor(runtime)
	return nil
}

func (m *downloadManager) applySelectedPriorities(runtime *downloadRuntime, allow bool) error {
	rows, err := m.server.db.QueryContext(runtime.ctx, `SELECT file_index,selected FROM download_files WHERE job_id=? ORDER BY file_index`, runtime.jobID)
	if err != nil {
		return err
	}
	defer rows.Close()
	selected := make(map[int]bool)
	for rows.Next() {
		var index int
		var yes bool
		if err := rows.Scan(&index, &yes); err != nil {
			return err
		}
		if yes {
			selected[index] = true
		}
	}
	if err := rows.Err(); err != nil {
		return err
	}
	files := runtime.torrent.Files()
	for index, file := range files {
		if allow && selected[index] {
			file.SetPriority(torrent.PiecePriorityNormal)
		} else {
			file.SetPriority(torrent.PiecePriorityNone)
		}
	}
	if allow {
		runtime.torrent.AllowDataDownload()
		runtime.torrent.AllowDataUpload()
	} else {
		runtime.torrent.DisallowDataDownload()
		runtime.torrent.DisallowDataUpload()
	}
	return nil
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
			files := runtime.torrent.Files()
			var completed int64
			allComplete := true
			for _, item := range job.Files {
				if !item.Selected || item.Index < 0 || item.Index >= len(files) {
					continue
				}
				got := min(files[item.Index].BytesCompleted(), item.Size)
				completed += got
				if got < item.Size {
					allComplete = false
				}
			}
			stats := runtime.torrent.Stats()
			now := time.Now()
			read := stats.BytesReadUsefulData.Int64()
			runtime.mu.Lock()
			deltaSeconds := now.Sub(runtime.lastTick).Seconds()
			speed := int64(0)
			if deltaSeconds > 0 && read >= runtime.lastRead {
				speed = int64(float64(read-runtime.lastRead) / deltaSeconds)
			}
			runtime.lastRead, runtime.lastTick = read, now
			runtime.mu.Unlock()
			_, _ = m.server.db.ExecContext(runtime.ctx, `UPDATE download_jobs SET completed_size=?,download_speed=?,peers=?,updated_at=? WHERE id=? AND status='downloading'`, completed, max(speed, 0), stats.ActivePeers, time.Now().UTC().Format(time.RFC3339Nano), runtime.jobID)
			if allComplete && job.SelectedSize > 0 {
				runtime.torrent.DisallowDataDownload()
				runtime.torrent.DisallowDataUpload()
				_, _ = m.server.db.ExecContext(runtime.ctx, `UPDATE download_jobs SET status='importing',download_speed=0,peers=0,imported_size=0,import_speed=0,current_file='',error='',updated_at=? WHERE id=?`, time.Now().UTC().Format(time.RFC3339Nano), runtime.jobID)
				go m.importRuntime(runtime)
				return
			}
		}
	}
}

type importedDownloadFile struct {
	path, objectKey, mimeType, etag string
	size                            int64
}

type downloadImportProgressReader struct {
	reader     io.Reader
	read       int64
	onProgress func(int64)
}

func (r *downloadImportProgressReader) Read(p []byte) (int, error) {
	n, err := r.reader.Read(p)
	if n > 0 {
		r.read += int64(n)
		if r.onProgress != nil {
			r.onProgress(r.read)
		}
	}
	return n, err
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
	files := runtime.torrent.Files()
	stored := make([]importedDownloadFile, 0)
	var imported int64
	lastBytes := int64(0)
	lastUpdate := time.Now()
	persistProgress := func(total int64, currentFile string, force bool) {
		now := time.Now()
		elapsed := now.Sub(lastUpdate)
		if !force && elapsed < 500*time.Millisecond && total-lastBytes < 4<<20 {
			return
		}
		speed := int64(0)
		if elapsed > 0 && total >= lastBytes {
			speed = int64(float64(total-lastBytes) / elapsed.Seconds())
		}
		_, _ = m.server.db.ExecContext(runtime.ctx, `UPDATE download_jobs SET imported_size=?,import_speed=?,current_file=?,updated_at=? WHERE id=? AND status='importing'`, total, max(speed, 0), currentFile, now.UTC().Format(time.RFC3339Nano), job.ID)
		lastBytes, lastUpdate = total, now
	}
	for _, item := range job.Files {
		if !item.Selected {
			continue
		}
		if item.Index < 0 || item.Index >= len(files) {
			m.fail(runtime.jobID, errors.New("种子文件索引发生变化"))
			return
		}
		reader := files[item.Index].NewReader()
		reader.SetContext(runtime.ctx)
		persistProgress(imported, item.Path, true)
		progressReader := &downloadImportProgressReader{reader: io.LimitReader(reader, item.Size)}
		progressReader.onProgress = func(fileBytes int64) { persistProgress(imported+fileBytes, item.Path, false) }
		key, manifest, storeErr := m.server.storage.Store(runtime.ctx, progressReader)
		reader.Close()
		if storeErr != nil || manifest.Size != item.Size {
			if storeErr == nil {
				storeErr = fmt.Errorf("导入大小 %d 与预期 %d 不一致", manifest.Size, item.Size)
			}
			m.fail(runtime.jobID, fmt.Errorf("导入 %s: %w", item.Path, storeErr))
			return
		}
		imported += manifest.Size
		persistProgress(imported, item.Path, true)
		fileName := path.Base(item.Path)
		mimeType := mime.TypeByExtension(strings.ToLower(filepath.Ext(fileName)))
		if mimeType == "" {
			mimeType = "application/octet-stream"
		}
		stored = append(stored, importedDownloadFile{path: item.Path, objectKey: key, size: manifest.Size, mimeType: mimeType, etag: manifest.ID()})
	}
	if err := m.commitImported(runtime.ctx, job, stored, len(files) > 1); err != nil {
		m.fail(runtime.jobID, err)
		return
	}
	if job.InfoHash != "" {
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 30*time.Minute)
		if err := m.pieces.DeleteTorrent(cleanupCtx, job.InfoHash); err != nil {
			m.server.log.Warn("completed torrent temporary pieces cleanup failed", "job", job.ID, "error", err)
		}
		cancel()
	}
	runtime.cancel()
	runtime.torrent.Drop()
	m.mu.Lock()
	delete(m.jobs, runtime.jobID)
	m.mu.Unlock()
	m.server.log.Info("built-in torrent download imported", "job", job.ID, "name", job.Name, "files", len(stored), "size", job.SelectedSize)
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
		if _, err := tx.Exec(`INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at) VALUES(?,?,?,'file',?,?,?,?,'ready',?,?)`, ids.New(), currentParent, name, file.objectKey, file.size, file.mimeType, file.etag, now, now); err != nil {
			if isConflict(err) {
				return errors.New("目标目录中存在同名文件")
			}
			return err
		}
	}
	if _, err := tx.ExecContext(ctx, `UPDATE download_jobs SET status='done',completed_size=selected_size,download_speed=0,imported_size=selected_size,import_speed=0,current_file='',peers=0,error='',updated_at=? WHERE id=?`, now, job.ID); err != nil {
		return err
	}
	return tx.Commit()
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
	runtime.torrent.DisallowDataDownload()
	runtime.torrent.DisallowDataUpload()
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
	if err != nil || job.Status != "paused" {
		return errors.New("任务当前不能继续")
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
	job, err := m.get(ctx, jobID, false)
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
		runtime.torrent.DisallowDataDownload()
		runtime.torrent.DisallowDataUpload()
		runtime.torrent.Drop()
	}
	if job.InfoHash != "" && !m.piecesUsedByOtherJob(ctx, job.ID, job.InfoHash) {
		if err := m.pieces.DeleteTorrent(ctx, job.InfoHash); err != nil {
			return err
		}
	}
	if _, err := m.server.db.ExecContext(ctx, `DELETE FROM download_jobs WHERE id=?`, jobID); err != nil {
		return err
	}
	return nil
}

func (m *downloadManager) piecesUsedByOtherJob(ctx context.Context, jobID, infoHash string) bool {
	var count int
	err := m.server.db.QueryRowContext(ctx, `SELECT COUNT(*) FROM download_jobs WHERE id<>? AND info_hash=? AND status IN ('metadata','waiting','queued','downloading','paused','importing')`, jobID, infoHash).Scan(&count)
	return err != nil || count > 0
}

func (m *downloadManager) list(ctx context.Context) ([]downloadJob, error) {
	rows, err := m.server.db.QueryContext(ctx, `SELECT id,parent_id,source_type,COALESCE(info_hash,''),name,status,selected_size,completed_size,download_speed,imported_size,import_speed,current_file,peers,error,created_at,updated_at FROM download_jobs ORDER BY created_at DESC`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	items := []downloadJob{}
	for rows.Next() {
		var job downloadJob
		if err := rows.Scan(&job.ID, &job.ParentID, &job.SourceType, &job.InfoHash, &job.Name, &job.Status, &job.SelectedSize, &job.CompletedSize, &job.DownloadSpeed, &job.ImportedSize, &job.ImportSpeed, &job.CurrentFile, &job.Peers, &job.Error, &job.CreatedAt, &job.UpdatedAt); err != nil {
			return nil, err
		}
		items = append(items, job)
	}
	return items, rows.Err()
}

func (m *downloadManager) get(ctx context.Context, jobID string, withFiles bool) (downloadJob, error) {
	var job downloadJob
	err := m.server.db.QueryRowContext(ctx, `SELECT id,parent_id,source_type,COALESCE(info_hash,''),name,status,selected_size,completed_size,download_speed,imported_size,import_speed,current_file,peers,error,created_at,updated_at FROM download_jobs WHERE id=?`, jobID).Scan(&job.ID, &job.ParentID, &job.SourceType, &job.InfoHash, &job.Name, &job.Status, &job.SelectedSize, &job.CompletedSize, &job.DownloadSpeed, &job.ImportedSize, &job.ImportSpeed, &job.CurrentFile, &job.Peers, &job.Error, &job.CreatedAt, &job.UpdatedAt)
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
}

func (m *downloadManager) fail(jobID string, err error) {
	m.setStatus(jobID, "failed", err.Error())
	m.mu.Lock()
	runtime := m.jobs[jobID]
	delete(m.jobs, jobID)
	m.mu.Unlock()
	if runtime != nil {
		runtime.torrent.DisallowDataDownload()
		runtime.torrent.DisallowDataUpload()
		runtime.cancel()
		runtime.torrent.Drop()
	}
	m.server.log.Error("built-in torrent task failed", "job", jobID, "error", err)
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
				_ = m.remove(context.Background(), id)
			}
		}
	}
}

func (m *downloadManager) cleanupCompletedPieces() {
	rows, err := m.server.db.QueryContext(m.ctx, `
		SELECT DISTINCT pieces.info_hash
		FROM download_pieces AS pieces
		WHERE EXISTS (
			SELECT 1 FROM download_jobs AS completed
			WHERE completed.info_hash=pieces.info_hash AND completed.status='done'
		) AND NOT EXISTS (
			SELECT 1 FROM download_jobs AS active
			WHERE active.info_hash=pieces.info_hash
			  AND active.status IN ('metadata','waiting','queued','downloading','paused','importing')
		)`)
	if err != nil {
		m.server.log.Warn("completed torrent piece cleanup scan failed", "error", err)
		return
	}
	var hashes []string
	for rows.Next() {
		var hash string
		if rows.Scan(&hash) == nil {
			hashes = append(hashes, hash)
		}
	}
	rows.Close()
	for _, hash := range hashes {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Minute)
		if err := m.pieces.DeleteTorrent(ctx, hash); err != nil {
			m.server.log.Warn("completed torrent piece cleanup failed", "info_hash", hash, "error", err)
		}
		cancel()
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
	if err := s.downloads.resumeAny(r.Context(), chi.URLParam(r, "id")); err != nil {
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
