// Package btstore implements an anacrolix/torrent piece store backed by the
// same S3-compatible object store as Revaro. Only unverified pieces touch the
// local filesystem; once a complete piece passes BitTorrent's hash check it is
// uploaded as one object and the local cache file is removed.
package btstore

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"strconv"
	"sync"
	"time"

	appstorage "github.com/VesperGlow/revaro/internal/storage"
	"github.com/anacrolix/torrent/metainfo"
	torrentstorage "github.com/anacrolix/torrent/storage"
)

const objectPrefix = "bt-temp/"

type Client struct {
	db       *sql.DB
	objects  appstorage.Storage
	cacheDir string
	log      *slog.Logger

	mu     sync.Mutex
	pieces map[string]*piece
}

func New(db *sql.DB, objects appstorage.Storage, cacheDir string, log *slog.Logger) (*Client, error) {
	if log == nil {
		log = slog.Default()
	}
	if err := os.MkdirAll(cacheDir, 0o700); err != nil {
		return nil, fmt.Errorf("create torrent cache: %w", err)
	}
	if err := os.Chmod(cacheDir, 0o700); err != nil {
		return nil, fmt.Errorf("secure torrent cache: %w", err)
	}
	return &Client{db: db, objects: objects, cacheDir: cacheDir, log: log, pieces: make(map[string]*piece)}, nil
}

func (c *Client) OpenTorrent(_ context.Context, info *metainfo.Info, infoHash metainfo.Hash) (torrentstorage.TorrentImpl, error) {
	hash := infoHash.HexString()
	if info.PieceLength <= 0 || info.PieceLength > 64<<20 {
		return torrentstorage.TorrentImpl{}, errors.New("torrent piece size must be between 1 byte and 64 MiB")
	}
	dir := filepath.Join(c.cacheDir, hash)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return torrentstorage.TorrentImpl{}, err
	}
	return torrentstorage.TorrentImpl{
		Piece: func(metaPiece metainfo.Piece) torrentstorage.PieceImpl {
			return c.getPiece(hash, metaPiece.Index(), metaPiece.Length(), dir)
		},
		Close: func() error { return nil },
	}, nil
}

func (c *Client) getPiece(hash string, index int, size int64, dir string) *piece {
	identity := hash + ":" + strconv.Itoa(index)
	c.mu.Lock()
	defer c.mu.Unlock()
	if existing := c.pieces[identity]; existing != nil {
		return existing
	}
	p := &piece{
		client: c, infoHash: hash, index: index, size: size,
		path:      filepath.Join(dir, fmt.Sprintf("%08d.part", index)),
		objectKey: fmt.Sprintf("%s%s/%08d", objectPrefix, hash, index),
	}
	var storedSize int64
	var storedKey string
	err := c.db.QueryRow(`SELECT size,object_key FROM download_pieces WHERE info_hash=? AND piece_index=?`, hash, index).Scan(&storedSize, &storedKey)
	if err == nil && storedSize == size && storedKey == p.objectKey {
		p.complete = true
	} else if err != nil && !errors.Is(err, sql.ErrNoRows) {
		p.completionErr = err
	}
	c.pieces[identity] = p
	return p
}

// DeleteTorrent removes all temporary piece objects and local incomplete
// files for an info hash. It is idempotent and deliberately uses the SQLite
// index rather than an S3 LIST as the source of truth.
func (c *Client) DeleteTorrent(ctx context.Context, hash string) error {
	rows, err := c.db.QueryContext(ctx, `SELECT object_key FROM download_pieces WHERE info_hash=?`, hash)
	if err != nil {
		return err
	}
	var keys []string
	for rows.Next() {
		var key string
		if err := rows.Scan(&key); err != nil {
			rows.Close()
			return err
		}
		keys = append(keys, key)
	}
	if err := rows.Close(); err != nil {
		return err
	}
	deleteCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	jobs := make(chan string)
	var workers sync.WaitGroup
	var firstErr error
	var errOnce sync.Once
	workerCount := min(8, len(keys))
	for range workerCount {
		workers.Add(1)
		go func() {
			defer workers.Done()
			for key := range jobs {
				if err := c.objects.DeleteObject(deleteCtx, key); err != nil {
					errOnce.Do(func() { firstErr = err; cancel() })
					return
				}
			}
		}()
	}
feed:
	for _, key := range keys {
		select {
		case jobs <- key:
		case <-deleteCtx.Done():
			break feed
		}
	}
	close(jobs)
	workers.Wait()
	if firstErr != nil {
		return firstErr
	}
	if err := ctx.Err(); err != nil {
		return err
	}
	if _, err := c.db.ExecContext(ctx, `DELETE FROM download_pieces WHERE info_hash=?`, hash); err != nil {
		return err
	}
	if err := os.RemoveAll(filepath.Join(c.cacheDir, hash)); err != nil {
		return err
	}
	c.mu.Lock()
	for identity := range c.pieces {
		if len(identity) > len(hash) && identity[:len(hash)+1] == hash+":" {
			delete(c.pieces, identity)
		}
	}
	c.mu.Unlock()
	return nil
}

func (c *Client) Close() error { return nil }

type piece struct {
	client    *Client
	infoHash  string
	index     int
	size      int64
	path      string
	objectKey string

	mu            sync.Mutex
	complete      bool
	completionErr error
}

func (p *piece) Completion() torrentstorage.Completion {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.completionErr != nil {
		return torrentstorage.Completion{Err: p.completionErr}
	}
	return torrentstorage.Completion{Ok: true, Complete: p.complete}
}

func (p *piece) WriteAt(data []byte, off int64) (int, error) {
	if off < 0 || off+int64(len(data)) > p.size {
		return 0, io.ErrShortWrite
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.complete {
		return 0, errors.New("cannot write a completed torrent piece")
	}
	file, err := os.OpenFile(p.path, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return 0, err
	}
	n, writeErr := file.WriteAt(data, off)
	closeErr := file.Close()
	if writeErr != nil {
		return n, writeErr
	}
	return n, closeErr
}

func (p *piece) ReadAt(data []byte, off int64) (int, error) {
	reader, err := p.NewReader()
	if err != nil {
		return 0, err
	}
	defer reader.Close()
	return reader.ReadAt(data, off)
}

func (p *piece) NewReader() (torrentstorage.PieceReader, error) {
	p.mu.Lock()
	complete := p.complete
	p.mu.Unlock()
	if !complete {
		return os.Open(p.path)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	reader, err := p.client.objects.Open(appstorage.WithDynamicReadAhead(ctx), p.objectKey)
	if err != nil {
		cancel()
		return nil, err
	}
	if reader.Size() != p.size {
		reader.Close()
		cancel()
		return nil, fmt.Errorf("torrent piece %d has size %d, expected %d", p.index, reader.Size(), p.size)
	}
	return &pieceReader{ReadSeekCloserAt: reader, cancel: cancel}, nil
}

func (p *piece) MarkComplete() error {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.complete {
		return nil
	}
	file, err := os.Open(p.path)
	if err != nil {
		return err
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return err
	}
	if info.Size() != p.size {
		return fmt.Errorf("verified torrent piece %d has size %d, expected %d", p.index, info.Size(), p.size)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Minute)
	defer cancel()
	if _, err := p.client.objects.StoreBlob(ctx, p.objectKey, "application/octet-stream", file, p.size); err != nil {
		return err
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	if _, err := p.client.db.ExecContext(ctx, `INSERT INTO download_pieces(info_hash,piece_index,size,object_key,completed_at) VALUES(?,?,?,?,?) ON CONFLICT(info_hash,piece_index) DO UPDATE SET size=excluded.size,object_key=excluded.object_key,completed_at=excluded.completed_at`, p.infoHash, p.index, p.size, p.objectKey, now); err != nil {
		_ = p.client.objects.DeleteObject(context.Background(), p.objectKey)
		return err
	}
	p.complete = true
	p.completionErr = nil
	if err := os.Remove(p.path); err != nil && !errors.Is(err, os.ErrNotExist) {
		p.client.log.Warn("completed torrent cache file cleanup failed", "info_hash", p.infoHash, "piece", p.index, "error", err)
	}
	return nil
}

func (p *piece) MarkNotComplete() error {
	p.mu.Lock()
	defer p.mu.Unlock()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	if p.complete {
		if err := p.client.objects.DeleteObject(ctx, p.objectKey); err != nil {
			return err
		}
	}
	if _, err := p.client.db.ExecContext(ctx, `DELETE FROM download_pieces WHERE info_hash=? AND piece_index=?`, p.infoHash, p.index); err != nil {
		return err
	}
	p.complete = false
	p.completionErr = nil
	if err := os.Remove(p.path); err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	return nil
}

type pieceReader struct {
	appstorage.ReadSeekCloserAt
	cancel context.CancelFunc
}

func (r *pieceReader) Close() error { r.cancel(); return r.ReadSeekCloserAt.Close() }
