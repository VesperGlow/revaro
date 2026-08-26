package storage

import (
	"context"
	"fmt"
	"io"
	"sort"
	"sync"
)

const (
	fileReaderBasePrefetchBlocks  = 2
	fileReaderPrefetchConcurrency = 4
	fileReaderInitialReadAhead    = int64(8 << 20)
)

type dynamicReadAheadKey struct{}

// WithDynamicReadAhead enables adaptive look-ahead for a sequential consumer.
// BLOCK_READ_AHEAD is only the hard ceiling: a reader starts with a small
// window, grows it after sustained forward reads, and resets it on a seek.
func WithDynamicReadAhead(ctx context.Context) context.Context {
	return context.WithValue(ctx, dynamicReadAheadKey{}, true)
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

type blockFuture struct {
	done chan struct{}
	data []byte
	err  error
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

	cancel         context.CancelFunc
	closeOnce      sync.Once
	prefetchMu     sync.Mutex
	prefetch       map[int]*blockFuture
	prefetchSlots  chan struct{}
	prefetchCtx    context.Context
	prefetchCancel context.CancelFunc

	adaptive       bool
	readAheadCap   int64
	readAhead      int64
	sequentialRead int64
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
	prefetchCtx, prefetchCancel := context.WithCancel(readerCtx)
	adaptive, _ := ctx.Value(dynamicReadAheadKey{}).(bool)
	readAhead := int64(0)
	if adaptive && s.readAhead > 0 {
		readAhead = min(s.readAhead, fileReaderInitialReadAhead)
	}
	return &fileReader{
		getBlock: s.getBlock, ctx: readerCtx, cancel: cancel, m: m, starts: starts, loadedIdx: -1,
		prefetch: make(map[int]*blockFuture), prefetchSlots: make(chan struct{}, fileReaderPrefetchConcurrency),
		prefetchCtx: prefetchCtx, prefetchCancel: prefetchCancel, adaptive: adaptive,
		readAheadCap: s.readAhead, readAhead: readAhead,
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
	r.observeSequentialRead(int64(n))
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
		r.resetPrefetch()
		r.err = nil
		r.sequentialRead = 0
		if r.adaptive && r.readAheadCap > 0 {
			r.readAhead = min(r.readAheadCap, fileReaderInitialReadAhead)
		}
	}
	r.off = next
	return r.off, nil
}

func (r *fileReader) Close() error {
	r.closeOnce.Do(func() {
		r.prefetchMu.Lock()
		if r.prefetchCancel != nil {
			r.prefetchCancel()
		}
		r.prefetchMu.Unlock()
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
	data, err := r.loadOrWait(idx)
	if err != nil {
		return nil, err
	}
	r.loaded, r.loadedIdx = data, idx
	r.prunePrefetch(idx)
	r.startPrefetch(idx + 1)
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

func (r *fileReader) loadOrWait(idx int) ([]byte, error) {
	r.prefetchMu.Lock()
	future := r.prefetch[idx]
	if future != nil {
		delete(r.prefetch, idx)
	}
	r.prefetchMu.Unlock()
	if future == nil {
		return r.readBlock(r.ctx, idx)
	}
	select {
	case <-future.done:
		return future.data, future.err
	case <-r.ctx.Done():
		return nil, r.ctx.Err()
	}
}

func (r *fileReader) startPrefetch(first int) {
	if first >= len(r.m.Blocks) {
		return
	}
	r.prefetchMu.Lock()
	if r.prefetch == nil {
		r.prefetch = make(map[int]*blockFuture)
	}
	if r.prefetchSlots == nil {
		r.prefetchSlots = make(chan struct{}, fileReaderPrefetchConcurrency)
	}
	if r.prefetchCtx == nil {
		r.prefetchCtx, r.prefetchCancel = context.WithCancel(r.ctx)
	}
	prefetchCtx := r.prefetchCtx
	last := r.prefetchEnd(first)
	r.prefetchMu.Unlock()
	for idx := first; idx < last; idx++ {
		r.prefetchMu.Lock()
		if _, exists := r.prefetch[idx]; exists {
			r.prefetchMu.Unlock()
			continue
		}
		future := &blockFuture{done: make(chan struct{})}
		r.prefetch[idx] = future
		r.prefetchMu.Unlock()
		go func(index int, result *blockFuture, fetchCtx context.Context) {
			select {
			case r.prefetchSlots <- struct{}{}:
				defer func() { <-r.prefetchSlots }()
			case <-fetchCtx.Done():
				result.err = fetchCtx.Err()
				close(result.done)
				return
			}
			result.data, result.err = r.readBlock(fetchCtx, index)
			close(result.done)
		}(idx, future, prefetchCtx)
	}
}

func (r *fileReader) prunePrefetch(current int) {
	r.prefetchMu.Lock()
	defer r.prefetchMu.Unlock()
	first, last := current+1, r.prefetchEnd(current+1)
	for idx, future := range r.prefetch {
		if idx >= first && idx < last {
			continue
		}
		select {
		case <-future.done:
			delete(r.prefetch, idx)
		default:
		}
	}
}

func (r *fileReader) prefetchEnd(first int) int {
	if first >= len(r.m.Blocks) {
		return len(r.m.Blocks)
	}
	target := r.readAhead
	if !r.adaptive || target <= 0 {
		return min(len(r.m.Blocks), first+fileReaderBasePrefetchBlocks)
	}
	var bytes int64
	end := first
	for end < len(r.m.Blocks) && (bytes < target || end < first+fileReaderBasePrefetchBlocks) {
		bytes += r.m.Blocks[end].Size
		end++
	}
	return end
}

func (r *fileReader) observeSequentialRead(n int64) {
	if !r.adaptive || r.readAheadCap <= 0 || n <= 0 {
		return
	}
	r.sequentialRead += n
	if r.sequentialRead < max(fileReaderInitialReadAhead, r.readAhead) {
		return
	}
	r.sequentialRead = 0
	next := r.readAhead * 2
	if next == 0 {
		next = fileReaderInitialReadAhead
	}
	r.readAhead = min(r.readAheadCap, next)
}

func (r *fileReader) resetPrefetch() {
	r.prefetchMu.Lock()
	defer r.prefetchMu.Unlock()
	if r.prefetchCancel != nil {
		r.prefetchCancel()
	}
	r.prefetchCtx, r.prefetchCancel = context.WithCancel(r.ctx)
	r.prefetch = make(map[int]*blockFuture)
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
