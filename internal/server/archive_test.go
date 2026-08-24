package server

import "testing"

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
