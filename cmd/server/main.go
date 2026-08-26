package main

import (
	"context"
	"errors"
	"log/slog"
	"net"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"
	"time"

	"github.com/VesperGlow/revaro/internal/auth"
	"github.com/VesperGlow/revaro/internal/config"
	"github.com/VesperGlow/revaro/internal/database"
	"github.com/VesperGlow/revaro/internal/server"
	"github.com/VesperGlow/revaro/internal/storage"
)

func main() {
	log := slog.New(slog.NewTextHandler(os.Stdout, &slog.HandlerOptions{Level: slog.LevelInfo}))
	if len(os.Args) > 1 {
		if os.Args[1] == "reset-admin" {
			resetAdministrator(log)
			return
		}
		log.Error("unknown command", "command", os.Args[1])
		os.Exit(2)
	}
	cfg, err := config.Load()
	if err != nil {
		log.Error("configuration invalid", "error", err)
		os.Exit(1)
	}
	db, err := database.Open(cfg.DatabasePath())
	if err != nil {
		log.Error("database startup failed", "error", err)
		os.Exit(1)
	}
	defer db.Close()
	log.Info("database ready", "path", cfg.DatabasePath())
	authService := &auth.Service{DB: db}
	initialCredentials, err := authService.Initialize(context.Background(), cfg.AdminUsername, cfg.AdminPassword)
	if err != nil {
		log.Error("administrator initialization failed", "error", err)
		os.Exit(1)
	}
	store, err := storage.NewS3WithDB(context.Background(), cfg, db)
	if err != nil {
		log.Error("S3 client initialization failed", "error", err)
		os.Exit(1)
	}
	checkCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	err = store.Ping(checkCtx)
	cancel()
	if err != nil {
		log.Error("S3 connection check failed", "bucket", cfg.S3Bucket, "error", err)
		os.Exit(1)
	}
	provider := "s3"
	if cfg.IsUpCloud() {
		provider = "upcloud"
	}
	log.Info("S3 connection ready", "bucket", cfg.S3Bucket, "provider", provider, "proxy_transfers", cfg.ProxyTransfers)
	app := server.New(db, store, authService, cfg, log)
	defer app.Close()
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()
	// Old manifests remain readable while one low-priority pass collapses them
	// into opaque whole-file blobs. Startup and normal requests never wait for it.
	go func() {
		if migrated, err := app.MigrateLegacyObjects(ctx); err != nil && ctx.Err() == nil {
			log.Error("legacy FastCDC migration paused; it will resume on the next start", "error", err)
		} else if err == nil {
			log.Info("legacy FastCDC migration complete remaining=0", "migrated", migrated)
		}
	}()
	gcRequests := make(chan struct{}, 1)
	requestGC := func() {
		select {
		case gcRequests <- struct{}{}:
		default: // a pending request already guarantees another pass
		}
	}
	go func() {
		var ticker *time.Ticker
		var ticks <-chan time.Time
		if cfg.GCInterval > 0 {
			ticker = time.NewTicker(cfg.GCInterval)
			ticks = ticker.C
			defer ticker.Stop()
		}
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticks:
				app.CollectGarbage(context.Background())
			case <-gcRequests:
				app.CollectGarbage(context.Background())
			}
		}
	}()
	runHousekeeping := func() {
		app.CleanupExpiredUploads(context.Background())
		if app.CleanupExpiredTrash(context.Background()) > 0 {
			// Expired trash must release its content even when periodic orphan
			// collection is disabled with GC_INTERVAL=0.
			requestGC()
		}
		authService.Cleanup(context.Background())
	}
	runHousekeeping()
	if cfg.GCInterval > 0 {
		requestGC() // one initial pass, then the configured interval
	}
	go func() {
		ticker := time.NewTicker(15 * time.Minute)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				runHousekeeping()
			}
		}
	}()
	// Streaming downloads can legitimately run for much longer than a fixed
	// response deadline. Upload/read deadlines are enforced at the handler and
	// body-limit layers instead.
	httpServer := &http.Server{Addr: cfg.Addr, Handler: app.Handler(), ReadHeaderTimeout: 10 * time.Second, ReadTimeout: 30 * time.Second, WriteTimeout: 0, IdleTimeout: 120 * time.Second, MaxHeaderBytes: 1 << 20}
	listener, err := net.Listen("tcp", cfg.Addr)
	if err != nil {
		log.Error("server listen failed", "addr", cfg.Addr, "error", err)
		os.Exit(1)
	}
	log.Info("server started", "addr", cfg.Addr)
	if initialCredentials.Generated {
		log.Warn("initial administrator credentials; shown once, sign in and change them immediately", "username", initialCredentials.Username, "password", initialCredentials.Password)
	} else if initialCredentials.Created {
		log.Info("administrator initialized from environment", "username", initialCredentials.Username)
	}
	go func() {
		if err := httpServer.Serve(listener); err != nil && !errors.Is(err, http.ErrServerClosed) {
			log.Error("server stopped unexpectedly", "error", err)
			os.Exit(1)
		}
	}()
	<-ctx.Done()
	shutdownCtx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	if err := httpServer.Shutdown(shutdownCtx); err != nil {
		log.Error("graceful shutdown failed", "error", err)
	} else {
		log.Info("server stopped")
	}
}

func resetAdministrator(log *slog.Logger) {
	dataDir := os.Getenv("APP_DATA_DIR")
	if dataDir == "" {
		dataDir = "/data"
	}
	db, err := database.Open(filepath.Join(dataDir, "revaro.db"))
	if err != nil {
		log.Error("database startup failed", "error", err)
		os.Exit(1)
	}
	defer db.Close()
	username := os.Getenv("ADMIN_USERNAME")
	if len(os.Args) > 2 {
		username = os.Args[2]
	}
	credentials, err := (&auth.Service{DB: db}).ResetCredentials(context.Background(), username)
	if err != nil {
		log.Error("administrator reset failed", "error", err)
		os.Exit(1)
	}
	log.Warn("administrator credentials reset; shown once, sign in and change them immediately", "username", credentials.Username, "password", credentials.Password)
}
