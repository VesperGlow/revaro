package server

import (
	"context"
	"fmt"
	"io"
	"time"

	"github.com/VesperGlow/revaro/internal/storage"
)

// ObjectManager is the single policy layer above object storage. Business
// modules retain object naming and DB transactions, while retries, verification
// and durable cleanup are centralized here.
type ObjectManager struct {
	store  storage.Storage
	server *Server
}

func newObjectManager(store storage.Storage) *ObjectManager { return &ObjectManager{store: store} }

func (m *ObjectManager) retry(ctx context.Context, operation string, fn func() error) error {
	var last error
	for attempt := 0; attempt < 3; attempt++ {
		if err := ctx.Err(); err != nil {
			return appError("cancelled", "操作已取消", err, false)
		}
		if last = fn(); last == nil {
			return nil
		}
		if storage.IsNotFound(last) {
			return last
		}
		if attempt < 2 {
			timer := time.NewTimer(time.Duration(attempt+1) * 100 * time.Millisecond)
			select {
			case <-ctx.Done():
				timer.Stop()
				return appError("cancelled", "操作已取消", ctx.Err(), false)
			case <-timer.C:
			}
		}
	}
	return appError("object_"+operation+"_failed", "对象存储暂时不可用，请稍后重试", last, true)
}

func (m *ObjectManager) Stat(ctx context.Context, key string) (storage.ObjectInfo, error) {
	var out storage.ObjectInfo
	err := m.retry(ctx, "stat", func() error { var err error; out, err = m.store.HeadObject(ctx, key); return err })
	return out, err
}

func (m *ObjectManager) Stream(ctx context.Context, key, mime string, body io.Reader, size int64) (storage.ObjectInfo, error) {
	// Readers cannot safely be replayed, so the stream itself is attempted once;
	// callers retry the idempotent operation with a fresh stream.
	out, err := m.store.StoreBlob(ctx, key, mime, body, size)
	if err != nil {
		return storage.ObjectInfo{}, appError("object_put_failed", "文件写入存储失败，请稍后重试", err, true)
	}
	return out, nil
}

func (m *ObjectManager) Put(ctx context.Context, key, mime string, data []byte) (storage.ObjectInfo, error) {
	var out storage.ObjectInfo
	err := m.retry(ctx, "put", func() error { var err error; out, err = m.store.PutObject(ctx, key, mime, data); return err })
	return out, err
}
func (m *ObjectManager) Open(ctx context.Context, key string) (io.ReadCloser, error) {
	body, err := m.store.OpenRaw(ctx, key)
	if err != nil {
		return nil, appError("object_read_failed", "无法读取文件内容", err, true)
	}
	return body, nil
}
func (m *ObjectManager) CompleteMultipart(ctx context.Context, key, uploadID string, parts []storage.CompletedPart) (storage.ObjectInfo, error) {
	info, err := m.store.CompleteMultipart(ctx, key, uploadID, parts)
	if err == nil {
		return info, nil
	}
	if existing, headErr := m.store.HeadObject(ctx, key); headErr == nil {
		return existing, nil
	}
	return storage.ObjectInfo{}, appError("multipart_commit_failed", "分片上传提交失败，请重试", err, true)
}
func (m *ObjectManager) Ping(ctx context.Context) error { return m.store.Ping(ctx) }
func (m *ObjectManager) PresignGet(ctx context.Context, key, name, mime string, inline bool, ttl time.Duration) (string, error) {
	return m.store.PresignGetObject(ctx, key, name, mime, inline, ttl)
}
func (m *ObjectManager) PresignPut(ctx context.Context, key, mime string, ttl time.Duration) (string, error) {
	return m.store.PresignPutObject(ctx, key, mime, ttl)
}
func (m *ObjectManager) CreateMultipart(ctx context.Context, key, mime string) (string, error) {
	return m.store.CreateMultipart(ctx, key, mime)
}
func (m *ObjectManager) PresignPart(ctx context.Context, key, id string, part int32, ttl time.Duration) (string, error) {
	return m.store.PresignUploadPart(ctx, key, id, part, ttl)
}
func (m *ObjectManager) AbortMultipart(ctx context.Context, key, id string) error {
	return m.store.AbortMultipart(ctx, key, id)
}
func (m *ObjectManager) Get(ctx context.Context, key string, limit int64) ([]byte, error) {
	return m.store.GetObject(ctx, key, limit)
}
func (m *ObjectManager) OpenSeek(ctx context.Context, key string) (storage.ReadSeekCloserAt, error) {
	return m.store.Open(ctx, key)
}
func (m *ObjectManager) PutImmutable(ctx context.Context, key, mime string, data []byte) error {
	return m.store.PutImmutable(ctx, key, mime, data)
}
func (m *ObjectManager) ListPrefix(ctx context.Context, prefix string) ([]storage.ObjectRef, error) {
	return m.store.ListPrefix(ctx, prefix)
}
func (m *ObjectManager) WalkPrefix(ctx context.Context, prefix string, fn func([]storage.ObjectRef) error) error {
	return m.store.WalkPrefix(ctx, prefix, fn)
}
func (m *ObjectManager) Archive() (storage.ArchiveExtractor, bool) {
	v, ok := m.store.(storage.ArchiveExtractor)
	return v, ok
}
func (m *ObjectManager) Torrent() (storage.TorrentEngine, bool) {
	v, ok := m.store.(storage.TorrentEngine)
	return v, ok
}

func (m *ObjectManager) Delete(ctx context.Context, key, reason string) error {
	err := m.retry(ctx, "delete", func() error { return m.store.DeleteObject(ctx, key) })
	if err == nil || storage.IsNotFound(err) {
		return nil
	}
	if m.server != nil {
		m.server.queueObjectCleanup(ctx, key, reason)
	}
	return err
}

func (m *ObjectManager) DeleteMany(ctx context.Context, keys []string, reason string) error {
	if len(keys) == 0 {
		return nil
	}
	err := m.retry(ctx, "delete", func() error { return m.store.DeleteObjects(ctx, keys) })
	if err != nil && m.server != nil {
		for _, key := range keys {
			m.server.queueObjectCleanup(ctx, key, reason)
		}
	}
	return err
}

func (m *ObjectManager) Verify(ctx context.Context, key string, expectedSize int64, expectedHash string) (storage.ObjectInfo, string, error) {
	info, err := m.Stat(ctx, key)
	if err != nil {
		return storage.ObjectInfo{}, "", err
	}
	if info.Size != expectedSize {
		return info, "", appError("size_mismatch", "文件大小校验失败", fmt.Errorf("got %d want %d", info.Size, expectedSize), false)
	}
	hash, err := m.server.hashObjectRaw(ctx, key, expectedSize)
	if err != nil {
		return info, "", appError("hash_failed", "文件完整性校验失败，请重试", err, true)
	}
	if expectedHash != "" && hash != expectedHash {
		return info, hash, appError("hash_mismatch", "文件完整性校验失败", fmt.Errorf("hash mismatch"), false)
	}
	return info, hash, nil
}
