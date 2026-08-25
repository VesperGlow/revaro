package storage

import (
	"context"
	"fmt"
	"io"
	"sort"
	"sync"
)

const fileReaderPrefetchBlocks = 3

type blockFuture struct {
	done chan struct{}
	data []byte
	err  error
}

// fileReader streams a logical file back from its blocks. It implements
// io.ReadSeeker so http.ServeContent can serve Range requests for video
// seeking; only the blocks intersecting the requested range are fetched.
type fileReader struct {
	getBlock      func(context.Context, string) ([]byte, error)
	ctx           context.Context
	m             Manifest
	starts        []int64
	off           int64
	loadedIdx     int
	loaded        []byte
	err           error
	cancel        context.CancelFunc
	closeOnce     sync.Once
	prefetchMu    sync.Mutex
	prefetch      map[int]*blockFuture
	prefetchSlots chan struct{}
}

func (s *S3) Open(ctx context.Context, key string) (io.ReadSeekCloser, error) {
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
		getBlock: s.GetBlock, ctx: readerCtx, cancel: cancel, m: m, starts: starts, loadedIdx: -1,
		prefetch: make(map[int]*blockFuture), prefetchSlots: make(chan struct{}, fileReaderPrefetchBlocks),
	}, nil
}

func (r *fileReader) Read(p []byte) (int, error) {
	if r.err != nil {
		return 0, r.err
	}
	if len(p) == 0 {
		return 0, nil
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

func (r *fileReader) Seek(offset int64, whence int) (int64, error) {
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

// blockAt locates the block containing off by its prefix offset, keeping
// backward and random Range seeks logarithmic even for very large manifests.
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

func (r *fileReader) readBlock(idx int) ([]byte, error) {
	block := r.m.Blocks[idx]
	data, err := r.getBlock(r.ctx, block.ID)
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
		return r.readBlock(idx)
	}
	select {
	case <-future.done:
		return future.data, future.err
	case <-r.ctx.Done():
		return nil, r.ctx.Err()
	}
}

func (r *fileReader) startPrefetch(first int) {
	r.prefetchMu.Lock()
	if r.prefetch == nil {
		r.prefetch = make(map[int]*blockFuture)
	}
	if r.prefetchSlots == nil {
		r.prefetchSlots = make(chan struct{}, fileReaderPrefetchBlocks)
	}
	r.prefetchMu.Unlock()
	last := min(len(r.m.Blocks), first+fileReaderPrefetchBlocks)
	for idx := first; idx < last; idx++ {
		r.prefetchMu.Lock()
		if _, exists := r.prefetch[idx]; exists {
			r.prefetchMu.Unlock()
			continue
		}
		future := &blockFuture{done: make(chan struct{})}
		r.prefetch[idx] = future
		r.prefetchMu.Unlock()
		go func(index int, result *blockFuture) {
			select {
			case r.prefetchSlots <- struct{}{}:
				defer func() { <-r.prefetchSlots }()
			case <-r.ctx.Done():
				result.err = r.ctx.Err()
				close(result.done)
				return
			}
			result.data, result.err = r.readBlock(index)
			close(result.done)
		}(idx, future)
	}
}

func (r *fileReader) prunePrefetch(current int) {
	r.prefetchMu.Lock()
	defer r.prefetchMu.Unlock()
	for idx, future := range r.prefetch {
		if idx > current && idx <= current+fileReaderPrefetchBlocks {
			continue
		}
		select {
		case <-future.done:
			delete(r.prefetch, idx)
		default:
		}
	}
}

// ReadFile reads an entire logical file with a size guard.
func (s *S3) ReadFile(ctx context.Context, key string, limit int64) ([]byte, error) {
	rc, err := s.Open(ctx, key)
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
