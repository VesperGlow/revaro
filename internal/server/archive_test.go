package server

import (
	"bytes"
	"context"
	"errors"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestCancelArchiveDoesNotRemoveActiveWorkerWorkspace(t *testing.T) {
	app := newTestApp(t)
	workspace := filepath.Join(t.TempDir(), "active")
	if err := os.Mkdir(workspace, 0o700); err != nil {
		t.Fatal(err)
	}
	jobCtx, cancel := context.WithCancel(context.Background())
	defer cancel()
	job := &archiveJob{ID: "active-job", FileID: "file", Status: "extracting", cancel: cancel}
	job.setStaged(workspace, filepath.Join(workspace, "source.archive"))
	app.srv.archiveMu.Lock()
	app.srv.archiveJobs[job.ID] = job
	app.srv.archiveMu.Unlock()
	app.srv.cancelTaskRuntime(context.Background(), Task{SourceType: "archive", SourceID: job.ID})
	if _, err := os.Stat(workspace); err != nil {
		t.Fatalf("active worker workspace was removed by handler: %v", err)
	}
	if snapshot := job.snapshot(); snapshot.Status != "extracting" {
		t.Fatalf("runtime adapter rewrote active status before worker unwound: %q", snapshot.Status)
	}
	select {
	case <-jobCtx.Done():
	default:
		t.Fatal("active worker context was not cancelled")
	}
}

func TestArchiveNamesAndPaths(t *testing.T) {
	for _, name := range []string{"backup.zip", "movie.7z", "files.rar", "source.tar.gz", "source.tar.xz", "source.tzst"} {
		if !isArchiveName(name) {
			t.Fatalf("expected archive name %q", name)
		}
	}
	if got := archiveBaseName("source.tar.gz"); got != "source" {
		t.Fatalf("archiveBaseName=%q", got)
	}
	for _, path := range []string{"folder/file.txt", "字幕/第一集.vtt", "top.bin"} {
		if err := validateArchivePath(path); err != nil {
			t.Fatalf("valid path %q: %v", path, err)
		}
	}
	for _, path := range []string{"../escape", "/absolute", "C:/drive", "folder/../../escape", "folder\\file"} {
		if err := validateArchivePath(path); err == nil {
			t.Fatalf("unsafe path accepted: %q", path)
		}
	}
	if got := archiveExpandedLimit(10 << 20); got != 4<<30 {
		t.Fatalf("small archive expansion limit=%d", got)
	}
	if got := archiveExpandedLimit(1 << 30); got != maxArchiveExpandedBytes {
		t.Fatalf("large archive expansion limit=%d", got)
	}
}

func TestArchiveFailureIsDetailedAndStructured(t *testing.T) {
	app := newTestApp(t)
	var logs bytes.Buffer
	app.srv.log = slog.New(slog.NewJSONHandler(&logs, nil))
	job := &archiveJob{ID: "job-1", FileID: "file-1", Status: "extracting"}
	app.srv.failArchiveJob(job, errors.New("invalid archive"))
	snapshot := job.snapshot()
	if snapshot.Status != "failed" || snapshot.Error != "解压失败：invalid archive" || snapshot.Message != snapshot.Error {
		t.Fatalf("job status=%q error=%q message=%q", snapshot.Status, snapshot.Error, snapshot.Message)
	}
	for _, value := range []string{`"level":"WARN"`, `"file":"file-1"`, `"job":"job-1"`, `"status":"extracting"`, `"error":"invalid archive"`} {
		if !strings.Contains(logs.String(), value) {
			t.Fatalf("structured log missing %s: %s", value, logs.String())
		}
	}
}

func TestArchiveDiskSpaceFailureIsExplicit(t *testing.T) {
	message := archiveFailureMessage(errors.New("write /tmp/output: no space left on device"))
	if !strings.Contains(message, "临时磁盘空间不足") {
		t.Fatalf("message=%q", message)
	}
}

func TestArchiveJobCanResumeOnlyFromPasswordState(t *testing.T) {
	job := &archiveJob{Status: "checking", Progress: 18}
	if job.resumeWithPassword() {
		t.Fatal("non-password job resumed")
	}
	job.needsPassword("压缩包已加密，请输入密码后继续")
	if !job.resumeWithPassword() {
		t.Fatal("password job did not resume")
	}
	snapshot := job.snapshot()
	if snapshot.Status != "checking" || snapshot.Error != "" || snapshot.Message != "正在验证密码" {
		t.Fatalf("resumed job status=%q error=%q message=%q", snapshot.Status, snapshot.Error, snapshot.Message)
	}
	if job.resumeWithPassword() {
		t.Fatal("concurrent password retry was accepted")
	}
}

func TestArchivePasswordStateKeepsAndCleansStaging(t *testing.T) {
	app := newTestApp(t)
	tempDir := t.TempDir()
	job := &archiveJob{ID: "job-staged", FileID: "file-staged", Status: "checking"}
	job.setStaged(tempDir, tempDir+"/source.zip")
	job.needsPassword("需要密码")
	if snapshot := job.snapshot(); snapshot.Status != "waiting_password" {
		t.Fatalf("password state=%q", snapshot.Status)
	}
	if !job.expirePasswordWait(time.Now().Add(archivePasswordWaitTTL + time.Second)) {
		t.Fatal("password wait did not expire")
	}
	if snapshot := job.snapshot(); snapshot.Status != "failed" || !strings.Contains(snapshot.Error, "超时") {
		t.Fatalf("expired password state=%q error=%q", snapshot.Status, snapshot.Error)
	}
	app.srv.cleanupArchiveJobStaging(job)
	if stagedDir, stagedPath := job.staged(); stagedDir != "" || stagedPath != "" {
		t.Fatalf("staging references were not cleared: dir=%q path=%q", stagedDir, stagedPath)
	}
}
