package storage

import (
	"context"
	"fmt"
	"io"
	"sort"
	"sync"
)

// WithDynamicReadAhead remains as a source-compatible no-op for legacy call
// sites. The compatibility reader is deliberately demand-only now: it never
// launches work beyond the block containing the current read.
func WithDynamicReadAhead(ctx context.Context) context.Context {
	return ctx
}

// ReadSeekCloserAt is the common logical-file data plane. A single instance is
// suitable for HTTP Range serving and for tools which prefer ReaderAt. Reads
// are backed by immutable FastCDC blocks through RAM L1, SSD L2, then S3 L3.
type ReadSeekCloserAt interface {
	io.Reader
	io.ReaderAt
	io.Seeker
	io.Closer
	Size() int64
}

type fileReader struct {
	getBlock func(context.Context, Block) ([]byte, error)
	ctx      context.Context
	m        Manifest
	starts   []int64

	mu        sync.Mutex
	off       int64
	loadedIdx int
	loaded    []byte
	err       error

	cancel    context.CancelFunc
	closeOnce sync.Once
}

func (s *S3) Open(ctx context.Context, key string) (ReadSeekCloserAt, error) {
	m, err := s.GetManifest(ctx, key)
	if err != nil {
		return nil, err
	}
	starts := make([]int64, len(m.Blocks))
	var offset int64
	for i, block := range m.Blocks {
		starts[i] = offset
		offset += block.Size
	}
	readerCtx, cancel := context.WithCancel(ctx)
	return &fileReader{
		getBlock: s.getBlock, ctx: readerCtx, cancel: cancel, m: m, starts: starts, loadedIdx: -1,
	}, nil
}

func (r *fileReader) Size() int64 { return r.m.Size }

func (r *fileReader) Read(p []byte) (int, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.err != nil {
		return 0, r.err
	}
	if len(p) == 0 {
		return 0, nil
	}
	if err := r.ctx.Err(); err != nil {
		return 0, err
	}
	if r.off >= r.m.Size {
		return 0, io.EOF
	}
	idx, start, _, err := r.blockAt(r.off)
	if err != nil {
		return 0, err
	}
	data, err := r.load(idx)
	if err != nil {
		r.err = err
		return 0, err
	}
	n := copy(p, data[r.off-start:])
	r.off += int64(n)
	return n, nil
}

// ReadAt is deliberately demand-only. Random probes must not launch a forward
// prefetch train; subsequent adjacent calls are served by the shared L1/L2
// caches and sequential consumers should use Read instead.
func (r *fileReader) ReadAt(p []byte, off int64) (int, error) {
	if len(p) == 0 {
		return 0, nil
	}
	if off < 0 {
		return 0, fmt.Errorf("negative read offset %d", off)
	}
	if off >= r.m.Size {
		return 0, io.EOF
	}
	written := 0
	for written < len(p) && off < r.m.Size {
		if err := r.ctx.Err(); err != nil {
			return written, err
		}
		idx, start, _, err := r.blockAt(off)
		if err != nil {
			return written, err
		}
		data, err := r.readBlock(r.ctx, idx)
		if err != nil {
			return written, err
		}
		n := copy(p[written:], data[off-start:])
		written += n
		off += int64(n)
	}
	if written < len(p) {
		return written, io.EOF
	}
	return written, nil
}

func (r *fileReader) Seek(offset int64, whence int) (int64, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	var base int64
	switch whence {
	case io.SeekStart:
		base = 0
	case io.SeekCurrent:
		base = r.off
	case io.SeekEnd:
		base = r.m.Size
	default:
		return 0, fmt.Errorf("invalid whence %d", whence)
	}
	next := base + offset
	if next < 0 {
		return 0, fmt.Errorf("negative seek position %d", next)
	}
	if next != r.off {
		r.err = nil
	}
	r.off = next
	return r.off, nil
}

func (r *fileReader) Close() error {
	r.closeOnce.Do(func() {
		if r.cancel != nil {
			r.cancel()
		}
	})
	return nil
}

func (r *fileReader) blockAt(off int64) (int, int64, int64, error) {
	if off < 0 || off >= r.m.Size || len(r.starts) == 0 {
		return 0, 0, 0, io.EOF
	}
	idx := sort.Search(len(r.starts), func(i int) bool { return r.starts[i] > off }) - 1
	if idx < 0 {
		idx = 0
	}
	return idx, r.starts[idx], r.m.Blocks[idx].Size, nil
}

func (r *fileReader) load(idx int) ([]byte, error) {
	if r.loadedIdx == idx && r.loaded != nil {
		return r.loaded, nil
	}
	data, err := r.readBlock(r.ctx, idx)
	if err != nil {
		return nil, err
	}
	r.loaded, r.loadedIdx = data, idx
	return data, nil
}

func (r *fileReader) readBlock(ctx context.Context, idx int) ([]byte, error) {
	block := r.m.Blocks[idx]
	data, err := r.getBlock(ctx, block)
	if err != nil {
		return nil, fmt.Errorf("read block %s: %w", block.ID, err)
	}
	if int64(len(data)) != block.Size {
		return nil, fmt.Errorf("block %s size mismatch: stored %d bytes, manifest says %d", block.ID, len(data), block.Size)
	}
	return data, nil
}

// ReadFile reads an entire logical file with a size guard.
func (s *S3) ReadFile(ctx context.Context, key string, limit int64) ([]byte, error) {
	rc, err := s.Open(WithDynamicReadAhead(ctx), key)
	if err != nil {
		return nil, err
	}
	defer rc.Close()
	data, err := io.ReadAll(io.LimitReader(rc, limit+1))
	if err != nil {
		return nil, err
	}
	if int64(len(data)) > limit {
		return nil, ErrObjectTooLarge
	}
	return data, nil
}
