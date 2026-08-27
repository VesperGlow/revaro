package storage

import (
	"context"
	"fmt"
	"io"
	"sync"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/s3"
)

func WithDynamicReadAhead(ctx context.Context) context.Context { return ctx }

type ReadSeekCloserAt interface {
	io.Reader
	io.ReaderAt
	io.Seeker
	io.Closer
	Size() int64
}

type objectReader struct {
	store  *S3
	ctx    context.Context
	key    string
	size   int64
	mu     sync.Mutex
	off    int64
	closed bool
}

func (s *S3) Open(ctx context.Context, key string) (ReadSeekCloserAt, error) {
	info, err := s.HeadObject(ctx, key)
	if err != nil {
		return nil, err
	}
	return &objectReader{store: s, ctx: ctx, key: key, size: info.Size}, nil
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
	closed := r.closed
	r.mu.Unlock()
	if closed {
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
	end := min(r.size-1, off+int64(len(p))-1)
	out, err := r.store.client.GetObject(r.ctx, &s3.GetObjectInput{Bucket: aws.String(r.store.bucket), Key: aws.String(r.key), Range: aws.String(fmt.Sprintf("bytes=%d-%d", off, end))})
	if err != nil {
		return 0, err
	}
	defer out.Body.Close()
	n, err := io.ReadFull(out.Body, p[:end-off+1])
	if err != nil {
		return n, err
	}
	if n < len(p) {
		return n, io.EOF
	}
	return n, nil
}
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
func (r *objectReader) Close() error { r.mu.Lock(); r.closed = true; r.mu.Unlock(); return nil }
func (s *S3) ReadFile(ctx context.Context, key string, limit int64) ([]byte, error) {
	return s.GetObject(ctx, key, limit)
}
