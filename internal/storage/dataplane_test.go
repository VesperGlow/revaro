package storage

import (
	"bytes"
	"context"
	"io"
	"os"
	"testing"
	"time"
)

func TestDataPlaneS3Lifecycle(t *testing.T) {
	addr, token := os.Getenv("DATA_PLANE_TEST_ADDR"), os.Getenv("DATA_PLANE_TEST_TOKEN")
	if addr == "" || token == "" {
		t.Skip("Rust data-plane integration endpoint not configured")
	}
	store := NewDataPlane(addr, token)
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	if err := store.Ping(ctx); err != nil {
		t.Fatal(err)
	}
	key := "blobs/data-plane-integration"
	t.Cleanup(func() { _ = store.DeleteObject(context.Background(), key) })
	payload := bytes.Repeat([]byte("0123456789abcdef"), (20<<20)/16+1)
	payload = payload[:20<<20]
	info, err := store.StoreBlob(ctx, key, "application/octet-stream", bytes.NewReader(payload), int64(len(payload)))
	if err != nil {
		t.Fatal(err)
	}
	if info.Size != int64(len(payload)) || info.ETag == "" {
		t.Fatalf("info=%+v", info)
	}
	reader, err := store.Open(WithDynamicReadAhead(ctx), key)
	if err != nil {
		t.Fatal(err)
	}
	defer reader.Close()
	if _, err := reader.Seek(7<<20, io.SeekStart); err != nil {
		t.Fatal(err)
	}
	got := make([]byte, 128<<10)
	if _, err := io.ReadFull(reader, got); err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(got, payload[7<<20:(7<<20)+len(got)]) {
		t.Fatal("Range/seek content mismatch")
	}
	refs, err := store.ListPrefix(ctx, "blobs/data-plane-")
	if err != nil || len(refs) != 1 || refs[0].Key != key {
		t.Fatalf("refs=%+v err=%v", refs, err)
	}
	if _, err := store.PresignGetObject(ctx, key, "测试.bin", "application/octet-stream", false, time.Minute); err != nil {
		t.Fatal(err)
	}
	if err := store.DeleteObjects(ctx, []string{key}); err != nil {
		t.Fatal(err)
	}
}
