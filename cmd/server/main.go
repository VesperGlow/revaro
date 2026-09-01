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
	"github.com/VesperGlow/revaro/internal/dataplane"
	"github.com/VesperGlow/revaro/internal/server"
	"github.com/VesperGlow/revaro/internal/storage"
)

func main() {
	log := slog.New(slog.NewTextHandler(os.Stdout, &slog.HandlerOptions{Level: slog.LevelInfo}))
	exitCode := 0
	// This defer is registered before resource-owning defers below, so those
	// always run before a non-zero process exit.
	defer func() {
		if exitCode != 0 {
			os.Exit(exitCode)
		}
	}()
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
	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()
	if err := ensureWorkDir(cfg.WorkDir); err != nil {
		log.Error("work directory startup check failed", "path", cfg.WorkDir, "error", err)
		os.Exit(1)
	}
	dataProcess, err := dataplane.Start(ctx, cfg.DataPlaneBinary, cfg.DataPlaneAddr, log)
	if err != nil {
		log.Error("data plane startup failed", "error", err)
		exitCode = 1
		return
	}
	defer dataProcess.Close()
	db, err := database.Open(cfg.DatabasePath())
	if err != nil {
		log.Error("database startup failed", "error", err)
		exitCode = 1
		return
	}
	defer db.Close()
	log.Info("database ready", "path", cfg.DatabasePath())
	authService := &auth.Service{DB: db}
	initialCredentials, err := authService.Initialize(ctx, cfg.AdminUsername, cfg.AdminPassword)
	if err != nil {
		log.Error("administrator initialization failed", "error", err)
		exitCode = 1
		return
	}
	store := storage.NewDataPlane(dataProcess.Addr(), dataProcess.Token())
	checkCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	err = store.Ping(checkCtx)
	cancel()
	if err != nil {
		log.Error("S3 connection check failed", "bucket", cfg.S3Bucket, "error", err)
		exitCode = 1
		return
	}
	provider := "s3"
	if cfg.IsUpCloud() {
		provider = "upcloud"
	}
	log.Info("S3 connection ready", "bucket", cfg.S3Bucket, "provider", provider, "proxy_transfers", cfg.ProxyTransfers)
	app := server.New(db, store, authService, cfg, log)
	defer app.Close()
	app.RegisterCleanup("auth", 15*time.Minute, 5*time.Minute, true, func(cleanupCtx context.Context) error { authService.Cleanup(cleanupCtx); return nil })
	// Streaming downloads can legitimately run for much longer than a fixed
	// response deadline. Upload/read deadlines are enforced at the handler and
	// body-limit layers instead.
	httpServer := &http.Server{Addr: cfg.Addr, Handler: app.Handler(), ReadHeaderTimeout: 10 * time.Second, ReadTimeout: 30 * time.Second, WriteTimeout: 0, IdleTimeout: 120 * time.Second, MaxHeaderBytes: 1 << 20}
	listener, err := net.Listen("tcp", cfg.Addr)
	if err != nil {
		log.Error("server listen failed", "addr", cfg.Addr, "error", err)
		exitCode = 1
		return
	}
	log.Info("server started", "addr", cfg.Addr)
	if initialCredentials.Generated {
		path, err := writeCredentials(cfg.DataDir, initialCredentials)
		if err != nil {
			log.Error("could not securely write initial administrator credentials", "error", err)
			exitCode = 1
			return
		}
		log.Warn("initial administrator credentials written to a mode-0600 file; sign in, change them, then remove the file", "path", path)
	} else if initialCredentials.Created {
		log.Info("administrator initialized from environment", "username", initialCredentials.Username)
	}
	serverErr := make(chan error, 1)
	go func() { serverErr <- httpServer.Serve(listener) }()
	select {
	case <-ctx.Done():
	case err := <-serverErr:
		if err != nil && !errors.Is(err, http.ErrServerClosed) {
			log.Error("server stopped unexpectedly", "error", err)
			exitCode = 1
		}
		stop()
	case childErr := <-dataProcess.Done():
		log.Error("Rust data plane stopped unexpectedly", "error", childErr)
		exitCode = 1
		stop()
	}
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
		_ = db.Close()
		os.Exit(1)
	}
	path, err := writeCredentials(dataDir, credentials)
	if err != nil {
		log.Error("could not securely write reset administrator credentials", "error", err)
		_ = db.Close()
		os.Exit(1)
	}
	log.Warn("administrator credentials reset and written to a mode-0600 file; remove it after signing in", "path", path)
}

func ensureWorkDir(path string) error {
	if err := os.MkdirAll(path, 0o700); err != nil {
		return err
	}
	probe, err := os.CreateTemp(path, ".revaro-write-check-*")
	if err != nil {
		return err
	}
	name := probe.Name()
	if err := probe.Close(); err != nil {
		_ = os.Remove(name)
		return err
	}
	return os.Remove(name)
}

func writeCredentials(dataDir string, credentials auth.InitialCredentials) (string, error) {
	path := filepath.Join(dataDir, "initial-admin-credentials")
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0o600)
	if err != nil {
		return "", err
	}
	if err := file.Chmod(0o600); err != nil {
		file.Close()
		return "", err
	}
	_, writeErr := file.WriteString("username=" + credentials.Username + "\npassword=" + credentials.Password + "\n")
	closeErr := file.Close()
	if writeErr != nil {
		return "", writeErr
	}
	return path, closeErr
}
