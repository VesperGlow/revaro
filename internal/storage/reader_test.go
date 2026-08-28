package storage

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"sync/atomic"
	"testing"

	"github.com/VesperGlow/revaro/internal/config"
)

func TestObjectReaderCoalescesSequentialSmallRangeReads(t *testing.T) {
	data := bytes.Repeat([]byte("0123456789abcdef"), 256<<10)
	var gets atomic.Int64
	h := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method == http.MethodHead {
			w.Header().Set("Content-Length", strconv.Itoa(len(data)))
			return
		}
		if r.Method != http.MethodGet {
			http.Error(w, "unexpected", 400)
			return
		}
		gets.Add(1)
		var start, end int
		if _, err := fmt.Sscanf(strings.TrimPrefix(r.Header.Get("Range"), "bytes="), "%d-%d", &start, &end); err != nil {
			http.Error(w, "range", 400)
			return
		}
		end = min(end, len(data)-1)
		w.Header().Set("Content-Range", fmt.Sprintf("bytes %d-%d/%d", start, end, len(data)))
		w.WriteHeader(http.StatusPartialContent)
		_, _ = w.Write(data[start : end+1])
	})
	server := httptest.NewServer(h)
	defer server.Close()
	store, err := NewS3(context.Background(), config.Config{S3Endpoint: server.URL, S3Region: "us-east-1", S3Bucket: "revaro", S3AccessKey: "a", S3SecretKey: "b", S3PathStyle: true})
	if err != nil {
		t.Fatal(err)
	}
	r, err := store.Open(WithDynamicReadAhead(context.Background()), "blobs/test")
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
	if got := gets.Load(); got > 2 {
		t.Fatalf("sequential small reads made %d range requests, want <=2", got)
	}
	if err := r.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := r.ReadAt(buf, 0); err != io.ErrClosedPipe {
		t.Fatalf("read after close=%v", err)
	}
}
