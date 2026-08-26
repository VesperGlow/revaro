package storage

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"sort"
	"strconv"
	"sync"
	"testing"

	"github.com/VesperGlow/revaro/internal/config"
)

type multipartS3Fixture struct {
	mu       sync.Mutex
	object   []byte
	parts    map[int][]byte
	created  int
	complete int
	aborted  int
	put      int
}

func (f *multipartS3Fixture) handler(w http.ResponseWriter, r *http.Request) {
	query := r.URL.Query()
	switch {
	case r.Method == http.MethodPost && query.Has("uploads"):
		f.mu.Lock()
		f.created++
		f.parts = make(map[int][]byte)
		f.mu.Unlock()
		w.Header().Set("Content-Type", "application/xml")
		_, _ = io.WriteString(w, `<?xml version="1.0" encoding="UTF-8"?><CreateMultipartUploadResult><Bucket>revaro</Bucket><Key>blobs/test</Key><UploadId>upload-1</UploadId></CreateMultipartUploadResult>`)
	case r.Method == http.MethodPut && query.Get("uploadId") != "":
		partNumber, _ := strconv.Atoi(query.Get("partNumber"))
		data, err := io.ReadAll(r.Body)
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		f.mu.Lock()
		f.parts[partNumber] = data
		f.mu.Unlock()
		w.Header().Set("ETag", fmt.Sprintf(`"part-%d"`, partNumber))
	case r.Method == http.MethodPost && query.Get("uploadId") != "":
		f.mu.Lock()
		numbers := make([]int, 0, len(f.parts))
		for number := range f.parts {
			numbers = append(numbers, number)
		}
		sort.Ints(numbers)
		f.object = f.object[:0]
		for _, number := range numbers {
			f.object = append(f.object, f.parts[number]...)
		}
		f.complete++
		f.mu.Unlock()
		w.Header().Set("Content-Type", "application/xml")
		_, _ = io.WriteString(w, `<?xml version="1.0" encoding="UTF-8"?><CompleteMultipartUploadResult><Bucket>revaro</Bucket><Key>blobs/test</Key><ETag>"complete"</ETag></CompleteMultipartUploadResult>`)
	case r.Method == http.MethodDelete && query.Get("uploadId") != "":
		f.mu.Lock()
		f.aborted++
		f.mu.Unlock()
		w.WriteHeader(http.StatusNoContent)
	case r.Method == http.MethodHead:
		f.mu.Lock()
		size := len(f.object)
		f.mu.Unlock()
		w.Header().Set("Content-Length", strconv.Itoa(size))
		w.Header().Set("ETag", `"stored"`)
	case r.Method == http.MethodPut:
		data, err := io.ReadAll(r.Body)
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		f.mu.Lock()
		f.object = data
		f.put++
		f.mu.Unlock()
		w.Header().Set("ETag", `"stored"`)
	case r.Method == http.MethodDelete:
		f.mu.Lock()
		f.object = nil
		f.mu.Unlock()
		w.WriteHeader(http.StatusNoContent)
	default:
		http.Error(w, "unexpected S3 request", http.StatusBadRequest)
	}
}

func newMultipartTestS3(t *testing.T) (*S3, *multipartS3Fixture) {
	t.Helper()
	fixture := &multipartS3Fixture{}
	server := httptest.NewServer(http.HandlerFunc(fixture.handler))
	t.Cleanup(server.Close)
	store, err := NewS3(context.Background(), config.Config{
		S3Endpoint: server.URL, S3Region: "us-east-1", S3Bucket: "revaro",
		S3AccessKey: "access-key", S3SecretKey: "secret-key", S3PathStyle: true,
	})
	if err != nil {
		t.Fatal(err)
	}
	return store, fixture
}

func TestStoreBlobUsesPutObjectOnlyForSmallKnownStreams(t *testing.T) {
	store, fixture := newMultipartTestS3(t)
	data := []byte("small blob")
	info, err := store.StoreBlob(context.Background(), "blobs/test", "application/octet-stream", bytes.NewReader(data), int64(len(data)))
	if err != nil {
		t.Fatal(err)
	}
	if info.Size != int64(len(data)) || !bytes.Equal(fixture.object, data) || fixture.put != 1 || fixture.created != 0 {
		t.Fatalf("info=%+v put=%d multipart=%d object=%d", info, fixture.put, fixture.created, len(fixture.object))
	}
}

func TestStoreBlobUsesMultipartForLargeKnownStreams(t *testing.T) {
	store, fixture := newMultipartTestS3(t)
	data := bytes.Repeat([]byte("x"), int(storeBlobMultipartThreshold+123))
	info, err := store.StoreBlob(context.Background(), "blobs/test", "application/octet-stream", bytes.NewReader(data), int64(len(data)))
	if err != nil {
		t.Fatal(err)
	}
	if info.Size != int64(len(data)) || !bytes.Equal(fixture.object, data) || fixture.created != 1 || fixture.complete != 1 || fixture.put != 0 {
		t.Fatalf("info=%+v created=%d complete=%d put=%d object=%d", info, fixture.created, fixture.complete, fixture.put, len(fixture.object))
	}
}

func TestStoreBlobUsesMultipartForUnknownStreams(t *testing.T) {
	store, fixture := newMultipartTestS3(t)
	data := []byte("unknown-size stream")
	info, err := store.StoreBlob(context.Background(), "blobs/test", "application/octet-stream", bytes.NewReader(data), -1)
	if err != nil {
		t.Fatal(err)
	}
	if info.Size != int64(len(data)) || !bytes.Equal(fixture.object, data) || fixture.created != 1 || fixture.complete != 1 || fixture.put != 0 {
		t.Fatalf("info=%+v created=%d complete=%d put=%d", info, fixture.created, fixture.complete, fixture.put)
	}
}

type failingStream struct {
	data []byte
	err  error
}

func (r *failingStream) Read(p []byte) (int, error) {
	if len(r.data) == 0 {
		return 0, r.err
	}
	n := copy(p, r.data)
	r.data = r.data[n:]
	return n, nil
}

func TestStoreBlobAbortsMultipartAfterStreamFailure(t *testing.T) {
	store, fixture := newMultipartTestS3(t)
	wantErr := errors.New("source failed")
	_, err := store.StoreBlob(context.Background(), "blobs/test", "application/octet-stream", &failingStream{data: []byte("partial"), err: wantErr}, -1)
	if !errors.Is(err, wantErr) {
		t.Fatalf("error=%v, want source failure", err)
	}
	if fixture.created != 1 || fixture.aborted != 1 || fixture.complete != 0 {
		t.Fatalf("created=%d aborted=%d complete=%d", fixture.created, fixture.aborted, fixture.complete)
	}
}
