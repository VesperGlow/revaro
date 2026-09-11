package storage

import (
	"bytes"
	"context"
	"errors"
	"io"
	"os"
	"path/filepath"
	"testing"
)

func TestLocalAtomicWritesAndConfinement(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	store, err := NewLocal(dir, nil)
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()
	key := BlobKey("b0e537a5-6817-418b-a6a8-f8d5b2a02c25")
	if _, err := store.PutObject(ctx, key, "", []byte("original")); err != nil {
		t.Fatal(err)
	}
	if _, err := store.StoreBlob(ctx, key, "", bytes.NewBufferString("short"), 8); err == nil {
		t.Fatal("short write accepted")
	}
	if _, err := store.StoreBlob(ctx, key, "", bytes.NewBufferString("too long"), 2); err == nil {
		t.Fatal("oversized write accepted")
	}
	cancelled, cancel := context.WithCancel(ctx)
	cancel()
	if _, err := store.PutObject(cancelled, key, "", []byte("cancelled")); !errors.Is(err, context.Canceled) {
		t.Fatalf("cancelled write: %v", err)
	}
	if err := store.PutImmutable(ctx, key, "", []byte("replacement")); err != nil {
		t.Fatal(err)
	}
	got, err := store.GetObject(ctx, key, 8)
	if err != nil || string(got) != "original" {
		t.Fatalf("atomic content=%q err=%v", got, err)
	}
	if _, err := store.GetObject(ctx, key, 7); !errors.Is(err, ErrObjectTooLarge) {
		t.Fatalf("read bound: %v", err)
	}
	outside := t.TempDir()
	if err := os.WriteFile(filepath.Join(outside, "secret"), []byte("outside"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(outside, filepath.Join(dir, "escape")); err != nil {
		t.Fatal(err)
	}
	for _, key := range []string{"../secret", "/secret", "blobs/../../secret", "escape/secret"} {
		if _, err := store.Open(ctx, key); err == nil {
			t.Fatalf("read escaped root: %q", key)
		}
		if _, err := store.PutObject(ctx, key, "", []byte("bad")); err == nil {
			t.Fatalf("write escaped root: %q", key)
		}
	}
	secret, _ := os.ReadFile(filepath.Join(outside, "secret"))
	if string(secret) != "outside" {
		t.Fatal("external file modified")
	}
}

func TestLocalMultipartSurvivesRestartAndVerifiesParts(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	store, err := NewLocal(dir, nil)
	if err != nil {
		t.Fatal(err)
	}
	key := BlobKey("62985120-f12d-4a7b-97b9-eb6327ca8ccc")
	id, err := store.CreateMultipart(ctx, key, "")
	if err != nil {
		t.Fatal(err)
	}
	one, err := store.UploadPart(ctx, key, id, 1, bytes.NewBufferString("abc"), 3)
	if err != nil {
		t.Fatal(err)
	}
	store.Close()
	store, err = NewLocal(dir, nil)
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()
	two, err := store.UploadPart(ctx, key, id, 2, bytes.NewBufferString("def"), 3)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := store.CompleteMultipart(ctx, key, id, []CompletedPart{{1, "wrong"}, {2, two.ETag}}); err == nil {
		t.Fatal("bad part hash accepted")
	}
	if _, err := store.HeadObject(ctx, key); !errors.Is(err, ErrNotFound) {
		t.Fatal("partial object published")
	}
	info, err := store.CompleteMultipart(ctx, key, id, []CompletedPart{{1, one.ETag}, {2, two.ETag}})
	if err != nil || info.Size != 6 {
		t.Fatalf("complete: %+v %v", info, err)
	}
	f, err := store.Open(ctx, key)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	if _, err = f.Seek(2, io.SeekStart); err != nil {
		t.Fatal(err)
	}
	got, _ := io.ReadAll(f)
	if string(got) != "cdef" {
		t.Fatalf("seek=%q", got)
	}
	all, err := store.ListPrefix(ctx, "blobs/")
	if err != nil || len(all) != 1 || all[0].Key != key {
		t.Fatalf("list=%+v %v", all, err)
	}
	if err := store.DeleteObject(ctx, key); err != nil {
		t.Fatal(err)
	}
	if _, err := store.Open(ctx, key); !errors.Is(err, ErrNotFound) {
		t.Fatalf("deleted=%v", err)
	}
}
