package server

import (
	"context"
	"database/sql"
	"log/slog"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/VesperGlow/revaro/internal/auth"
	"github.com/VesperGlow/revaro/internal/config"
	"github.com/VesperGlow/revaro/internal/storage"
	"github.com/VesperGlow/revaro/internal/webui"
	"github.com/go-chi/chi/v5"
	"github.com/go-chi/chi/v5/middleware"
	"golang.org/x/sync/singleflight"
)

const RootID = "00000000-0000-0000-0000-000000000000"
const maxJSONBody = 7 << 20
const maxDocumentBytes = 1 << 20
const maxAvatarBytes = 2 << 20
const avatarObjectKey = "profile/avatar"
const maxBlocksPerRequest = 1000
const maxCompleteBody = 32 << 20
const maxLogicalFileSize = 1 << 40 // 1 TiB

type Server struct {
	db                 *sql.DB
	storage            storage.Storage
	auth               *auth.Service
	cfg                config.Config
	log                *slog.Logger
	limiter            *loginLimiter
	s3Origin           string // S3_PUBLIC_ENDPOINT 的 scheme://host，用于收窄 CSP
	shareSlots         chan struct{}
	audioMergeSlots    chan struct{}
	audioMergeMu       sync.RWMutex
	audioMergeJobs     map[string]*audioMergeJob
	localMergeJobSlots chan struct{}
	localMergeUploads  chan struct{}
	diskFree           func(string) (int64, error) // override for tests; nil uses statfs
	audioHLSSlots      chan struct{}
	audioHLSMu         sync.RWMutex
	audioHLSSessions   map[string]*audioHLSSession
	audioHLSCtx        context.Context
	audioHLSCancel     context.CancelFunc
	videoHLSSlots      chan struct{}
	videoHLSMu         sync.RWMutex
	videoHLSSessions   map[string]*videoHLSSession
	videoFMP4Slots     chan struct{}
	videoFMP4Mu        sync.RWMutex
	videoFMP4Sessions  map[string]*videoFMP4Session
	mediaAnalysis      *mediaAnalysisScheduler
	thumbnails         *thumbnailScheduler
	audioThumbSlots    chan struct{}
	audioThumbGroup    singleflight.Group
	generateAudioCover func(context.Context, File) ([]byte, error)
	mediaProbeGroup    singleflight.Group
	probeMediaSource   func(context.Context, File) (storage.MediaProbe, error)
	archiveSlots       chan struct{}
	archiveMu          sync.RWMutex
	archiveJobs        map[string]*archiveJob
	flowBuilds         singleflight.Group
	downloads          *downloadManager
	jobs               *JobManager
	tasks              *TaskManager
	objects            *ObjectManager
	cache              *CacheManager
	cleanup            *CleanupManager
	media              *MediaPipeline
	lifecycleMu        sync.Mutex
	lifecycleClosing   bool
	lifecycleWG        sync.WaitGroup
	statusMu           sync.RWMutex
	statusSnapshot     systemStatusResponse
	statusSubscribers  map[chan systemStatusResponse]struct{}
	statusStop         chan struct{}
	backupCancel       context.CancelFunc
}

type File struct {
	ID              string  `json:"id"`
	ParentID        *string `json:"parent_id"`
	Name            string  `json:"name"`
	Kind            string  `json:"kind"`
	Size            int64   `json:"size"`
	MimeType        string  `json:"mime_type,omitempty"`
	ETag            string  `json:"etag,omitempty"`
	ContentHash     string  `json:"content_hash,omitempty"`
	HashAlgorithm   string  `json:"hash_algorithm,omitempty"`
	Status          string  `json:"status"`
	CreatedAt       string  `json:"created_at"`
	UpdatedAt       string  `json:"updated_at"`
	DeletedAt       string  `json:"deleted_at,omitempty"`
	RestoreParentID *string `json:"restore_parent_id,omitempty"`
	HasCover        bool    `json:"has_cover,omitempty"`
	objectKey       string
}

func New(db *sql.DB, store storage.Storage, a *auth.Service, cfg config.Config, logger *slog.Logger) *Server {
	if logger == nil {
		logger = slog.Default()
	}
	s3Origin := ""
	if u, err := url.Parse(cfg.S3PublicEndpoint); err == nil && u.Host != "" {
		s3Origin = u.Scheme + "://" + u.Host
	}
	hlsCtx, hlsCancel := context.WithCancel(context.Background())
	resources := newResourceGovernor()
	s := &Server{
		db: db, storage: store, auth: a, cfg: cfg, log: logger,
		jobs:    NewJobManager(),
		limiter: newLoginLimiter(), s3Origin: s3Origin,
		shareSlots:      make(chan struct{}, 8),
		audioMergeSlots: make(chan struct{}, 2), audioMergeJobs: make(map[string]*audioMergeJob),
		localMergeJobSlots: make(chan struct{}, maxLocalMergeUploadingJobs),
		localMergeUploads:  make(chan struct{}, localMergeUploadConcurrency),
		audioHLSSlots:      make(chan struct{}, 2), audioHLSSessions: make(map[string]*audioHLSSession),
		videoHLSSlots: make(chan struct{}, 1), videoHLSSessions: make(map[string]*videoHLSSession),
		videoFMP4Slots: make(chan struct{}, 2), videoFMP4Sessions: make(map[string]*videoFMP4Session),
		mediaAnalysis:   newMediaAnalysisScheduler(2),
		thumbnails:      newThumbnailScheduler(1),
		audioThumbSlots: make(chan struct{}, 1),
		archiveSlots:    make(chan struct{}, 1), archiveJobs: make(map[string]*archiveJob),
		audioHLSCtx: hlsCtx, audioHLSCancel: hlsCancel,
		statusSubscribers: make(map[chan systemStatusResponse]struct{}), statusStop: make(chan struct{}),
	}
	s.objects = newObjectManager(store)
	s.objects.server = s
	s.cache = newCacheManager(filepath.Join(cfg.WorkDir, "cache"), maxVideoSubtitleCacheBytes, cfg.MediaCacheCapacity)
	s.cache.RegisterStats("hls", s.mediaCacheStats)
	s.cache.RegisterPruner("hls", s.pruneMediaCache)
	s.tasks = newTaskManager(db, s.jobs, resources)
	s.media = newMediaPipeline(store, resources)
	s.cleanup = newCleanupManager(logger, resources)
	s.probeMediaSource = func(ctx context.Context, file File) (storage.MediaProbe, error) {
		return s.media.Probe(ctx, file.objectKey)
	}
	s.generateAudioCover = func(ctx context.Context, file File) ([]byte, error) {
		return s.media.AudioCover(ctx, file.objectKey, thumbMaxDim)
	}
	s.RecoverTasks(context.Background())
	if cfg.BTEnabled {
		manager, err := newDownloadManager(s)
		if err != nil {
			logger.Error("built-in torrent engine unavailable", "error", err)
		} else {
			s.downloads = manager
		}
	}
	// Remove only legacy pending audio placeholders that are not owned by a
	// durable task. Restorable merges retain their output row through task_files.
	if result, err := db.Exec(`DELETE FROM files WHERE kind='file' AND status='pending' AND mime_type IN ('audio/mp4','audio/flac') AND object_key IS NULL AND NOT EXISTS (SELECT 1 FROM uploads WHERE uploads.file_id=files.id) AND NOT EXISTS (SELECT 1 FROM task_files WHERE task_files.file_id=files.id)`); err != nil {
		logger.Error("interrupted audio merge cleanup failed", "error", err)
	} else if removed, _ := result.RowsAffected(); removed > 0 {
		logger.Info("interrupted audio merges cleaned", "files", removed)
	}
	// Only Revaro-owned, recognizable workspaces are eligible for startup
	// cleanup. Unknown APP_WORK_DIR contents are never touched.
	_ = os.MkdirAll(cfg.WorkDir, 0o700)
	for _, pattern := range []string{"revaro-audio-merge-*", "revaro-audio-hls-*", "revaro-video-hls-*", "revaro-extract-*", backupStagingPattern} {
		stale, err := filepath.Glob(filepath.Join(cfg.WorkDir, pattern))
		if err != nil {
			logger.Warn("stale workspace scan failed", "pattern", pattern, "error", err)
			continue
		}
		for _, dir := range stale {
			if err := os.RemoveAll(dir); err != nil {
				logger.Warn("stale workspace cleanup failed", "path", dir, "error", err)
			}
		}
	}
	s.cleanupUnreferencedLocalMergeDirs()
	s.restorePersistentTasks()
	s.cleanup.Register("audio-hls", time.Minute, time.Minute, false, func(context.Context) error { s.cleanupAudioHLSSessions(); return nil })
	s.cleanup.Register("video-hls", time.Minute, time.Minute, false, func(context.Context) error { s.cleanupVideoHLSSessions(); return nil })
	s.cleanup.Register("video-fmp4", time.Minute, time.Minute, false, func(context.Context) error { s.cleanupVideoFMP4Sessions(); return nil })
	s.cleanup.Register("archive-password", time.Minute, time.Minute, false, func(context.Context) error { s.cleanupArchiveJobs(); return nil })
	s.cleanup.Register("cache", 5*time.Minute, time.Minute, false, func(context.Context) error { s.cache.Prune(); return nil })
	s.cleanup.Register("uploads", 15*time.Minute, 5*time.Minute, true, func(ctx context.Context) error { s.CleanupExpiredUploads(ctx); return nil })
	s.cleanup.Register("object-cleanup", 15*time.Minute, 5*time.Minute, true, func(ctx context.Context) error { s.CleanupObjects(ctx); return nil })
	s.cleanup.Register("local-merges", 15*time.Minute, 5*time.Minute, true, func(ctx context.Context) error { s.CleanupExpiredLocalMerges(ctx); return nil })
	s.cleanup.Register("trash", 15*time.Minute, 10*time.Minute, true, func(ctx context.Context) error {
		if s.CleanupExpiredTrash(ctx) > 0 {
			s.CollectGarbage(ctx)
		}
		return nil
	})
	if cfg.GCInterval > 0 {
		s.cleanup.Register("orphan-objects", cfg.GCInterval, 10*time.Minute, true, func(ctx context.Context) error { s.CollectGarbage(ctx); return nil })
	}
	s.cleanup.Start()
	s.startSystemStatusSnapshots()
	s.startDatabaseBackups()
	return s
}

func (s *Server) RegisterCleanup(name string, interval, timeout time.Duration, runNow bool, run func(context.Context) error) {
	s.cleanup.Register(name, interval, timeout, runNow, run)
}

// runBackground makes the Server the owner of every goroutine that may touch
// its database, storage client, or in-memory registries. Once shutdown starts,
// no new work is admitted and Close can safely wait before callers tear those
// dependencies down.
func (s *Server) runBackground(work func()) bool {
	s.lifecycleMu.Lock()
	defer s.lifecycleMu.Unlock()
	if s.lifecycleClosing {
		return false
	}
	s.lifecycleWG.Add(1)
	go func() {
		defer s.lifecycleWG.Done()
		work()
	}()
	return true
}

// Close cancels all Server-owned work, waits for it to stop touching shared
// dependencies, and then removes transient staging resources.
func (s *Server) Close() {
	s.lifecycleMu.Lock()
	if s.lifecycleClosing {
		s.lifecycleMu.Unlock()
		return
	}
	s.lifecycleClosing = true
	s.lifecycleMu.Unlock()
	if s.statusStop != nil {
		close(s.statusStop)
	}
	if s.backupCancel != nil {
		s.backupCancel()
	}
	if s.cleanup != nil {
		s.cleanup.Close()
	}
	if s.cache != nil {
		s.cache.Close()
	}
	if s.audioHLSCancel != nil {
		s.audioHLSCancel()
	}
	if s.jobs != nil {
		s.jobs.Close()
	}
	if s.downloads != nil {
		s.downloads.Close()
	}
	if s.mediaAnalysis != nil {
		s.mediaAnalysis.close()
	}
	if s.thumbnails != nil {
		s.thumbnails.close()
	}
	s.audioHLSMu.Lock()
	sessions := make([]*audioHLSSession, 0, len(s.audioHLSSessions))
	for _, session := range s.audioHLSSessions {
		sessions = append(sessions, session)
	}
	s.audioHLSSessions = make(map[string]*audioHLSSession)
	s.audioHLSMu.Unlock()
	for _, session := range sessions {
		session.stop()
	}
	s.videoHLSMu.Lock()
	videoSessions := make([]*videoHLSSession, 0, len(s.videoHLSSessions))
	for _, session := range s.videoHLSSessions {
		videoSessions = append(videoSessions, session)
	}
	s.videoHLSSessions = make(map[string]*videoHLSSession)
	s.videoHLSMu.Unlock()
	for _, session := range videoSessions {
		session.destroy()
	}
	s.videoFMP4Mu.Lock()
	fmp4Sessions := make([]*videoFMP4Session, 0, len(s.videoFMP4Sessions))
	for _, session := range s.videoFMP4Sessions {
		fmp4Sessions = append(fmp4Sessions, session)
	}
	s.videoFMP4Sessions = make(map[string]*videoFMP4Session)
	s.videoFMP4Mu.Unlock()
	for _, session := range fmp4Sessions {
		session.destroy()
	}
	s.lifecycleWG.Wait()
	s.archiveMu.RLock()
	archiveJobs := make([]*archiveJob, 0, len(s.archiveJobs))
	for _, job := range s.archiveJobs {
		archiveJobs = append(archiveJobs, job)
	}
	s.archiveMu.RUnlock()
	for _, job := range archiveJobs {
		s.cleanupArchiveJobStaging(job)
	}
}

func (s *Server) Handler() http.Handler {
	r := chi.NewRouter()
	r.Use(middleware.RequestID, middleware.Recoverer, s.securityHeaders, s.originGuard)
	r.Get("/healthz", func(w http.ResponseWriter, _ *http.Request) {
		writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
	})
	r.Get("/readyz", s.ready)
	r.Get("/s/{token}", s.publicShare)
	r.Route("/api", func(r chi.Router) {
		r.NotFound(func(w http.ResponseWriter, _ *http.Request) {
			problem(w, http.StatusNotFound, "api endpoint not found")
		})
		r.Post("/auth/login", s.login)
		r.Group(func(r chi.Router) {
			r.Use(s.requireAuth)
			r.Post("/auth/logout", s.logout)
			r.Get("/auth/me", s.me)
			r.Patch("/auth/credentials", s.changeCredentials)
			r.Patch("/auth/password", s.changePassword)
			r.Get("/auth/totp", s.totpStatus)
			r.Post("/auth/totp/setup", s.beginTOTPSetup)
			r.Post("/auth/totp/enable", s.enableTOTP)
			r.Post("/auth/totp/recovery-codes", s.regenerateTOTPRecoveryCodes)
			r.Delete("/auth/totp", s.disableTOTP)
			r.Get("/profile/avatar", s.getAvatar)
			r.Put("/profile/avatar", s.updateAvatar)
			r.Delete("/profile/avatar", s.deleteAvatar)
			r.Patch("/profile/username", s.changeUsername)
			r.Get("/storage/stats", s.storageStats)
			r.Get("/system/status", s.systemStatus)
			r.Get("/system/status/stream", s.systemStatusStream)
			r.Get("/events", s.jobEvents)
			r.Get("/tasks", s.listTasks)
			r.Get("/tasks/{id}", s.getTask)
			r.Post("/tasks/{id}/cancel", s.cancelTask)
			r.Post("/tasks/{id}/retry", s.retryTask)
			r.Post("/tasks/{id}/input", s.taskInput)
			r.Delete("/tasks/{id}", s.deleteTask)
			r.Get("/files/{id}", s.getFile)
			r.Get("/files/{id}/children", s.children)
			r.Get("/files/{id}/download", s.download)
			r.Get("/files/{id}/preview", s.preview)
			r.Get("/files/{id}/audio", s.audioMediaInfo)
			r.Get("/files/{id}/audio/stream", s.audioMediaStream)
			r.Post("/files/{id}/audio/hls", s.startAudioHLS)
			r.Get("/audio/hls/{session}/{asset}", s.audioHLSAsset)
			r.Delete("/audio/hls/{session}", s.stopAudioHLS)
			r.Get("/files/{id}/video", s.videoMediaInfo)
			r.Post("/files/{id}/media/reanalyze", s.reanalyzeMedia)
			r.Get("/files/{id}/video/subtitles/{subtitle}", s.videoSubtitle)
			r.Get("/files/{id}/video/fmp4", s.videoFMP4Metadata)
			r.Post("/files/{id}/video/fmp4", s.startVideoFMP4)
			r.Post("/files/{id}/video/hls", s.startVideoHLS)
			r.Get("/files/{id}/media/progress", s.mediaProgress)
			r.Put("/files/{id}/media/progress", s.saveMediaProgress)
			r.Get("/video/hls/{session}/{asset}", s.videoHLSAsset)
			r.Delete("/video/hls/{session}", s.stopVideoHLS)
			r.Get("/video/fmp4/{session}/stream", s.streamVideoFMP4)
			r.Delete("/video/fmp4/{session}", s.stopVideoFMP4)
			r.Get("/files/{id}/content", s.getDocument)
			r.Put("/files/{id}/content", s.updateDocument)
			r.Get("/files/{id}/book", s.bookInfo)
			r.Get("/files/{id}/book/assets/{index}", s.bookAsset)
			r.Get("/files/{id}/book/cover", s.bookCover)
			r.Get("/files/{id}/book/progress", s.bookProgress)
			r.Put("/files/{id}/book/progress", s.saveBookProgress)
			r.Get("/files/{id}/book/flow", s.bookFlow)
			r.Get("/files/{id}/book/flow/chunks/{index}", s.bookFlowChunk)
			r.Get("/files/{id}/thumbnail", s.thumbnail)
			r.Get("/files/{id}/share", s.getShare)
			r.Post("/files/{id}/share", s.createShare)
			r.Delete("/files/{id}/share", s.revokeShare)
			r.Post("/directories", s.createDirectory)
			r.Post("/documents", s.createDocument)
			r.Patch("/files/{id}", s.patchFile)
			r.Post("/files/{id}/copy", s.copyFile)
			r.Post("/files/{id}/extract", s.startArchiveExtract)
			r.Delete("/files/{id}", s.deleteFile)
			r.Get("/trash", s.trash)
			r.Delete("/trash", s.emptyTrash)
			r.Post("/trash/{id}/restore", s.restoreTrash)
			r.Delete("/trash/{id}", s.purgeTrash)
			r.Post("/uploads", s.createUpload)
			r.Get("/uploads/{id}", s.getUpload)
			r.Post("/uploads/{id}/parts", s.uploadParts)
			r.Put("/uploads/{id}/parts/{part}", s.recordUploadPart)
			r.Post("/uploads/{id}/complete", s.completeUpload)
			r.Delete("/uploads/{id}", s.abortUpload)
			r.Post("/audio-merges", s.createAudioMerge)
			r.Post("/audio-merges/local", s.createLocalAudioMerge)
			r.Post("/audio-merges/local/{id}/files/{fileIndex}/chunks/{chunkIndex}", s.uploadLocalMergeChunk)
			r.Post("/audio-merges/local/{id}/complete", s.completeLocalAudioMerge)
			r.Post("/downloads", s.createDownload)
			r.Get("/downloads/{id}", s.getDownload)
			r.Post("/downloads/{id}/start", s.startDownload)
			r.Post("/downloads/{id}/pause", s.pauseDownload)
			r.Post("/downloads/{id}/resume", s.resumeDownload)
			r.Get("/downloads/{id}/files/{index}/stream", s.streamDownloadFile)
		})
	})
	r.Handle("/*", webui.Handler())
	return r
}

func (s *Server) ready(w http.ResponseWriter, r *http.Request) {
	ctx, cancel := context.WithTimeout(r.Context(), 3*time.Second)
	defer cancel()
	if err := s.db.PingContext(ctx); err != nil {
		problem(w, http.StatusServiceUnavailable, "database unavailable")
		return
	}
	if err := s.objects.Ping(ctx); err != nil {
		problem(w, http.StatusServiceUnavailable, "object storage unavailable")
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"status": "ready"})
}
func (s *Server) securityHeaders(next http.Handler) http.Handler {
	// CSP 中 S3 直连来源按 S3_PUBLIC_ENDPOINT 收窄；未配置公网 endpoint
	//（AWS 默认）时保持宽松 https/http，否则浏览器无法直连对象存储。
	imgSrc, mediaSrc, connectSrc := "'self' data: blob:", "'self' blob:", "'self'"
	if s.s3Origin != "" {
		imgSrc += " " + s.s3Origin
		mediaSrc += " " + s.s3Origin
		connectSrc += " " + s.s3Origin
	} else {
		imgSrc += " https: http:"
		mediaSrc += " https: http:"
		connectSrc += " https: http:"
	}
	csp := "default-src 'self'; script-src 'self'; img-src " + imgSrc + "; media-src " + mediaSrc +
		"; style-src 'self' 'unsafe-inline'; connect-src " + connectSrc +
		"; worker-src 'self' blob:; object-src 'none'; base-uri 'self'; form-action 'self'; frame-src 'none'; frame-ancestors 'none'"
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("X-Content-Type-Options", "nosniff")
		w.Header().Set("X-Frame-Options", "DENY")
		w.Header().Set("Referrer-Policy", "same-origin")
		w.Header().Set("Permissions-Policy", "camera=(), microphone=(), geolocation=(), payment=(), usb=()")
		w.Header().Set("Content-Security-Policy", csp)
		if strings.HasPrefix(s.cfg.BaseURL, "https://") {
			w.Header().Set("Strict-Transport-Security", "max-age=31536000")
		}
		if strings.HasPrefix(r.URL.Path, "/api/") {
			w.Header().Set("Cache-Control", "no-store")
		}
		next.ServeHTTP(w, r)
	})
}
func (s *Server) originGuard(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet && r.Method != http.MethodHead && r.Method != http.MethodOptions {
			// 浏览器对所有跨站/同站写请求都会带 Origin；不带 Origin 的
			// 写请求只可能来自非浏览器客户端，一律拒绝（CSRF 纵深防御，
			// SameSite=Lax Cookie 之外的第二道闸）。
			if origin := r.Header.Get("Origin"); origin == "" {
				problem(w, http.StatusForbidden, "origin required")
				return
			} else {
				base, _ := url.Parse(s.cfg.BaseURL)
				got, err := url.Parse(origin)
				if err != nil || !strings.EqualFold(base.Scheme, got.Scheme) || !strings.EqualFold(base.Host, got.Host) {
					problem(w, http.StatusForbidden, "origin not allowed")
					return
				}
			}
		}
		next.ServeHTTP(w, r)
	})
}
