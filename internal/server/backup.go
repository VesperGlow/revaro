package server

import (
	"context"
	"fmt"
	"os"
	"slices"
	"strings"
	"time"
)

const (
	// backupObjectPrefix is the dedicated S3 key namespace for database
	// snapshots; it never mixes with blobs/, manifests/ or profile/ keys.
	backupObjectPrefix = "revaro-backups/database"
	// backupSnapshotNameLayout encodes a UTC timestamp inside every backup
	// key, so lexicographic key order matches chronological order.
	backupSnapshotNameLayout = "20060102T150405Z"
	backupSnapshotFilePrefix = "revaro-db-"
	backupSnapshotFileSuffix = ".sqlite"
	backupSnapshotMIME       = "application/x-sqlite3"
	// backupStagingPattern names WorkDir scratch files while a snapshot is
	// taken and uploaded; startup cleanup removes crash leftovers.
	backupStagingPattern = "revaro-db-backup-*.sqlite"
)

type databaseBackupRef struct {
	key string
	at  time.Time
}

// databaseBackupObjectKey returns the S3 key for a snapshot taken at when.
func databaseBackupObjectKey(when time.Time) string {
	return backupObjectPrefix + "/" + backupSnapshotFilePrefix + when.UTC().Format(backupSnapshotNameLayout) + backupSnapshotFileSuffix
}

// parseDatabaseBackupKey extracts the snapshot time encoded in a backup key.
// Keys outside the backup namespace or with malformed timestamps are ignored
// so foreign objects in the prefix can never break scheduling or retention.
func parseDatabaseBackupKey(key string) (time.Time, bool) {
	name, ok := strings.CutPrefix(key, backupObjectPrefix+"/")
	if !ok {
		return time.Time{}, false
	}
	name, ok = strings.CutSuffix(name, backupSnapshotFileSuffix)
	if !ok {
		return time.Time{}, false
	}
	stamp, ok := strings.CutPrefix(name, backupSnapshotFilePrefix)
	if !ok {
		return time.Time{}, false
	}
	when, err := time.Parse(backupSnapshotNameLayout, stamp)
	if err != nil {
		return time.Time{}, false
	}
	return when.UTC(), true
}

// startDatabaseBackups owns the periodic database backup loop. It never runs
// inside the CleanupManager because a slow snapshot upload would starve the
// other cleanup jobs, which execute sequentially on one goroutine. Failures
// are logged and retried with a bounded backoff; they never affect request
// serving.
func (s *Server) startDatabaseBackups() {
	if !s.cfg.BackupEnabled || s.cfg.BackupInterval <= 0 {
		return
	}
	ctx, cancel := context.WithCancel(context.Background())
	s.backupCancel = cancel
	s.runBackground(func() {
		failures := 0
		timer := time.NewTimer(0) // first pass at startup catches missed schedules
		defer timer.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-timer.C:
			}
			delay := s.cfg.BackupInterval
			if err := s.runDatabaseBackup(ctx); err != nil {
				if ctx.Err() != nil {
					return
				}
				failures++
				delay = min(time.Duration(1<<min(failures, 6))*time.Minute, s.cfg.BackupInterval)
				s.log.Warn("database backup failed; service keeps running", "retry_in", delay.String(), "error", err)
			} else {
				failures = 0
			}
			timer.Reset(delay)
		}
	})
}

// runDatabaseBackup performs one scheduler pass: it uploads a new consistent
// snapshot only when the newest stored backup is older than the configured
// interval, so restarts never flood the backup prefix with duplicates.
func (s *Server) runDatabaseBackup(ctx context.Context) error {
	if !s.cfg.BackupEnabled {
		return nil
	}
	backups, err := s.listDatabaseBackups(ctx)
	if err != nil {
		return err
	}
	if len(backups) > 0 && time.Since(backups[0].at) < s.cfg.BackupInterval {
		return nil
	}
	return s.createDatabaseBackup(ctx)
}

// createDatabaseBackup snapshots the live database with VACUUM INTO — SQLite's
// own transactionally consistent snapshot, taken through the database engine
// instead of copying hot files — streams it to the backup prefix, and prunes
// older snapshots down to the retention limit.
func (s *Server) createDatabaseBackup(ctx context.Context) error {
	if err := os.MkdirAll(s.cfg.WorkDir, 0o700); err != nil {
		return fmt.Errorf("database backup work directory: %w", err)
	}
	// VACUUM INTO requires the target file not to exist, so reserve a unique
	// staging name first. The defer guarantees the local copy is removed once
	// the upload attempt finishes, regardless of its outcome.
	reserved, err := os.CreateTemp(s.cfg.WorkDir, backupStagingPattern)
	if err != nil {
		return fmt.Errorf("reserve database backup staging file: %w", err)
	}
	staging := reserved.Name()
	reserved.Close()
	defer os.Remove(staging)
	if err := os.Remove(staging); err != nil {
		return fmt.Errorf("clear database backup staging file: %w", err)
	}
	if _, err := s.db.ExecContext(ctx, `VACUUM INTO ?`, staging); err != nil {
		return fmt.Errorf("sqlite snapshot via VACUUM INTO: %w", err)
	}
	file, err := os.Open(staging)
	if err != nil {
		return fmt.Errorf("open database snapshot: %w", err)
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return fmt.Errorf("stat database snapshot: %w", err)
	}
	key := databaseBackupObjectKey(time.Now())
	if _, err := s.storage.StoreBlob(ctx, key, backupSnapshotMIME, file, info.Size()); err != nil {
		return fmt.Errorf("upload database backup %s: %w", key, err)
	}
	s.log.Info("database backup uploaded", "key", key, "bytes", info.Size())
	if err := s.pruneDatabaseBackups(ctx); err != nil {
		// The new snapshot is safe; retention catches up on a later pass.
		s.log.Warn("database backup retention cleanup failed", "kept_minimum", s.cfg.BackupRetention, "error", err)
		return err
	}
	return nil
}

func (s *Server) listDatabaseBackups(ctx context.Context) ([]databaseBackupRef, error) {
	refs, err := s.objects.ListPrefix(ctx, backupObjectPrefix+"/")
	if err != nil {
		return nil, fmt.Errorf("list database backups: %w", err)
	}
	backups := make([]databaseBackupRef, 0, len(refs))
	for _, ref := range refs {
		if at, ok := parseDatabaseBackupKey(ref.Key); ok {
			backups = append(backups, databaseBackupRef{key: ref.Key, at: at})
		}
	}
	slices.SortFunc(backups, func(a, b databaseBackupRef) int { return b.at.Compare(a.at) })
	return backups, nil
}

// pruneDatabaseBackups deletes the oldest snapshots beyond the retention
// limit. Failed deletions are re-queued by the object manager's durable
// object-cleanup path.
func (s *Server) pruneDatabaseBackups(ctx context.Context) error {
	backups, err := s.listDatabaseBackups(ctx)
	if err != nil {
		return err
	}
	if len(backups) <= s.cfg.BackupRetention {
		return nil
	}
	stale := backups[s.cfg.BackupRetention:]
	keys := make([]string, 0, len(stale))
	for _, backup := range stale {
		keys = append(keys, backup.key)
	}
	if err := s.objects.DeleteMany(ctx, keys, "database-backup-retention"); err != nil {
		return fmt.Errorf("delete %d expired database backups: %w", len(keys), err)
	}
	s.log.Info("database backup retention applied", "removed", len(keys), "kept", s.cfg.BackupRetention)
	return nil
}
