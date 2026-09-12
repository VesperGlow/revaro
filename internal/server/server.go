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
	"github.com/VesperGlow/revaro/internal/cache"
	"github.com/VesperGlow/revaro/internal/config"
	"github.com/VesperGlow/revaro/internal/reader"
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
const maxLogicalFileSize = 1 << 40 // 1 TiB

type Server struct {
	uploadMu           sync.Mutex
	uploadOperations   map[string]*uploadOperation
	db                 *sql.DB
	storage            storage.Storage
	auth               *auth.Service
	cfg                config.Config
	log                *slog.Logger
	limiter            *loginLimiter
	shareSlots         chan struct{}
	workCtx            context.Context
	workCancel         context.CancelFunc
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
	batchTokensMu      sync.Mutex
	batchTokens        map[string]batchDownloadToken
	jobs               *JobManager
	tasks              *TaskManager
	objects            *ObjectManager
	cache              *cache.Manager
	books              *reader.Cache // 解析 Book 内存 LRU（注册进全局缓存管理器）
	cleanup            *CleanupManager
	media              *MediaPipeline
	lifecycleMu        sync.Mutex
	lifecycleClosing   bool
	lifecycleWG        sync.WaitGroup
	statusMu           sync.RWMutex
	statusSnapshot     systemStatusResponse
	statusSubscribers  map[chan systemStatusResponse]struct{}
	statusStop         chan struct{}
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

	workCtx, workCancel := context.WithCancel(context.Background())
	resources := newResourceGovernor()
	s := &Server{
		db: db, storage: store, auth: a, cfg: cfg, log: logger,
		jobs:            NewJobManager(),
		limiter:         newLoginLimiter(),
		shareSlots:      make(chan struct{}, 8),
		mediaAnalysis:   newMediaAnalysisScheduler(2),
		thumbnails:      newThumbnailScheduler(1),
		audioThumbSlots: make(chan struct{}, 1),
		archiveSlots:    make(chan struct{}, 1), archiveJobs: make(map[string]*archiveJob),
		workCtx: workCtx, workCancel: workCancel,
		statusSubscribers: make(map[chan systemStatusResponse]struct{}), statusStop: make(chan struct{}),
		batchTokens: make(map[string]batchDownloadToken),
	}
	s.objects = newObjectManager(store)
	s.objects.server = s
	s.books = reader.NewCache(bookCacheEntries, bookCacheBytes)
	s.cache = newGlobalCache(filepath.Join(cfg.WorkDir, "cache"), cfg.MediaCacheCapacity, s.books)
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
	// Only Revaro-owned, recognizable workspaces are eligible for startup
	// cleanup. Unknown APP_WORK_DIR contents are never touched.
	_ = os.MkdirAll(cfg.WorkDir, 0o700)
	for _, pattern := range []string{"revaro-extract-*"} {
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
	s.restorePersistentTasks()
	s.cleanup.Register("archive-password", time.Minute, time.Minute, false, func(context.Context) error { s.cleanupArchiveJobs(); return nil })
	s.cleanup.Register("cache", 5*time.Minute, time.Minute, false, func(context.Context) error { s.cache.Prune(); return nil })
	s.cleanup.Register("uploads", 15*time.Minute, 5*time.Minute, true, func(ctx context.Context) error { s.CleanupExpiredUploads(ctx); return nil })
	s.cleanup.Register("object-cleanup", 15*time.Minute, 5*time.Minute, true, func(ctx context.Context) error { s.CleanupObjects(ctx); return nil })
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
	if s.cleanup != nil {
		s.cleanup.Close()
	}
	if s.cache != nil {
		s.cache.Close()
	}
	if s.workCancel != nil {
		s.workCancel()
	}
	if s.jobs != nil {
		s.jobs.Close()
	}
	s.batchTokensMu.Lock()
	s.batchTokens = nil
	s.batchTokensMu.Unlock()
	if s.mediaAnalysis != nil {
		s.mediaAnalysis.close()
	}
	if s.thumbnails != nil {
		s.thumbnails.close()
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
			r.Post("/files/batch-download/prepare", s.prepareBatchDownload)
			r.Get("/files/batch-download/{token}", s.batchDownload)
			r.Get("/files/{id}/preview", s.preview)
			r.Get("/files/{id}/audio", s.audioMediaInfo)
			r.Get("/files/{id}/video", s.videoMediaInfo)
			r.Post("/files/{id}/media/reanalyze", s.reanalyzeMedia)
			r.Get("/files/{id}/video/subtitles/{subtitle}", s.videoSubtitle)
			r.Get("/files/{id}/media/progress", s.mediaProgress)
			r.Put("/files/{id}/media/progress", s.saveMediaProgress)
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
			r.Put("/uploads/{id}/data", s.uploadContent)
			r.Put("/uploads/{id}/data/{part}", s.uploadContent)
			r.Post("/uploads/{id}/parts", s.uploadParts)
			r.Put("/uploads/{id}/parts/{part}", s.recordUploadPart)
			r.Post("/uploads/{id}/complete", s.completeUpload)
			r.Delete("/uploads/{id}", s.abortUpload)
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
	imgSrc, mediaSrc, connectSrc := "'self' data: blob:", "'self' blob:", "'self'"
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
