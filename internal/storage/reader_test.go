package storage

import (
	"bytes"
	"context"
	"io"
	"sync/atomic"
	"testing"
)

type memoryRangeStore struct {
	data []byte
	gets atomic.Int64
}

func (s *memoryRangeStore) HeadObject(context.Context, string) (ObjectInfo, error) {
	return ObjectInfo{Size: int64(len(s.data))}, nil
}
func (s *memoryRangeStore) OpenRange(_ context.Context, _ string, start, end int64) (io.ReadCloser, error) {
	s.gets.Add(1)
	end = min(end, int64(len(s.data)-1))
	return io.NopCloser(bytes.NewReader(s.data[start : end+1])), nil
}

func TestObjectReaderCoalescesSequentialSmallRangeReads(t *testing.T) {
	data := bytes.Repeat([]byte("0123456789abcdef"), 256<<10)
	store := &memoryRangeStore{data: data}
	r, err := openObject(WithDynamicReadAhead(context.Background()), store, "blobs/test")
	if err != nil {
		t.Fatal(err)
	}
	defer r.Close()
	buf := make([]byte, 4096)
	for off := int64(0); off < 100*4096; off += 4096 {
		if _, err := r.ReadAt(buf, off); err != nil {
			t.Fatal(err)
		}
	}
	if got := store.gets.Load(); got > 2 {
		t.Fatalf("sequential small reads made %d range requests, want <=2", got)
	}
	if err := r.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := r.ReadAt(buf, 0); err != io.ErrClosedPipe {
		t.Fatalf("read after close=%v", err)
	}
}
