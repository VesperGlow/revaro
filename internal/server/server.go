package server

import (
	"bytes"
	"context"
	"crypto/rand"
	"database/sql"
	"encoding/base64"
	"encoding/json"
	"errors"
	"image/png"
	"io"
	"log/slog"
	"mime"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"
	"unicode/utf8"

	"github.com/VesperGlow/revaro/internal/auth"
	"github.com/VesperGlow/revaro/internal/config"
	"github.com/VesperGlow/revaro/internal/ids"
	"github.com/VesperGlow/revaro/internal/storage"
	"github.com/VesperGlow/revaro/internal/webui"
	"github.com/go-chi/chi/v5"
	"github.com/go-chi/chi/v5/middleware"
	"github.com/pquerna/otp"
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
	videoSubtitleMu    sync.Mutex
	videoSubtitleCache map[string]*videoSubtitleCacheEntry
	videoSubtitleBytes int64
	mediaAnalysis      *mediaAnalysisScheduler
	mediaProbeGroup    singleflight.Group
	archiveSlots       chan struct{}
	archiveMu          sync.RWMutex
	archiveJobs        map[string]*archiveJob
	downloads          *downloadManager
	jobs               *JobManager
}

type File struct {
	ID              string  `json:"id"`
	ParentID        *string `json:"parent_id"`
	Name            string  `json:"name"`
	Kind            string  `json:"kind"`
	Size            int64   `json:"size"`
	MimeType        string  `json:"mime_type,omitempty"`
	ETag            string  `json:"etag,omitempty"`
	Status          string  `json:"status"`
	CreatedAt       string  `json:"created_at"`
	UpdatedAt       string  `json:"updated_at"`
	DeletedAt       string  `json:"deleted_at,omitempty"`
	RestoreParentID *string `json:"restore_parent_id,omitempty"`
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
		videoSubtitleCache: make(map[string]*videoSubtitleCacheEntry),
		mediaAnalysis:      newMediaAnalysisScheduler(2),
		archiveSlots:       make(chan struct{}, 1), archiveJobs: make(map[string]*archiveJob),
		audioHLSCtx: hlsCtx, audioHLSCancel: hlsCancel,
	}
	if cfg.BTEnabled {
		manager, err := newDownloadManager(s)
		if err != nil {
			logger.Error("built-in torrent engine unavailable", "error", err)
		} else {
			s.downloads = manager
		}
	}
	// Audio merges run in memory and cannot survive a process restart. Their
	// pending output row has no uploads record (browser uploads always do), so
	// remove only those abandoned placeholders before serving the file list.
	if result, err := db.Exec(`DELETE FROM files WHERE kind='file' AND status='pending' AND mime_type IN ('audio/mp4','audio/flac') AND object_key IS NULL AND NOT EXISTS (SELECT 1 FROM uploads WHERE uploads.file_id=files.id)`); err != nil {
		logger.Error("interrupted audio merge cleanup failed", "error", err)
	} else if removed, _ := result.RowsAffected(); removed > 0 {
		logger.Info("interrupted audio merges cleaned", "files", removed)
	}
	// Only Revaro-owned, recognizable workspaces are eligible for startup
	// cleanup. Unknown APP_WORK_DIR contents are never touched.
	_ = os.MkdirAll(cfg.WorkDir, 0o700)
	for _, pattern := range []string{"revaro-local-merge-*", "revaro-audio-merge-*", "revaro-audio-hls-*", "revaro-video-hls-*", "revaro-extract-*"} {
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
	go s.cleanupAudioHLSSessions()
	go s.cleanupVideoHLSSessions()
	go s.cleanupVideoFMP4Sessions()
	go s.cleanupArchiveJobs()
	return s
}

// Close stops transient playback transcoders and removes their temporary HLS
// segments. Persisted files and merge jobs are not affected.
func (s *Server) Close() {
	if s.jobs != nil {
		s.jobs.Close()
	}
	if s.downloads != nil {
		s.downloads.Close()
	}
	if s.audioHLSCancel != nil {
		s.audioHLSCancel()
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
			r.Get("/events", s.jobEvents)
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
			r.Get("/files/{id}/book/content", s.bookContent)
			r.Get("/files/{id}/book/assets/{index}", s.bookAsset)
			r.Get("/files/{id}/book/cover", s.bookCover)
			r.Get("/files/{id}/book/progress", s.bookProgress)
			r.Put("/files/{id}/book/progress", s.saveBookProgress)
			r.Get("/files/{id}/thumbnail", s.thumbnail)
			r.Put("/files/{id}/thumbnail", s.saveThumbnail)
			r.Get("/files/{id}/share", s.getShare)
			r.Post("/files/{id}/share", s.createShare)
			r.Delete("/files/{id}/share", s.revokeShare)
			r.Post("/directories", s.createDirectory)
			r.Post("/documents", s.createDocument)
			r.Patch("/files/{id}", s.patchFile)
			r.Post("/files/{id}/copy", s.copyFile)
			r.Post("/files/{id}/extract", s.startArchiveExtract)
			r.Get("/archive-jobs", s.listArchiveExtracts)
			r.Get("/archive-jobs/{id}", s.getArchiveExtract)
			r.Post("/archive-jobs/{id}/password", s.resumeArchiveExtract)
			r.Post("/archive-jobs/{id}/cancel", s.cancelArchiveExtract)
			r.Delete("/archive-jobs/{id}", s.deleteArchiveExtract)
			r.Delete("/files/{id}", s.deleteFile)
			r.Get("/trash", s.trash)
			r.Delete("/trash", s.emptyTrash)
			r.Post("/trash/{id}/restore", s.restoreTrash)
			r.Delete("/trash/{id}", s.purgeTrash)
			r.Post("/uploads", s.createUpload)
			r.Post("/uploads/{id}/parts", s.uploadParts)
			r.Post("/uploads/{id}/complete", s.completeUpload)
			r.Delete("/uploads/{id}", s.abortUpload)
			r.Post("/audio-merges", s.createAudioMerge)
			r.Get("/audio-merges", s.listAudioMerges)
			r.Get("/audio-merges/{id}", s.getAudioMerge)
			r.Delete("/audio-merges/{id}", s.cancelAudioMerge)
			r.Post("/audio-merges/local", s.createLocalAudioMerge)
			r.Post("/audio-merges/local/{id}/files/{fileIndex}/chunks/{chunkIndex}", s.uploadLocalMergeChunk)
			r.Post("/audio-merges/local/{id}/complete", s.completeLocalAudioMerge)
			r.Post("/downloads", s.createDownload)
			r.Get("/downloads", s.listDownloads)
			r.Get("/downloads/{id}", s.getDownload)
			r.Post("/downloads/{id}/start", s.startDownload)
			r.Post("/downloads/{id}/pause", s.pauseDownload)
			r.Post("/downloads/{id}/resume", s.resumeDownload)
			r.Delete("/downloads/{id}", s.deleteDownload)
			r.Get("/downloads/{id}/files/{index}/stream", s.streamDownloadFile)
		})
	})
	r.Handle("/*", webui.Handler())
	return r
}

func (s *Server) ready(w http.ResponseWriter, r *http.Request) {
	if err := s.db.PingContext(r.Context()); err != nil {
		problem(w, http.StatusServiceUnavailable, "database unavailable")
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

func (s *Server) login(w http.ResponseWriter, r *http.Request) {
	ip, _, _ := net.SplitHostPort(r.RemoteAddr)
	if ip == "" {
		ip = r.RemoteAddr
	}
	if !s.limiter.allow(ip) {
		w.Header().Set("Retry-After", "60")
		problem(w, http.StatusTooManyRequests, "too many login attempts; try again later")
		return
	}
	var in struct {
		Username     string `json:"username"`
		Password     string `json:"password"`
		SecondFactor string `json:"second_factor"`
	}
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	if len(in.Username) > 128 || len(in.Password) > 1024 || len(in.SecondFactor) > 128 {
		s.limiter.fail(ip)
		problem(w, http.StatusUnauthorized, "invalid credentials")
		return
	}
	if !s.limiter.acquire() {
		w.Header().Set("Retry-After", "2")
		problem(w, http.StatusTooManyRequests, "login verification is busy; try again shortly")
		return
	}
	defer s.limiter.release()
	token, expires, err := s.auth.Login(r.Context(), in.Username, in.Password, in.SecondFactor)
	if err != nil {
		switch {
		case errors.Is(err, auth.ErrTOTPRequired):
			problemCode(w, http.StatusUnauthorized, "totp_required", "enter your authenticator or recovery code")
		case errors.Is(err, auth.ErrInvalidSecondFactor):
			s.limiter.fail(ip)
			problemCode(w, http.StatusUnauthorized, "invalid_second_factor", "the authenticator or recovery code is incorrect")
		case errors.Is(err, auth.ErrInvalidCredentials):
			s.limiter.fail(ip)
			problem(w, http.StatusUnauthorized, "invalid credentials")
		default:
			s.log.Error("login failed", "error", err)
			problem(w, http.StatusInternalServerError, "could not verify login")
		}
		return
	}
	s.limiter.success(ip)
	http.SetCookie(w, &http.Cookie{Name: "revaro_session", Value: token, Path: "/", HttpOnly: true, Secure: s.cfg.CookieSecure, SameSite: http.SameSiteLaxMode, Expires: expires, MaxAge: int(time.Until(expires).Seconds())})
	s.log.Info("user logged in", "user", in.Username)
	writeJSON(w, http.StatusOK, map[string]any{"username": in.Username, "has_avatar": s.hasAvatar(r.Context())})
}
func (s *Server) logout(w http.ResponseWriter, r *http.Request) {
	if c, err := r.Cookie("revaro_session"); err == nil {
		s.auth.Logout(r.Context(), c.Value)
	}
	s.clearSessionCookie(w)
	w.WriteHeader(http.StatusNoContent)
}
func (s *Server) me(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, http.StatusOK, map[string]any{"username": r.Context().Value(userKey{}).(string), "has_avatar": s.hasAvatar(r.Context())})
}

func (s *Server) totpStatus(w http.ResponseWriter, r *http.Request) {
	status, err := s.auth.TOTPStatus(r.Context())
	if err != nil {
		s.log.Error("TOTP status read failed", "error", err)
		problem(w, http.StatusInternalServerError, "could not read two-factor settings")
		return
	}
	writeJSON(w, http.StatusOK, status)
}

func (s *Server) beginTOTPSetup(w http.ResponseWriter, r *http.Request) {
	var in struct {
		CurrentPassword string `json:"current_password"`
	}
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	if in.CurrentPassword == "" || len(in.CurrentPassword) > 1024 {
		problem(w, http.StatusBadRequest, "current password is required")
		return
	}
	username := r.Context().Value(userKey{}).(string)
	setup, err := s.auth.BeginTOTPSetup(r.Context(), username, in.CurrentPassword)
	if err != nil {
		s.totpProblem(w, err)
		return
	}
	key, err := otp.NewKeyFromURL(setup.URI)
	if err != nil {
		s.log.Error("TOTP QR key creation failed", "error", err)
		problem(w, http.StatusInternalServerError, "could not create authenticator QR code")
		return
	}
	image, err := key.Image(256, 256)
	if err != nil {
		s.log.Error("TOTP QR image creation failed", "error", err)
		problem(w, http.StatusInternalServerError, "could not create authenticator QR code")
		return
	}
	var pngData bytes.Buffer
	if err := png.Encode(&pngData, image); err != nil {
		s.log.Error("TOTP QR encoding failed", "error", err)
		problem(w, http.StatusInternalServerError, "could not create authenticator QR code")
		return
	}
	writeJSON(w, http.StatusCreated, map[string]any{
		"secret":      setup.Secret,
		"uri":         setup.URI,
		"qr_data_url": "data:image/png;base64," + base64.StdEncoding.EncodeToString(pngData.Bytes()),
	})
}

type totpConfirmation struct {
	CurrentPassword string `json:"current_password"`
	Code            string `json:"code"`
}

func (s *Server) enableTOTP(w http.ResponseWriter, r *http.Request) {
	var in totpConfirmation
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	if !validTOTPConfirmation(in) {
		problem(w, http.StatusBadRequest, "current password and verification code are required")
		return
	}
	codes, err := s.auth.ConfirmTOTPSetup(r.Context(), r.Context().Value(userKey{}).(string), in.CurrentPassword, in.Code, currentSessionToken(r))
	if err != nil {
		s.totpProblem(w, err)
		return
	}
	s.log.Info("TOTP two-factor authentication enabled", "user", r.Context().Value(userKey{}).(string))
	writeJSON(w, http.StatusOK, map[string]any{"enabled": true, "recovery_codes": codes})
}

func (s *Server) regenerateTOTPRecoveryCodes(w http.ResponseWriter, r *http.Request) {
	var in totpConfirmation
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	if !validTOTPConfirmation(in) {
		problem(w, http.StatusBadRequest, "current password and verification code are required")
		return
	}
	codes, err := s.auth.RegenerateRecoveryCodes(r.Context(), r.Context().Value(userKey{}).(string), in.CurrentPassword, in.Code)
	if err != nil {
		s.totpProblem(w, err)
		return
	}
	s.log.Info("TOTP recovery codes regenerated", "user", r.Context().Value(userKey{}).(string))
	writeJSON(w, http.StatusOK, map[string]any{"enabled": true, "recovery_codes": codes})
}

func (s *Server) disableTOTP(w http.ResponseWriter, r *http.Request) {
	var in totpConfirmation
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	if !validTOTPConfirmation(in) {
		problem(w, http.StatusBadRequest, "current password and verification code are required")
		return
	}
	username := r.Context().Value(userKey{}).(string)
	if err := s.auth.DisableTOTP(r.Context(), username, in.CurrentPassword, in.Code, currentSessionToken(r)); err != nil {
		s.totpProblem(w, err)
		return
	}
	s.log.Info("TOTP two-factor authentication disabled", "user", username)
	w.WriteHeader(http.StatusNoContent)
}

func validTOTPConfirmation(in totpConfirmation) bool {
	return in.CurrentPassword != "" && len(in.CurrentPassword) <= 1024 && strings.TrimSpace(in.Code) != "" && len(in.Code) <= 128
}

func currentSessionToken(r *http.Request) string {
	if cookie, err := r.Cookie("revaro_session"); err == nil {
		return cookie.Value
	}
	return ""
}

func (s *Server) totpProblem(w http.ResponseWriter, err error) {
	switch {
	case errors.Is(err, auth.ErrInvalidCredentials):
		problem(w, http.StatusUnauthorized, "current password is incorrect")
	case errors.Is(err, auth.ErrInvalidSecondFactor):
		problem(w, http.StatusUnauthorized, "the authenticator or recovery code is incorrect")
	case errors.Is(err, auth.ErrTOTPAlreadyEnabled):
		problem(w, http.StatusConflict, "two-factor authentication is already enabled")
	case errors.Is(err, auth.ErrTOTPNotEnabled):
		problem(w, http.StatusConflict, "two-factor authentication is not enabled")
	case errors.Is(err, auth.ErrTOTPSetupExpired):
		problem(w, http.StatusGone, "two-factor setup expired; start again")
	default:
		s.log.Error("TOTP operation failed", "error", err)
		problem(w, http.StatusInternalServerError, "could not update two-factor settings")
	}
}

func (s *Server) hasAvatar(ctx context.Context) bool {
	var value string
	return s.db.QueryRowContext(ctx, `SELECT value FROM settings WHERE key='avatar_mime'`).Scan(&value) == nil && value != ""
}

func (s *Server) getAvatar(w http.ResponseWriter, r *http.Request) {
	var contentType string
	if err := s.db.QueryRowContext(r.Context(), `SELECT value FROM settings WHERE key='avatar_mime'`).Scan(&contentType); err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			problem(w, http.StatusNotFound, "avatar not found")
		} else {
			problem(w, http.StatusInternalServerError, "database error")
		}
		return
	}
	data, err := s.storage.GetObject(r.Context(), avatarObjectKey, maxAvatarBytes)
	if err != nil {
		s.log.Error("avatar read failed", "error", err)
		problem(w, http.StatusBadGateway, "could not read avatar")
		return
	}
	w.Header().Set("Content-Type", contentType)
	w.Header().Set("Content-Length", strconv.Itoa(len(data)))
	w.Header().Set("Content-Disposition", "inline")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(data)
}

func (s *Server) updateAvatar(w http.ResponseWriter, r *http.Request) {
	var in struct {
		DataURL string `json:"data_url"`
	}
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	comma := strings.IndexByte(in.DataURL, ',')
	if comma < 0 || !strings.HasPrefix(in.DataURL, "data:image/") {
		problem(w, http.StatusBadRequest, "avatar must be a data URL")
		return
	}
	data, err := base64.StdEncoding.DecodeString(in.DataURL[comma+1:])
	if err != nil || len(data) == 0 {
		problem(w, http.StatusBadRequest, "avatar data is invalid")
		return
	}
	if len(data) > maxAvatarBytes {
		problem(w, http.StatusRequestEntityTooLarge, "avatar must not exceed 2 MiB")
		return
	}
	contentType := http.DetectContentType(data)
	switch contentType {
	case "image/jpeg", "image/png", "image/gif", "image/webp":
	default:
		problem(w, http.StatusUnsupportedMediaType, "avatar must be JPEG, PNG, GIF, or WebP")
		return
	}
	if _, err := s.storage.PutObject(r.Context(), avatarObjectKey, contentType, data); err != nil {
		s.log.Error("avatar write failed", "error", err)
		problem(w, http.StatusBadGateway, "could not save avatar")
		return
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	if _, err := s.db.ExecContext(r.Context(), `INSERT INTO settings(key,value,updated_at) VALUES('avatar_mime',?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at`, contentType, now); err != nil {
		s.log.Error("avatar metadata write failed", "error", err)
		problem(w, http.StatusInternalServerError, "could not save avatar")
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) deleteAvatar(w http.ResponseWriter, r *http.Request) {
	if err := s.storage.DeleteObject(r.Context(), avatarObjectKey); err != nil {
		s.log.Error("avatar delete failed", "error", err)
		problem(w, http.StatusBadGateway, "could not delete avatar")
		return
	}
	if _, err := s.db.ExecContext(r.Context(), `DELETE FROM settings WHERE key='avatar_mime'`); err != nil {
		problem(w, http.StatusInternalServerError, "could not delete avatar")
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) changeCredentials(w http.ResponseWriter, r *http.Request) {
	var in struct {
		CurrentPassword string `json:"current_password"`
		Username        string `json:"username"`
		Password        string `json:"password"`
	}
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	if in.Username == "" || len(in.Username) > 128 || len(in.Password) < 12 || len(in.Password) > 1024 || len(in.CurrentPassword) > 1024 {
		problem(w, http.StatusBadRequest, "username is required and password must be between 12 and 1024 characters")
		return
	}
	currentUsername := r.Context().Value(userKey{}).(string)
	if err := s.auth.ChangeCredentials(r.Context(), currentUsername, in.CurrentPassword, in.Username, in.Password); err != nil {
		if errors.Is(err, auth.ErrInvalidCredentials) {
			problem(w, http.StatusUnauthorized, "current password is incorrect")
			return
		}
		s.log.Error("credential change failed", "error", err)
		problem(w, http.StatusInternalServerError, "could not update credentials")
		return
	}
	s.clearSessionCookie(w)
	s.log.Info("administrator credentials changed", "previous_user", currentUsername, "user", in.Username)
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) changePassword(w http.ResponseWriter, r *http.Request) {
	var in struct {
		CurrentPassword string `json:"current_password"`
		Password        string `json:"password"`
	}
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	if len(in.Password) < 12 || len(in.Password) > 1024 || len(in.CurrentPassword) > 1024 {
		problem(w, http.StatusBadRequest, "password must be between 12 and 1024 characters")
		return
	}
	username := r.Context().Value(userKey{}).(string)
	if err := s.auth.ChangeCredentials(r.Context(), username, in.CurrentPassword, username, in.Password); err != nil {
		if errors.Is(err, auth.ErrInvalidCredentials) {
			problem(w, http.StatusUnauthorized, "current password is incorrect")
			return
		}
		s.log.Error("password change failed", "error", err)
		problem(w, http.StatusInternalServerError, "could not update password")
		return
	}
	s.clearSessionCookie(w)
	s.log.Info("administrator password changed", "user", username)
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) changeUsername(w http.ResponseWriter, r *http.Request) {
	var in struct {
		Username string `json:"username"`
	}
	if err := decodeJSON(w, r, &in); err != nil {
		return
	}
	in.Username = strings.TrimSpace(in.Username)
	if in.Username == "" || len(in.Username) > 128 {
		problem(w, http.StatusBadRequest, "username must be between 1 and 128 characters")
		return
	}
	previous := r.Context().Value(userKey{}).(string)
	if err := s.auth.ChangeUsername(r.Context(), in.Username); err != nil {
		s.log.Error("username change failed", "error", err)
		problem(w, http.StatusInternalServerError, "could not update username")
		return
	}
	s.log.Info("administrator username changed", "previous_user", previous, "user", in.Username)
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) clearSessionCookie(w http.ResponseWriter) {
	http.SetCookie(w, &http.Cookie{Name: "revaro_session", Value: "", Path: "/", HttpOnly: true, Secure: s.cfg.CookieSecure, SameSite: http.SameSiteLaxMode, MaxAge: -1, Expires: time.Unix(1, 0)})
}

type userKey struct{}

func (s *Server) requireAuth(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		c, err := r.Cookie("revaro_session")
		if err != nil {
			problem(w, http.StatusUnauthorized, "authentication required")
			return
		}
		user, err := s.auth.Authenticate(r.Context(), c.Value)
		if err != nil {
			problem(w, http.StatusUnauthorized, "authentication required")
			return
		}
		next.ServeHTTP(w, r.WithContext(context.WithValue(r.Context(), userKey{}, user)))
	})
}

func scanFile(row interface{ Scan(...any) error }) (File, error) {
	var f File
	var parent, mime, etag, deleted, restoreParent sql.NullString
	err := row.Scan(&f.ID, &parent, &f.Name, &f.Kind, &f.objectKey, &f.Size, &mime, &etag, &f.Status, &f.CreatedAt, &f.UpdatedAt, &deleted, &restoreParent)
	if parent.Valid {
		f.ParentID = &parent.String
	}
	f.MimeType = mime.String
	f.ETag = etag.String
	f.DeletedAt = deleted.String
	if restoreParent.Valid {
		f.RestoreParentID = &restoreParent.String
	}
	return f, err
}

const fileColumns = `id,parent_id,name,kind,COALESCE(object_key,''),size,mime_type,etag,status,created_at,updated_at,deleted_at,restore_parent_id`

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
	const qualified = `f.id,f.parent_id,f.name,f.kind,COALESCE(f.object_key,''),f.size,f.mime_type,f.etag,f.status,f.created_at,f.updated_at,f.deleted_at,f.restore_parent_id`
	rows, err := s.db.QueryContext(ctx, `WITH RECURSIVE p(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at,deleted_at,restore_parent_id,depth) AS (SELECT `+fileColumns+`,0 FROM files WHERE id=? AND deleted_at IS NULL UNION ALL SELECT `+qualified+`,p.depth+1 FROM files f JOIN p ON f.id=p.parent_id WHERE f.deleted_at IS NULL) SELECT `+fileColumns+` FROM p ORDER BY depth DESC`, id)
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
		problem(w, http.StatusConflict, "an item with that name already exists")
		return
	}
	if err != nil {
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
		problem(w, http.StatusInternalServerError, "document content changed but metadata update failed")
		return
	}
	if n, _ := res.RowsAffected(); n == 0 {
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

// serveFileContent redirects opaque blobs to S3 so Range, cancellation and
// backpressure stay end-to-end.
func (s *Server) serveFileContent(w http.ResponseWriter, r *http.Request, f File, inline bool) {
	mimeType := safeDeliveryMime(responseMime(f))
	if mimeType == "application/octet-stream" {
		inline = false
	}
	if s.cfg.ProxyTransfers {
		rc, err := s.storage.Open(storage.WithDynamicReadAhead(r.Context()), f.objectKey)
		if err != nil {
			problem(w, http.StatusBadGateway, "object storage read failed")
			return
		}
		defer rc.Close()
		w.Header().Set("Content-Type", mimeType)
		disposition := "attachment"
		if inline {
			disposition = "inline"
		}
		w.Header().Set("Content-Disposition", disposition+"; filename*=UTF-8''"+strings.ReplaceAll(url.PathEscape(f.Name), "+", "%20"))
		http.ServeContent(w, r, f.Name, time.Time{}, rc)
		return
	}
	u, err := s.storage.PresignGetObject(r.Context(), f.objectKey, f.Name, mimeType, inline, s.cfg.PresignExpires)
	if err != nil {
		problem(w, http.StatusBadGateway, "could not create download URL")
		return
	}
	http.Redirect(w, r, u, http.StatusFound)
}
func (s *Server) readContent(ctx context.Context, f File) ([]byte, error) {
	return s.storage.GetObject(ctx, f.objectKey, maxDocumentBytes)
}

func (s *Server) storeBlob(ctx context.Context, body io.Reader, size int64, mimeType string) (string, storage.ObjectInfo, error) {
	key := storage.BlobKey(ids.New())
	info, err := s.storage.StoreBlob(ctx, key, mimeType, body, size)
	if err != nil {
		return "", storage.ObjectInfo{}, err
	}
	return key, info, nil
}

func responseMime(f File) string {
	if f.MimeType != "" && f.MimeType != "application/octet-stream" {
		return f.MimeType
	}
	switch strings.ToLower(filepath.Ext(f.Name)) {
	case ".jpg", ".jpeg":
		return "image/jpeg"
	case ".png":
		return "image/png"
	case ".gif":
		return "image/gif"
	case ".webp":
		return "image/webp"
	case ".avif":
		return "image/avif"
	case ".mp4":
		return "video/mp4"
	case ".webm":
		return "video/webm"
	case ".ogv":
		return "video/ogg"
	case ".mov":
		return "video/quicktime"
	case ".m4v":
		return "video/x-m4v"
	case ".mkv":
		return "video/x-matroska"
	case ".avi":
		return "video/x-msvideo"
	case ".flv":
		return "video/x-flv"
	case ".wmv":
		return "video/x-ms-wmv"
	case ".mpg", ".mpeg":
		return "video/mpeg"
	case ".ts", ".m2ts", ".mts":
		return "video/mp2t"
	case ".mp3":
		return "audio/mpeg"
	case ".wav":
		return "audio/wav"
	case ".ogg", ".oga":
		return "audio/ogg"
	case ".m4a":
		return "audio/mp4"
	case ".aac":
		return "audio/aac"
	case ".flac":
		return "audio/flac"
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
	case ".txt", ".conf", ".ini", ".log":
		return "text/plain; charset=utf-8"
	default:
		return f.MimeType
	}
}

// Active web formats must never be served with an executable MIME type from
// the application origin. They remain downloadable as opaque bytes.
func safeDeliveryMime(value string) string {
	mediaType, _, err := mime.ParseMediaType(value)
	if err != nil {
		return "application/octet-stream"
	}
	switch strings.ToLower(mediaType) {
	case "text/html", "application/xhtml+xml", "image/svg+xml", "application/xml", "text/xml",
		"application/javascript", "text/javascript", "application/ecmascript", "text/ecmascript":
		return "application/octet-stream"
	default:
		return value
	}
}

func newShareToken() (string, error) {
	b := make([]byte, 32)
	if _, err := rand.Read(b); err != nil {
		return "", err
	}
	return base64.RawURLEncoding.EncodeToString(b), nil
}

func (s *Server) getShare(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	var token, created string
	err := s.db.QueryRowContext(r.Context(), `SELECT s.token,s.created_at FROM shares s JOIN files f ON f.id=s.file_id WHERE s.file_id=? AND f.kind='file' AND f.status='ready' AND f.deleted_at IS NULL`, id).Scan(&token, &created)
	if errors.Is(err, sql.ErrNoRows) {
		writeJSON(w, http.StatusOK, map[string]any{"active": false})
		return
	}
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not read share")
		return
	}
	writeJSON(w, http.StatusOK, map[string]any{"active": true, "url": s.cfg.BaseURL + "/s/" + token, "created_at": created})
}

func (s *Server) createShare(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	f, err := s.file(r.Context(), id)
	if err != nil || f.Kind != "file" || f.Status != "ready" {
		problem(w, http.StatusNotFound, "ready file not found")
		return
	}
	token, err := newShareToken()
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not generate share link")
		return
	}
	created := time.Now().UTC().Format(time.RFC3339Nano)
	_, err = s.db.ExecContext(r.Context(), `INSERT INTO shares(file_id,token,created_at) VALUES(?,?,?) ON CONFLICT(file_id) DO UPDATE SET token=excluded.token,created_at=excluded.created_at`, id, token, created)
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not create share link")
		return
	}
	s.log.Info("file share created", "file_id", id)
	writeJSON(w, http.StatusCreated, map[string]any{"active": true, "url": s.cfg.BaseURL + "/s/" + token, "created_at": created})
}

func (s *Server) revokeShare(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if _, err := s.db.ExecContext(r.Context(), `DELETE FROM shares WHERE file_id=?`, id); err != nil {
		problem(w, http.StatusInternalServerError, "could not revoke share link")
		return
	}
	s.log.Info("file share revoked", "file_id", id)
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) publicShare(w http.ResponseWriter, r *http.Request) {
	token := chi.URLParam(r, "token")
	if len(token) < 32 || len(token) > 128 {
		problem(w, http.StatusNotFound, "share not found")
		return
	}
	var f File
	var parent, mime, etag sql.NullString
	err := s.db.QueryRowContext(r.Context(), `SELECT f.id,f.parent_id,f.name,f.kind,COALESCE(f.object_key,''),f.size,f.mime_type,f.etag,f.status,f.created_at,f.updated_at FROM shares s JOIN files f ON f.id=s.file_id WHERE s.token=? AND f.kind='file' AND f.status='ready' AND f.deleted_at IS NULL`, token).Scan(&f.ID, &parent, &f.Name, &f.Kind, &f.objectKey, &f.Size, &mime, &etag, &f.Status, &f.CreatedAt, &f.UpdatedAt)
	if errors.Is(err, sql.ErrNoRows) {
		problem(w, http.StatusNotFound, "share not found")
		return
	}
	if err != nil {
		problem(w, http.StatusInternalServerError, "could not open share")
		return
	}
	f.MimeType = mime.String
	w.Header().Set("Cache-Control", "no-store")
	w.Header().Set("Referrer-Policy", "no-referrer")
	w.Header().Set("X-Robots-Tag", "noindex, nofollow, noarchive")
	w.Header().Set("Content-Security-Policy", "sandbox; default-src 'none'; base-uri 'none'; form-action 'none'")
	select {
	case s.shareSlots <- struct{}{}:
		defer func() { <-s.shareSlots }()
	default:
		w.Header().Set("Retry-After", "5")
		problem(w, http.StatusTooManyRequests, "public downloads are busy; try again shortly")
		return
	}
	// 分享 URL 等同于访问凭据，记录每次访问（token 只记前缀掩码），
	// 便于发现泄露后定位访问来源。
	s.log.Info("public share served", "file_id", f.ID, "file", f.Name, "token_prefix", token[:min(len(token), 8)])
	s.serveFileContent(w, r, f, isPreviewable(f))
}

type createUploadInput struct {
	ParentID string `json:"parent_id"`
	Name     string `json:"name"`
	Size     int64  `json:"size"`
	MimeType string `json:"mime_type"`
}

const multipartUploadThreshold = int64(16 << 20)
const defaultMultipartPartSize = int64(16 << 20)

func multipartPartSize(size int64) int64 {
	partSize := defaultMultipartPartSize
	if size > partSize*10000 {
		// Keep safely below S3's 10,000-part limit and round to MiB so the
		// browser can slice without awkward byte boundaries.
		partSize = ((size+9999)/10000 + (1 << 20) - 1) / (1 << 20) * (1 << 20)
	}
	return partSize
}

func (s *Server) createUpload(w http.ResponseWriter, r *http.Request) {
	var in createUploadInput
	if decodeJSON(w, r, &in) != nil {
		return
	}
	if err := validateName(in.Name); err != nil {
		problem(w, 400, err.Error())
		return
	}
	if in.Size < 0 || in.Size > maxLogicalFileSize {
		problem(w, 400, "invalid file size")
		return
	}
	if len(in.MimeType) > 255 {
		problem(w, 400, "mime type is too long")
		return
	}
	if in.MimeType == "" {
		in.MimeType = "application/octet-stream"
	}
	if _, _, err := mime.ParseMediaType(in.MimeType); err != nil {
		problem(w, 400, "mime type is invalid")
		return
	}
	p, err := s.file(r.Context(), in.ParentID)
	if err != nil || p.Kind != "directory" || p.Status != "ready" {
		problem(w, 400, "parent directory is invalid")
		return
	}
	fileID, uploadID := ids.New(), ids.New()
	objectKey := storage.BlobKey(ids.New())
	mode := "single"
	partSize := max(in.Size, int64(1))
	var s3UploadID, uploadURL string
	var partCount int
	if in.Size >= multipartUploadThreshold {
		mode = "multipart"
		partSize = multipartPartSize(in.Size)
		partCount, err = storage.ValidMultipartPartCount(in.Size, partSize)
		if err == nil {
			s3UploadID, err = s.storage.CreateMultipart(r.Context(), objectKey, in.MimeType)
		}
	} else {
		uploadURL, err = s.storage.PresignPutObject(r.Context(), objectKey, in.MimeType, s.cfg.PresignExpires)
	}
	if err != nil {
		s.log.Error("blob upload initialization failed", "file", in.Name, "mode", mode, "error", err)
		problem(w, http.StatusBadGateway, "object storage could not initialize the upload")
		return
	}
	now := time.Now().UTC()
	expires := now.Add(s.cfg.UploadExpires)
	tx, err := s.db.BeginTx(r.Context(), nil)
	if err != nil {
		problem(w, 500, "database error")
		return
	}
	_, err = tx.ExecContext(r.Context(), `INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,status,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?)`, fileID, in.ParentID, in.Name, "file", objectKey, in.Size, in.MimeType, "pending", now.Format(time.RFC3339Nano), now.Format(time.RFC3339Nano))
	if err == nil {
		_, err = tx.ExecContext(r.Context(), `INSERT INTO uploads(id,file_id,mode,object_key,s3_upload_id,part_size,expected_size,mime_type,status,created_at,expires_at) VALUES(?,?,?,?,?,?,?,?, 'pending',?,?)`, uploadID, fileID, mode, objectKey, nullString(s3UploadID), partSize, in.Size, in.MimeType, now.Format(time.RFC3339Nano), expires.Format(time.RFC3339Nano))
	}
	if err != nil {
		tx.Rollback()
		if s3UploadID != "" {
			_ = s.storage.AbortMultipart(context.Background(), objectKey, s3UploadID)
		}
		if isConflict(err) {
			problem(w, 409, "an item with that name already exists")
		} else {
			problem(w, 500, "could not create upload")
		}
		return
	}
	if err = tx.Commit(); err != nil {
		if s3UploadID != "" {
			_ = s.storage.AbortMultipart(context.Background(), objectKey, s3UploadID)
		}
		problem(w, 500, "could not create upload")
		return
	}
	s.log.Info("blob upload created", "file", in.Name, "size", in.Size, "mode", mode, "object_key", objectKey, "part_size", partSize, "parts", partCount)
	writeJSON(w, 201, map[string]any{
		"upload_id": uploadID,
		"file_id":   fileID,
		"mode":      mode, "url": uploadURL, "part_size": partSize, "part_count": partCount,
		"expires_at": expires.Format(time.RFC3339Nano),
	})
}

func nullString(value string) any {
	if value == "" {
		return nil
	}
	return value
}

type uploadRecord struct {
	ID, FileID, Mode, ObjectKey, S3UploadID, MimeType, Status, ExpiresAt string
	PartSize, ExpectedSize                                               int64
}

func (u uploadRecord) expired(now time.Time) bool {
	t, err := time.Parse(time.RFC3339Nano, u.ExpiresAt)
	return err != nil || !t.After(now)
}

func (s *Server) upload(ctx context.Context, id string) (uploadRecord, error) {
	var u uploadRecord
	err := s.db.QueryRowContext(ctx, `SELECT id,file_id,mode,object_key,COALESCE(s3_upload_id,''),part_size,expected_size,mime_type,status,expires_at FROM uploads WHERE id=?`, id).Scan(&u.ID, &u.FileID, &u.Mode, &u.ObjectKey, &u.S3UploadID, &u.PartSize, &u.ExpectedSize, &u.MimeType, &u.Status, &u.ExpiresAt)
	return u, err
}

func (s *Server) uploadParts(w http.ResponseWriter, r *http.Request) {
	u, err := s.upload(r.Context(), chi.URLParam(r, "id"))
	if err != nil || u.Status != "pending" || u.Mode != "multipart" || u.expired(time.Now().UTC()) {
		problem(w, 404, "pending upload not found")
		return
	}
	var body struct {
		PartNumbers []int32 `json:"part_numbers"`
	}
	if decodeJSON(w, r, &body) != nil {
		return
	}
	if len(body.PartNumbers) == 0 || len(body.PartNumbers) > 100 {
		problem(w, 400, "request between 1 and 100 upload parts")
		return
	}
	partCount, _ := storage.ValidMultipartPartCount(u.ExpectedSize, u.PartSize)
	seen := make(map[int32]bool, len(body.PartNumbers))
	parts := make([]map[string]any, len(body.PartNumbers))
	for i, partNumber := range body.PartNumbers {
		if partNumber < 1 || int(partNumber) > partCount || seen[partNumber] {
			problem(w, 400, "invalid multipart part number")
			return
		}
		seen[partNumber] = true
		url, signErr := s.storage.PresignUploadPart(r.Context(), u.ObjectKey, u.S3UploadID, partNumber, s.cfg.PresignExpires)
		if signErr != nil {
			s.log.Error("multipart part signing failed", "upload", u.ID, "part", partNumber, "error", signErr)
			problem(w, 502, "object storage could not prepare upload parts")
			return
		}
		parts[i] = map[string]any{"part_number": partNumber, "url": url}
	}
	writeJSON(w, 200, map[string]any{"parts": parts})
}

func (s *Server) completeUpload(w http.ResponseWriter, r *http.Request) {
	u, err := s.upload(r.Context(), chi.URLParam(r, "id"))
	if err != nil || u.Status != "pending" || u.expired(time.Now().UTC()) {
		problem(w, 404, "pending upload not found")
		return
	}
	var body struct {
		Parts []storage.CompletedPart `json:"parts"`
	}
	if decodeJSONLimit(w, r, &body, 2<<20) != nil {
		return
	}
	var info storage.ObjectInfo
	if u.Mode == "multipart" {
		partCount, _ := storage.ValidMultipartPartCount(u.ExpectedSize, u.PartSize)
		if len(body.Parts) != partCount {
			problem(w, 400, "multipart completion list is incomplete")
			return
		}
		sort.Slice(body.Parts, func(i, j int) bool { return body.Parts[i].PartNumber < body.Parts[j].PartNumber })
		for i, part := range body.Parts {
			if part.PartNumber != int32(i+1) || strings.TrimSpace(part.ETag) == "" {
				problem(w, 400, "multipart completion list is invalid")
				return
			}
		}
		info, err = s.storage.CompleteMultipart(r.Context(), u.ObjectKey, u.S3UploadID, body.Parts)
	} else {
		if len(body.Parts) != 0 {
			problem(w, 400, "single upload must not include multipart parts")
			return
		}
		info, err = s.storage.HeadObject(r.Context(), u.ObjectKey)
	}
	if err != nil {
		s.log.Error("blob upload completion failed", "upload", u.ID, "mode", u.Mode, "error", err)
		problem(w, 502, "object storage could not complete the upload")
		return
	}
	if info.Size != u.ExpectedSize {
		_ = s.storage.DeleteObject(r.Context(), u.ObjectKey)
		problem(w, 400, "uploaded object size does not match the declared size")
		return
	}
	if err := s.finalizeUpload(r.Context(), u, info.ETag); err != nil {
		problem(w, 500, "object stored but metadata finalization failed")
		return
	}
	f, _ := s.file(r.Context(), u.FileID)
	s.log.Info("blob upload completed", "file", f.Name, "mode", u.Mode, "size", info.Size, "object_key", u.ObjectKey)
	s.scheduleMediaAnalysis(f)
	writeJSON(w, 200, f)
}

func (s *Server) finalizeUpload(ctx context.Context, u uploadRecord, etag string) error {
	tx, err := s.db.BeginTx(ctx, nil)
	if err == nil {
		_, err = tx.ExecContext(ctx, `UPDATE files SET status='ready',size=?,etag=?,updated_at=? WHERE id=? AND status='pending' AND object_key=?`, u.ExpectedSize, etag, time.Now().UTC().Format(time.RFC3339Nano), u.FileID, u.ObjectKey)
	}
	if err == nil {
		_, err = tx.ExecContext(ctx, `UPDATE uploads SET status='completed' WHERE id=? AND status='pending'`, u.ID)
	}
	if err != nil {
		if tx != nil {
			tx.Rollback()
		}
		return err
	}
	if err = tx.Commit(); err != nil {
		return err
	}
	return nil
}
func (s *Server) abortUpload(w http.ResponseWriter, r *http.Request) {
	u, err := s.upload(r.Context(), chi.URLParam(r, "id"))
	if err != nil || u.Status != "pending" {
		problem(w, 404, "pending upload not found")
		return
	}
	if err := s.cleanupPendingUploadObject(r.Context(), u); err != nil {
		s.log.Warn("blob upload object cleanup failed", "upload", u.ID, "object_key", u.ObjectKey, "error", err)
	}
	tx, err := s.db.BeginTx(r.Context(), nil)
	if err == nil {
		_, err = tx.ExecContext(r.Context(), `UPDATE uploads SET status='aborted' WHERE id=?`, u.ID)
	}
	if err == nil {
		_, err = tx.ExecContext(r.Context(), `DELETE FROM files WHERE id=? AND status='pending'`, u.FileID)
	}
	if err != nil {
		if tx != nil {
			tx.Rollback()
		}
		problem(w, 500, "could not clean upload metadata")
		return
	}
	if err = tx.Commit(); err != nil {
		problem(w, 500, "could not clean upload metadata")
		return
	}
	w.WriteHeader(204)
}

// cleanupPendingUploadObject handles both an active multipart upload and the
// less common case where CompleteMultipart succeeded but SQLite finalization
// did not. Deleting the key after abort is safe when no object was committed.
func (s *Server) cleanupPendingUploadObject(ctx context.Context, u uploadRecord) error {
	if u.Mode == "multipart" && u.S3UploadID != "" {
		if err := s.storage.AbortMultipart(ctx, u.ObjectKey, u.S3UploadID); err != nil {
			s.log.Debug("multipart abort skipped", "upload", u.ID, "error", err)
		}
	}
	return s.storage.DeleteObject(ctx, u.ObjectKey)
}

func (s *Server) failUpload(ctx context.Context, uploadID, fileID string) {
	_, _ = s.db.ExecContext(ctx, `UPDATE uploads SET status='failed' WHERE id=?`, uploadID)
	_, _ = s.db.ExecContext(ctx, `UPDATE files SET status='failed',updated_at=? WHERE id=?`, time.Now().UTC().Format(time.RFC3339Nano), fileID)
}

func (s *Server) CleanupExpiredUploads(ctx context.Context) {
	rows, err := s.db.QueryContext(ctx, `SELECT id FROM uploads WHERE status='pending' AND expires_at<=?`, time.Now().UTC().Format(time.RFC3339Nano))
	if err != nil {
		s.log.Error("scan stale uploads failed", "error", err)
		return
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
		u, err := s.upload(ctx, id)
		if err != nil {
			continue
		}
		err = s.cleanupPendingUploadObject(ctx, u)
		if err != nil {
			s.log.Warn("stale blob upload cleanup failed", "upload", id, "error", err)
			continue
		}
		tx, err := s.db.BeginTx(ctx, nil)
		if err != nil {
			continue
		}
		_, err = tx.ExecContext(ctx, `UPDATE uploads SET status='aborted' WHERE id=?`, id)
		if err == nil {
			_, err = tx.ExecContext(ctx, `DELETE FROM files WHERE id=? AND status='pending'`, u.FileID)
		}
		if err == nil {
			err = tx.Commit()
		} else {
			tx.Rollback()
		}
		if err == nil {
			s.log.Info("stale upload cleaned", "upload", id)
		}
	}
}

// parallel runs fn over every index concurrently, bounded by limit
// workers, and returns the first error (other workers finish their
// in-flight item before the function returns).
func parallel(indices []int, limit int, fn func(int) error) error {
	if len(indices) == 0 {
		return nil
	}
	if limit < 1 {
		limit = 1
	}
	if limit > len(indices) {
		limit = len(indices)
	}
	queue := make(chan int)
	var wg sync.WaitGroup
	var mu sync.Mutex
	var firstErr error
	for i := 0; i < limit; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for idx := range queue {
				if err := fn(idx); err != nil {
					mu.Lock()
					if firstErr == nil {
						firstErr = err
					}
					mu.Unlock()
				}
			}
		}()
	}
	for _, idx := range indices {
		queue <- idx
	}
	close(queue)
	wg.Wait()
	return firstErr
}

// referencedStorageKeys returns every content object and derived thumbnail the
// metadata can still reach, across active and trashed file states.
func (s *Server) referencedStorageKeys(ctx context.Context) (map[string]bool, map[string]bool, error) {
	rows, err := s.db.QueryContext(ctx, `SELECT object_key,name FROM files WHERE kind='file' AND object_key IS NOT NULL AND object_key <> ''`)
	if err != nil {
		return nil, nil, err
	}
	defer rows.Close()
	objects := map[string]bool{}
	thumbnails := map[string]bool{}
	for rows.Next() {
		var key, name string
		if err := rows.Scan(&key, &name); err != nil {
			return nil, nil, err
		}
		objects[key] = true
		if canHaveThumbnail(name) {
			thumbnails[thumbnailKey(key)] = true
		}
	}
	if err := rows.Err(); err != nil {
		return nil, nil, err
	}
	if err := rows.Close(); err != nil {
		return nil, nil, err
	}
	mediaRows, err := s.db.QueryContext(ctx, `SELECT am.stream_object_key,f.object_key,am.has_cover FROM audio_media am JOIN files f ON f.id=am.file_id`)
	if err != nil {
		return nil, nil, err
	}
	defer mediaRows.Close()
	for mediaRows.Next() {
		var streamKey, masterKey string
		var hasCover bool
		if err := mediaRows.Scan(&streamKey, &masterKey, &hasCover); err != nil {
			return nil, nil, err
		}
		objects[streamKey] = true
		if hasCover {
			thumbnails[thumbnailKey(masterKey)] = true
		}
	}
	if err := mediaRows.Err(); err != nil {
		return nil, nil, err
	}
	return objects, thumbnails, nil
}

// CollectGarbage deletes unreferenced blobs and derived thumbnails after the
// upload grace period.
func (s *Server) CollectGarbage(ctx context.Context) {
	cutoff := time.Now().UTC().Add(-(s.cfg.UploadExpires + time.Hour))
	referenced, referencedThumbnails, err := s.referencedStorageKeys(ctx)
	if err != nil {
		s.log.Error("GC metadata scan failed", "error", err)
		return
	}
	deletedBlobs := s.collectUnreferencedPrefix(ctx, "blobs/", cutoff, referenced)
	deletedThumbnails := s.collectUnreferencedPrefix(ctx, "thumbs/", cutoff, referencedThumbnails)
	if deletedBlobs+deletedThumbnails > 0 {
		s.log.Info("garbage collection finished", "blobs", deletedBlobs, "thumbnails", deletedThumbnails)
	}
}

func (s *Server) collectUnreferencedPrefix(ctx context.Context, prefix string, cutoff time.Time, referenced map[string]bool) int {
	deleted := 0
	err := s.storage.WalkPrefix(ctx, prefix, func(objects []storage.ObjectRef) error {
		keys := make([]string, 0, len(objects))
		for _, object := range objects {
			if !referenced[object.Key] && object.LastModified.Before(cutoff) {
				keys = append(keys, object.Key)
			}
		}
		for len(keys) > 0 {
			n := min(len(keys), 1000)
			if err := s.storage.DeleteObjects(ctx, keys[:n]); err != nil {
				return err
			}
			deleted += n
			keys = keys[n:]
		}
		return ctx.Err()
	})
	if err != nil && !errors.Is(err, context.Canceled) {
		s.log.Warn("GC streaming pass failed", "prefix", prefix, "error", err)
	}
	return deleted
}

func validateName(name string) error {
	if name == "" || name == "." || name == ".." {
		return errors.New("invalid name")
	}
	if strings.TrimSpace(name) != name {
		return errors.New("name cannot start or end with whitespace")
	}
	if len(name) > 1024 || utf8.RuneCountInString(name) > 255 {
		return errors.New("name is too long")
	}
	if strings.ContainsAny(name, "/\\") {
		return errors.New("name cannot contain path separators")
	}
	for _, r := range name {
		if r < 32 || r == 127 {
			return errors.New("name contains control characters")
		}
	}
	return nil
}
func isPreviewable(f File) bool {
	mimeType := responseMime(f)
	if strings.HasPrefix(mimeType, "video/") || strings.HasPrefix(mimeType, "audio/") {
		return true
	}
	switch mimeType {
	case "image/jpeg", "image/png", "image/webp", "image/gif", "image/avif":
		return true
	default:
		return false
	}
}
func isConflict(err error) bool {
	return err != nil && (strings.Contains(err.Error(), "UNIQUE constraint failed") || strings.Contains(err.Error(), "constraint failed"))
}
func decodeJSON(w http.ResponseWriter, r *http.Request, dst any) error {
	return decodeJSONLimit(w, r, dst, maxJSONBody)
}
func decodeJSONLimit(w http.ResponseWriter, r *http.Request, dst any, limit int64) error {
	r.Body = http.MaxBytesReader(w, r.Body, limit)
	dec := json.NewDecoder(r.Body)
	dec.DisallowUnknownFields()
	if err := dec.Decode(dst); err != nil {
		problem(w, 400, "invalid JSON request")
		return err
	}
	if err := dec.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		problem(w, 400, "request must contain one JSON value")
		return errors.New("multiple JSON values")
	}
	return nil
}
func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}
func problem(w http.ResponseWriter, status int, message string) {
	writeJSON(w, status, map[string]any{"error": map[string]any{"status": status, "message": message}})
}
func problemCode(w http.ResponseWriter, status int, code, message string) {
	writeJSON(w, status, map[string]any{"error": map[string]any{"status": status, "code": code, "message": message}})
}

type loginAttempt struct {
	count int
	reset time.Time
}
type loginLimiter struct {
	mu       sync.Mutex
	attempts map[string]loginAttempt
	global   loginAttempt
	slots    chan struct{}
}

func newLoginLimiter() *loginLimiter {
	return &loginLimiter{attempts: map[string]loginAttempt{}, slots: make(chan struct{}, 2)}
}
func (l *loginLimiter) allow(ip string) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	now := time.Now()
	if now.After(l.global.reset) {
		l.global = loginAttempt{}
	}
	if l.global.count >= 30 {
		return false
	}
	a := l.attempts[ip]
	if now.After(a.reset) {
		delete(l.attempts, ip)
		return true
	}
	return a.count < 5
}
func (l *loginLimiter) fail(ip string) {
	l.mu.Lock()
	defer l.mu.Unlock()
	now := time.Now()
	a := l.attempts[ip]
	if now.After(a.reset) {
		a = loginAttempt{reset: now.Add(15 * time.Minute)}
	}
	a.count++
	l.attempts[ip] = a
	if now.After(l.global.reset) {
		l.global = loginAttempt{reset: now.Add(15 * time.Minute)}
	}
	l.global.count++
}
func (l *loginLimiter) success(ip string) { l.mu.Lock(); delete(l.attempts, ip); l.mu.Unlock() }
func (l *loginLimiter) acquire() bool {
	select {
	case l.slots <- struct{}{}:
		return true
	default:
		return false
	}
}
func (l *loginLimiter) release() { <-l.slots }
