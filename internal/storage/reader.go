package storage

import (
	"context"
	"fmt"
	"io"
	"sync"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/s3"
)

type dynamicReadAheadKey struct{}

func WithDynamicReadAhead(ctx context.Context) context.Context {
	return context.WithValue(ctx, dynamicReadAheadKey{}, true)
}

type ReadSeekCloserAt interface {
	io.Reader
	io.ReaderAt
	io.Seeker
	io.Closer
	Size() int64
}

type objectReader struct {
	store       *S3
	ctx         context.Context
	key         string
	size        int64
	mu          sync.Mutex
	off         int64
	closed      bool
	cancel      context.CancelFunc
	window      []byte
	windowStart int64
	windowSize  int64
	lastEnd     int64
	adaptive    bool
}

func (s *S3) Open(ctx context.Context, key string) (ReadSeekCloserAt, error) {
	info, err := s.HeadObject(ctx, key)
	if err != nil {
		return nil, err
	}
	readerCtx, cancel := context.WithCancel(ctx)
	adaptive, _ := ctx.Value(dynamicReadAheadKey{}).(bool)
	return &objectReader{store: s, ctx: readerCtx, cancel: cancel, key: key, size: info.Size, windowStart: -1, windowSize: 1 << 20, lastEnd: -1, adaptive: adaptive}, nil
}
func (r *objectReader) Size() int64 { return r.size }
func (r *objectReader) Read(p []byte) (int, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.closed {
		return 0, io.ErrClosedPipe
	}
	n, err := r.readAt(p, r.off)
	r.off += int64(n)
	return n, err
}
func (r *objectReader) ReadAt(p []byte, off int64) (int, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.closed {
		return 0, io.ErrClosedPipe
	}
	return r.readAt(p, off)
}
func (r *objectReader) readAt(p []byte, off int64) (int, error) {
	if len(p) == 0 {
		return 0, nil
	}
	if off < 0 {
		return 0, fmt.Errorf("negative read offset %d", off)
	}
	if off >= r.size {
		return 0, io.EOF
	}
	requestEnd := min(r.size, off+int64(len(p)))
	windowEnd := r.windowStart + int64(len(r.window))
	if r.windowStart >= 0 && off >= r.windowStart && requestEnd <= windowEnd {
		n := copy(p, r.window[off-r.windowStart:requestEnd-r.windowStart])
		r.noteAccess(off, int64(n))
		if n < len(p) {
			return n, io.EOF
		}
		return n, nil
	}
	// Bound retained memory. Large caller buffers are fetched directly; small
	// reads get a per-reader adaptive window that grows only on continuity.
	fetchSize := int64(len(p))
	cache := r.adaptive && fetchSize <= 4<<20
	if cache {
		if off == r.lastEnd {
			r.windowSize = min(r.windowSize*2, 4<<20)
		} else {
			r.windowSize = 1 << 20
		}
		fetchSize = max(fetchSize, r.windowSize)
	}
	end := min(r.size, off+fetchSize) - 1
	out, err := r.store.client.GetObject(r.ctx, &s3.GetObjectInput{Bucket: aws.String(r.store.bucket), Key: aws.String(r.key), Range: aws.String(fmt.Sprintf("bytes=%d-%d", off, end))})
	if err != nil {
		return 0, err
	}
	defer out.Body.Close()
	if cache {
		buf := make([]byte, end-off+1)
		n, readErr := io.ReadFull(out.Body, buf)
		if readErr != nil && readErr != io.ErrUnexpectedEOF {
			return 0, readErr
		}
		r.window, r.windowStart = buf[:n], off
		n = copy(p, r.window[:min(int64(len(r.window)), int64(len(p)))])
		r.noteAccess(off, int64(n))
		if n < len(p) {
			return n, io.EOF
		}
		return n, nil
	}
	n, err := io.ReadFull(out.Body, p[:end-off+1])
	if err != nil {
		return n, err
	}
	r.window, r.windowStart = nil, -1
	r.noteAccess(off, int64(n))
	if n < len(p) {
		return n, io.EOF
	}
	return n, nil
}
func (r *objectReader) noteAccess(off, n int64) { r.lastEnd = off + n }
func (r *objectReader) Seek(offset int64, whence int) (int64, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.closed {
		return 0, io.ErrClosedPipe
	}
	var base int64
	switch whence {
	case io.SeekStart:
	case io.SeekCurrent:
		base = r.off
	case io.SeekEnd:
		base = r.size
	default:
		return 0, fmt.Errorf("invalid whence %d", whence)
	}
	next := base + offset
	if next < 0 {
		return 0, fmt.Errorf("negative seek position %d", next)
	}
	r.off = next
	return next, nil
}
func (r *objectReader) Close() error {
	// Cancel before waiting for an in-flight serialized Range GET to release
	// the mutex, otherwise Close could not promptly interrupt a stalled read.
	r.cancel()
	r.mu.Lock()
	if !r.closed {
		r.closed = true
		r.window = nil
	}
	r.mu.Unlock()
	return nil
}
func (s *S3) ReadFile(ctx context.Context, key string, limit int64) ([]byte, error) {
	return s.GetObject(ctx, key, limit)
}
