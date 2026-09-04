package server

import (
	"bytes"
	"context"
	"database/sql"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sync/atomic"
	"testing"
	"time"
)

func (a *testApp) enableBackups(interval time.Duration, retention int) {
	a.srv.cfg.BackupEnabled = true
	a.srv.cfg.BackupInterval = interval
	a.srv.cfg.BackupRetention = retention
}

func (a *testApp) seedBackup(key string, at time.Time, content []byte) {
	a.store.mu.Lock()
	defer a.store.mu.Unlock()
	a.store.raw[key] = content
	a.store.modified[key] = at
}

func (a *testApp) backupKeys() []string {
	refs, err := a.srv.listDatabaseBackups(context.Background())
	if err != nil {
		a.t.Fatalf("list database backups: %v", err)
	}
	keys := make([]string, 0, len(refs))
	for _, backup := range refs {
		keys = append(keys, backup.key)
	}
	return keys
}

func (a *testApp) requireNoStagingSnapshots() {
	t := a.t
	t.Helper()
	leftovers, err := filepath.Glob(filepath.Join(a.srv.cfg.WorkDir, backupStagingPattern))
	if err != nil {
		t.Fatal(err)
	}
	if len(leftovers) != 0 {
		t.Fatalf("staging snapshot files left behind: %v", leftovers)
	}
}

func TestDatabaseBackupObjectKeyRoundTrip(t *testing.T) {
	at := time.Date(2026, 9, 2, 10, 30, 5, 0, time.UTC)
	key := databaseBackupObjectKey(at)
	if key != "revaro-backups/database/revaro-db-20260902T103005Z.sqlite" {
		t.Fatalf("unexpected backup key %q", key)
	}
	parsed, ok := parseDatabaseBackupKey(key)
	if !ok || !parsed.Equal(at) {
		t.Fatalf("round trip = %v, %v", parsed, ok)
	}
	for _, foreign := range []string{
		"blobs/00000000-0000-0000-0000-000000000000",
		"profile/avatar",
		"revaro-db-20260902T103005Z.sqlite",
		"revaro-backups/database/revaro-db-20260902T103005Z",
		"revaro-backups/database/foreign-object",
		"revaro-backups/database/revaro-db-notatime.sqlite",
		"revaro-backups/database/revaro-db-2026-09-02T10:30:05Z.sqlite",
	} {
		if _, ok := parseDatabaseBackupKey(foreign); ok {
			t.Fatalf("key %q should not parse as a backup", foreign)
		}
	}
}

func TestDatabaseBackupUploadsSnapshotAndPrunesRetention(t *testing.T) {
	app := newTestApp(t)
	app.enableBackups(24*time.Hour, 3)
	now := time.Now().UTC()
	for _, offset := range []time.Duration{-96 * time.Hour, -72 * time.Hour, -48 * time.Hour} {
		app.seedBackup(databaseBackupObjectKey(now.Add(offset)), now.Add(offset), []byte("old snapshot"))
	}
	if err := app.srv.runDatabaseBackup(context.Background()); err != nil {
		t.Fatalf("run backup: %v", err)
	}
	keys := app.backupKeys()
	if len(keys) != 3 {
		t.Fatalf("backup count = %d, want retention 3: %v", len(keys), keys)
	}
	newest := keys[0]
	created, ok := parseDatabaseBackupKey(newest)
	if !ok || time.Since(created) > 5*time.Minute {
		t.Fatalf("newest backup timestamp %v not recent (key %q)", created, newest)
	}
	if _, pruned := app.store.raw[databaseBackupObjectKey(now.Add(-96*time.Hour))]; pruned {
		t.Fatal("oldest backup outside retention was not pruned")
	}
	// 上传对象必须是合法的 SQLite 数据库（引擎生成的一致性快照），而非热文件拷贝。
	raw := app.store.raw[newest]
	if !bytes.HasPrefix(raw, []byte("SQLite format 3\x00")) {
		t.Fatalf("uploaded object is not a SQLite database: %x", raw[:16])
	}
	if app.store.rawMime[newest] != backupSnapshotMIME {
		t.Fatalf("backup mime = %q", app.store.rawMime[newest])
	}
	snapshotPath := filepath.Join(t.TempDir(), "snap.sqlite")
	if err := os.WriteFile(snapshotPath, raw, 0o600); err != nil {
		t.Fatal(err)
	}
	snapshot, err := sql.Open("sqlite", "file:"+snapshotPath+"?mode=ro")
	if err != nil {
		t.Fatal(err)
	}
	defer snapshot.Close()
	var tableCount int
	if err := snapshot.QueryRow(`SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='files'`).Scan(&tableCount); err != nil {
		t.Fatalf("snapshot is not a readable database: %v", err)
	}
	if tableCount != 1 {
		t.Fatal("snapshot is missing the migrated files table")
	}
	app.requireNoStagingSnapshots()
}

// TestDatabaseBackupCatchUpIsSingleSnapshot 锁定启动补备份语义：服务停机跨过
// 多个备份周期后，第一轮调度只为最新的数据库状态补做一次备份，之后的轮次因
// 快照新鲜而跳过，绝不为每个错过的周期分别补做。
func TestDatabaseBackupCatchUpIsSingleSnapshot(t *testing.T) {
	app := newTestApp(t)
	app.enableBackups(24*time.Hour, 10)
	now := time.Now().UTC()
	for _, offset := range []time.Duration{-100 * time.Hour, -76 * time.Hour, -52 * time.Hour} {
		app.seedBackup(databaseBackupObjectKey(now.Add(offset)), now.Add(offset), []byte("old snapshot"))
	}
	before := len(app.backupKeys())
	if err := app.srv.runDatabaseBackup(context.Background()); err != nil {
		t.Fatalf("catch-up backup: %v", err)
	}
	keys := app.backupKeys()
	if len(keys) != before+1 {
		t.Fatalf("catch-up produced %d new backups, want exactly 1: %v", len(keys)-before, keys)
	}
	fresh := 0
	for _, key := range keys {
		if at, ok := parseDatabaseBackupKey(key); ok && time.Since(at) < 24*time.Hour {
			fresh++
		}
	}
	if fresh != 1 {
		t.Fatalf("fresh catch-up snapshots = %d, want 1", fresh)
	}
	// 第二轮必须被新鲜快照短路：不会继续为已错过的历史周期补做。
	if err := app.srv.runDatabaseBackup(context.Background()); err != nil {
		t.Fatalf("second pass: %v", err)
	}
	if keys := app.backupKeys(); len(keys) != before+1 {
		t.Fatalf("second pass changed backup count: %v", keys)
	}
}

// TestDatabaseBackupVacuumCoexistsWithConcurrentWrites 验证 VACUUM INTO 在
// 生产 DSN（WAL + busy_timeout）下的并发行为：快照期间持续写入既不失败也不
// 被 5 秒 busy_timeout 拒绝，产出的是某个时间点的完整一致副本。
func TestDatabaseBackupVacuumCoexistsWithConcurrentWrites(t *testing.T) {
	app := newTestApp(t)
	app.enableBackups(24*time.Hour, 5)

	stop := make(chan struct{})
	writerErr := make(chan error, 1)
	var inserts atomic.Int64
	go func() {
		for i := 0; ; i++ {
			select {
			case <-stop:
				close(writerErr)
				return
			default:
			}
			id := fmt.Sprintf("storm-%d", i)
			if _, err := app.db.Exec(`INSERT INTO files(id,parent_id,name,kind,object_key,status,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?)`,
				id, RootID, "vacuum-storm-"+id, "directory", nil, "ready", time.Now().UTC().Format(time.RFC3339Nano), time.Now().UTC().Format(time.RFC3339Nano)); err != nil {
				writerErr <- fmt.Errorf("write %d failed while snapshot runs: %w", i, err)
				close(writerErr)
				return
			}
			inserts.Add(1)
		}
	}()

	for inserts.Load() == 0 {
		time.Sleep(time.Millisecond)
	}
	writesBefore := inserts.Load()
	if err := app.srv.runDatabaseBackup(context.Background()); err != nil {
		close(stop)
		t.Fatalf("backup with concurrent writes: %v", err)
	}
	writesDuring := inserts.Load() - writesBefore
	time.Sleep(20 * time.Millisecond)
	close(stop)
	if err := <-writerErr; err != nil {
		t.Fatalf("concurrent writer failed under WAL snapshot: %v", err)
	}
	if writesDuring <= 0 {
		t.Fatal("no writes committed while the snapshot was taken; VACUUM INTO appears to block writers")
	}

	raw := app.store.raw[app.backupKeys()[0]]
	snapshotPath := filepath.Join(t.TempDir(), "snap.sqlite")
	if err := os.WriteFile(snapshotPath, raw, 0o600); err != nil {
		t.Fatal(err)
	}
	snapshot, err := sql.Open("sqlite", "file:"+snapshotPath+"?mode=ro")
	if err != nil {
		t.Fatal(err)
	}
	defer snapshot.Close()
	var integrity string
	if err := snapshot.QueryRow(`PRAGMA integrity_check`).Scan(&integrity); err != nil || integrity != "ok" {
		t.Fatalf("snapshot integrity = %q, err %v", integrity, err)
	}
	var rows int64
	if err := snapshot.QueryRow(`SELECT COUNT(*) FROM files WHERE name LIKE 'vacuum-storm-%'`).Scan(&rows); err != nil {
		t.Fatal(err)
	}
	// 快照行数下限 = 备份开始前已提交的写入；上限 = 备份返回时的全部写入。
	// 两条界同时成立说明快照没有撕裂。
	if rows < writesBefore {
		t.Fatalf("snapshot rows %d < committed-before %d; snapshot is torn", rows, writesBefore)
	}
	if rows > inserts.Load() {
		t.Fatalf("snapshot rows %d exceed total committed writes %d", rows, inserts.Load())
	}
}

func TestDatabaseBackupSkipsFreshSnapshot(t *testing.T) {
	app := newTestApp(t)
	app.enableBackups(24*time.Hour, 5)
	recent := time.Now().UTC().Add(-time.Hour)
	app.seedBackup(databaseBackupObjectKey(recent), recent, []byte("fresh"))
	if err := app.srv.runDatabaseBackup(context.Background()); err != nil {
		t.Fatalf("run backup: %v", err)
	}
	if keys := app.backupKeys(); len(keys) != 1 {
		t.Fatalf("fresh snapshot triggered another backup: %v", keys)
	}
}

func TestDatabaseBackupDisabledIsNoOp(t *testing.T) {
	app := newTestApp(t)
	app.srv.cfg.BackupInterval = 24 * time.Hour
	app.srv.cfg.BackupRetention = 3
	stale := time.Now().UTC().Add(-72 * time.Hour)
	app.seedBackup(databaseBackupObjectKey(stale), stale, []byte("stale"))
	if err := app.srv.runDatabaseBackup(context.Background()); err != nil {
		t.Fatalf("disabled backup returned error: %v", err)
	}
	if keys := app.backupKeys(); len(keys) != 1 {
		t.Fatalf("disabled backup modified the backup prefix: %v", keys)
	}
}

func TestDatabaseBackupUploadFailureKeepsStateClean(t *testing.T) {
	app := newTestApp(t)
	app.enableBackups(24*time.Hour, 3)
	app.store.mu.Lock()
	app.store.storeBlobErr = errors.New("s3 unavailable")
	app.store.mu.Unlock()
	if err := app.srv.runDatabaseBackup(context.Background()); err == nil {
		t.Fatal("upload failure must surface as an error so the scheduler retries")
	}
	if keys := app.backupKeys(); len(keys) != 0 {
		t.Fatalf("failed upload left objects behind: %v", keys)
	}
	app.requireNoStagingSnapshots()
}

func TestSystemStatusReportsBackupComponent(t *testing.T) {
	app := newTestApp(t)
	if app.srv.cfg.BackupEnabled {
		t.Fatal("test app should start with backups disabled")
	}
	out := app.srv.collectSystemStatus(context.Background())
	if out.Backup.Status != "ok" || out.Backup.Enabled {
		t.Fatalf("disabled backup status = %+v", out.Backup)
	}
}
