package btstore

import (
	"bytes"
	"context"
	"io"
	"log/slog"
	"os"
	"testing"

	"github.com/VesperGlow/revaro/internal/database"
	appstorage "github.com/VesperGlow/revaro/internal/storage"
)

type testStorage struct {
	appstorage.Storage
	data      []byte
	streamed  int64
	putCalled bool
}

func (s *testStorage) StoreBlob(_ context.Context, _ string, _ string, r io.Reader, size int64) (appstorage.ObjectInfo, error) {
	n, err := io.Copy(io.Discard, r)
	s.streamed = n
	return appstorage.ObjectInfo{Size: n}, err
}
func (s *testStorage) PutObject(context.Context, string, string, []byte) (appstorage.ObjectInfo, error) {
	s.putCalled = true
	return appstorage.ObjectInfo{}, nil
}
func (s *testStorage) Open(context.Context, string) (appstorage.ReadSeekCloserAt, error) {
	return &testReader{Reader: *bytes.NewReader(s.data)}, nil
}
func (s *testStorage) DeleteObject(context.Context, string) error { return nil }

type testReader struct{ bytes.Reader }

func (r *testReader) Close() error { return nil }
func (r *testReader) Size() int64  { return r.Reader.Size() }

func TestLargePieceCompletionStreamsAndCompletedReadsUseReaderAt(t *testing.T) {
	db, err := database.Open(t.TempDir() + "/db.sqlite")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	store := &testStorage{data: bytes.Repeat([]byte("abcdefgh"), 256<<10)}
	client := &Client{db: db, objects: store, log: slog.Default()}
	path := t.TempDir() + "/piece"
	f, err := os.Create(path)
	if err != nil {
		t.Fatal(err)
	}
	const pieceSize = int64(64 << 20)
	if err := f.Truncate(pieceSize); err != nil {
		t.Fatal(err)
	}
	_ = f.Close()
	p := &piece{client: client, infoHash: "hash", index: 1, size: pieceSize, path: path, objectKey: "bt-temp/hash/00000001"}
	if err := p.MarkComplete(); err != nil {
		t.Fatal(err)
	}
	if store.putCalled || store.streamed != pieceSize {
		t.Fatalf("put=%v streamed=%d", store.putCalled, store.streamed)
	}
	p.size = int64(len(store.data))
	r, err := p.NewReader()
	if err != nil {
		t.Fatal(err)
	}
	defer r.Close()
	buf := make([]byte, 32)
	if _, err := r.ReadAt(buf, 12345); err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(buf, store.data[12345:12345+32]) {
		t.Fatal("random ReadAt mismatch")
	}
}
