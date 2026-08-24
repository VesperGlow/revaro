package server

import (
	"bytes"
	"context"
	"errors"
	"io"
	"log/slog"
	"strings"
	"testing"
)

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
	for _, attributes := range []string{"A_ lrwxrwxrwx", "reparse point"} {
		if !archiveAttributesUnsafe(attributes) {
			t.Fatalf("unsafe archive attributes accepted: %q", attributes)
		}
	}
}

func TestArchiveFailureIsDetailedAndStructured(t *testing.T) {
	app := newTestApp(t)
	var logs bytes.Buffer
	app.srv.log = slog.New(slog.NewJSONHandler(&logs, nil))
	job := &archiveJob{ID: "job-1", FileID: "file-1", Status: "extracting"}
	app.srv.failArchiveJob(job, errors.New("7-Zip: invalid archive"))
	snapshot := job.snapshot()
	if snapshot.Status != "failed" || snapshot.Error != "解压失败：7-Zip: invalid archive" || snapshot.Message != snapshot.Error {
		t.Fatalf("job status=%q error=%q message=%q", snapshot.Status, snapshot.Error, snapshot.Message)
	}
	for _, value := range []string{`"level":"WARN"`, `"file":"file-1"`, `"job":"job-1"`, `"status":"extracting"`, `"error":"7-Zip: invalid archive"`} {
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
	size, err := archiveExpandedSize([]archiveEntry{{Path: "one", Size: 3}, {Path: "dir", IsDir: true}, {Path: "two", Size: 5}})
	if err != nil || size != 8 {
		t.Fatalf("expanded size=%d err=%v", size, err)
	}
}

func TestEncryptedArchiveIsDetectedBeforeExtraction(t *testing.T) {
	listing := "Path = secret.txt\nSize = 42\nAttributes = A\nEncrypted = +\n\n"
	if _, err := parseArchiveListing(listing, 1024, false); !errors.Is(err, errArchivePasswordRequired) {
		t.Fatalf("encrypted listing error=%v, want password required", err)
	}
	entries, err := parseArchiveListing(listing, 1024, true)
	if err != nil || len(entries) != 1 || !entries[0].Encrypted {
		t.Fatalf("password-supplied listing=%+v err=%v", entries, err)
	}
	if err := archivePasswordFailure("ERROR: Wrong password?", false); !errors.Is(err, errArchivePasswordRequired) {
		t.Fatalf("missing-password classification=%v", err)
	}
	if err := archivePasswordFailure("ERROR: Wrong password?", true); !errors.Is(err, errArchiveWrongPassword) {
		t.Fatalf("wrong-password classification=%v", err)
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
	if snapshot.Status != "queued" || snapshot.Error != "" || snapshot.Message != "正在验证密码" {
		t.Fatalf("resumed job status=%q error=%q message=%q", snapshot.Status, snapshot.Error, snapshot.Message)
	}
	if job.resumeWithPassword() {
		t.Fatal("concurrent password retry was accepted")
	}
}

func TestArchiveSourceSupportsManifestAndLegacyRawObjects(t *testing.T) {
	app := newTestApp(t)
	manifestFile := app.readyFile(t, "manifest.zip", []byte("manifest archive"))
	app.store.raw["legacy/archive.zip"] = []byte("legacy archive")
	legacyFile := File{objectKey: "legacy/archive.zip"}
	for _, test := range []struct {
		file File
		want string
	}{{manifestFile, "manifest archive"}, {legacyFile, "legacy archive"}} {
		source, err := app.srv.openArchiveSource(context.Background(), test.file)
		if err != nil {
			t.Fatal(err)
		}
		got, err := io.ReadAll(source)
		_ = source.Close()
		if err != nil || string(got) != test.want {
			t.Fatalf("source=%q err=%v, want %q", got, err, test.want)
		}
	}
}
