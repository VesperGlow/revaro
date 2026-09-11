package storage

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path"
	"strconv"
	"strings"
	"time"

	"github.com/VesperGlow/revaro/internal/ids"
)

// Local keeps object names opaque and confines every operation to one root.
// Temporary uploads live on the same filesystem so publication is atomic.
type Local struct {
	*DataPlane
	root *os.Root
}

func NewLocal(dir string, engine *DataPlane) (*Local, error) {
	if err := os.MkdirAll(dir, 0700); err != nil {
		return nil, err
	}
	root, err := os.OpenRoot(dir)
	if err != nil {
		return nil, err
	}
	return &Local{DataPlane: engine, root: root}, nil
}
func (l *Local) Close() error { return l.root.Close() }
func validKey(key string) bool {
	return key != "" && key != "." && key != ".." && !strings.Contains(key, "\\") && !strings.HasPrefix(key, "/") && path.Clean(key) == key && !strings.HasPrefix(key, "../")
}
func (l *Local) Ping(ctx context.Context) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	name := ".probe-" + ids.New()
	f, err := l.root.OpenFile(name, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		return err
	}
	err = f.Close()
	_ = l.root.Remove(name)
	return err
}

type localFile struct {
	*os.File
	size int64
}

func (f *localFile) Size() int64 { return f.size }
func (l *Local) Open(ctx context.Context, key string) (ReadSeekCloserAt, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if !validKey(key) {
		return nil, errors.New("invalid object key")
	}
	f, err := l.root.Open(key)
	if errors.Is(err, fs.ErrNotExist) {
		return nil, ErrNotFound
	}
	if err != nil {
		return nil, err
	}
	stat, err := f.Stat()
	if err != nil {
		f.Close()
		return nil, err
	}
	if !stat.Mode().IsRegular() {
		f.Close()
		return nil, ErrNotFound
	}
	return &localFile{f, stat.Size()}, nil
}
func (l *Local) OpenRaw(ctx context.Context, key string) (io.ReadCloser, error) {
	return l.Open(ctx, key)
}
func (l *Local) ReadFile(ctx context.Context, key string, limit int64) ([]byte, error) {
	return l.GetObject(ctx, key, limit)
}
func (l *Local) GetObject(ctx context.Context, key string, limit int64) ([]byte, error) {
	if limit < 0 {
		return nil, ErrObjectTooLarge
	}
	f, err := l.Open(ctx, key)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	if f.Size() > limit {
		return nil, ErrObjectTooLarge
	}
	return io.ReadAll(io.LimitReader(f, limit))
}
func (l *Local) HeadObject(ctx context.Context, key string) (ObjectInfo, error) {
	f, err := l.Open(ctx, key)
	if err != nil {
		return ObjectInfo{}, err
	}
	defer f.Close()
	stat, err := f.(*localFile).Stat()
	if err != nil {
		return ObjectInfo{}, err
	}
	return ObjectInfo{Size: stat.Size(), ETag: fmt.Sprintf("%x-%x", stat.Size(), stat.ModTime().UnixNano())}, nil
}

type contextReader struct {
	ctx context.Context
	r   io.Reader
}

func (r contextReader) Read(p []byte) (int, error) {
	if err := r.ctx.Err(); err != nil {
		return 0, err
	}
	return r.r.Read(p)
}

func (l *Local) write(ctx context.Context, key string, body io.Reader, size int64, immutable bool) (ObjectInfo, error) {
	if !validKey(key) {
		return ObjectInfo{}, errors.New("invalid object key")
	}
	if err := l.root.MkdirAll(path.Dir(key), 0700); err != nil {
		return ObjectInfo{}, err
	}
	temp := path.Join(path.Dir(key), ".upload-"+ids.New())
	f, err := l.root.OpenFile(temp, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		return ObjectInfo{}, err
	}
	defer l.root.Remove(temp)
	reader := io.Reader(contextReader{ctx, body})
	if size >= 0 {
		reader = io.LimitReader(reader, size+1)
	}
	n, err := io.Copy(f, reader)
	if err == nil && size >= 0 && n != size {
		err = fmt.Errorf("object size %d, expected %d", n, size)
	}
	if err == nil {
		err = ctx.Err()
	}
	if err == nil {
		err = f.Sync()
	}
	closeErr := f.Close()
	if err == nil {
		err = closeErr
	}
	if err != nil {
		return ObjectInfo{}, err
	}
	if immutable {
		err = l.root.Link(temp, key)
		if errors.Is(err, fs.ErrExist) {
			return l.HeadObject(ctx, key)
		}
	} else {
		err = l.root.Rename(temp, key)
	}
	if err != nil {
		return ObjectInfo{}, err
	}
	dir, err := l.root.Open(path.Dir(key))
	if err != nil {
		return ObjectInfo{}, err
	}
	err = dir.Sync()
	dir.Close()
	if err != nil {
		return ObjectInfo{}, err
	}
	return l.HeadObject(ctx, key)
}
func (l *Local) StoreBlob(ctx context.Context, key, mime string, body io.Reader, size int64) (ObjectInfo, error) {
	return l.write(ctx, key, body, size, false)
}
func (l *Local) PutObject(ctx context.Context, key, mime string, data []byte) (ObjectInfo, error) {
	return l.StoreBlob(ctx, key, mime, bytes.NewReader(data), int64(len(data)))
}
func (l *Local) PutImmutable(ctx context.Context, key, mime string, data []byte) error {
	_, err := l.write(ctx, key, bytes.NewReader(data), int64(len(data)), true)
	return err
}
func (l *Local) DeleteObject(ctx context.Context, key string) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if !validKey(key) {
		return errors.New("invalid object key")
	}
	err := l.root.Remove(key)
	if errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	return err
}
func (l *Local) DeleteObjects(ctx context.Context, keys []string) error {
	for _, key := range keys {
		if err := l.DeleteObject(ctx, key); err != nil {
			return err
		}
	}
	return nil
}
func (l *Local) WalkPrefix(ctx context.Context, prefix string, visit func([]ObjectRef) error) error {
	page := make([]ObjectRef, 0, 256)
	err := fs.WalkDir(l.root.FS(), ".", func(name string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if err = ctx.Err(); err != nil {
			return err
		}
		if name != "." && strings.HasPrefix(entry.Name(), ".") {
			if entry.IsDir() {
				return fs.SkipDir
			}
			return nil
		}
		if entry.IsDir() || !entry.Type().IsRegular() || !strings.HasPrefix(name, prefix) {
			return nil
		}
		info, err := entry.Info()
		if err != nil {
			return err
		}
		page = append(page, ObjectRef{Key: name, Size: info.Size(), LastModified: info.ModTime()})
		if len(page) == cap(page) {
			if err := visit(page); err != nil {
				return err
			}
			page = make([]ObjectRef, 0, 256)
		}
		return nil
	})
	if err != nil {
		return err
	}
	if len(page) > 0 {
		return visit(page)
	}
	return nil
}
func (l *Local) ListPrefix(ctx context.Context, prefix string) ([]ObjectRef, error) {
	out := []ObjectRef{}
	err := l.WalkPrefix(ctx, prefix, func(page []ObjectRef) error { out = append(out, page...); return nil })
	return out, err
}

func multipartDir(key, id string) (string, error) {
	if !validKey(key) || len(id) != 36 || strings.ContainsAny(id, "/\\.") {
		return "", errors.New("invalid multipart reference")
	}
	hash := sha256.Sum256([]byte(key))
	return ".multipart/" + id + "/" + hex.EncodeToString(hash[:]), nil
}
func (l *Local) CreateMultipart(ctx context.Context, key, mime string) (string, error) {
	if err := ctx.Err(); err != nil {
		return "", err
	}
	id := ids.New()
	dir, err := multipartDir(key, id)
	if err != nil {
		return "", err
	}
	return id, l.root.MkdirAll(dir, 0700)
}
func (l *Local) UploadPart(ctx context.Context, key, id string, part int32, body io.Reader, size int64) (ObjectInfo, error) {
	dir, err := multipartDir(key, id)
	if err != nil {
		return ObjectInfo{}, err
	}
	if part < 1 || part > 10000 {
		return ObjectInfo{}, errors.New("invalid part number")
	}
	if _, err = l.root.Stat(dir); err != nil {
		return ObjectInfo{}, err
	}
	return l.write(ctx, dir+"/"+strconv.Itoa(int(part)), body, size, false)
}
func (l *Local) CompleteMultipart(ctx context.Context, key, id string, parts []CompletedPart) (ObjectInfo, error) {
	dir, err := multipartDir(key, id)
	if err != nil {
		return ObjectInfo{}, err
	}
	if len(parts) == 0 || len(parts) > 10000 {
		return ObjectInfo{}, errors.New("invalid part count")
	}
	var size int64
	for i, p := range parts {
		if p.PartNumber != int32(i+1) {
			return ObjectInfo{}, errors.New("parts must be consecutive")
		}
		info, err := l.HeadObject(ctx, dir+"/"+strconv.Itoa(i+1))
		if err != nil {
			return ObjectInfo{}, err
		}
		if strings.Trim(p.ETag, "\"") != info.ETag {
			return ObjectInfo{}, errors.New("part ETag mismatch")
		}
		size += info.Size
	}
	reader, writer := io.Pipe()
	go func() {
		var copyErr error
		for i := range parts {
			f, err := l.Open(ctx, dir+"/"+strconv.Itoa(i+1))
			if err != nil {
				copyErr = err
				break
			}
			_, err = io.Copy(writer, contextReader{ctx, f})
			f.Close()
			if err != nil {
				copyErr = err
				break
			}
		}
		writer.CloseWithError(copyErr)
	}()
	info, err := l.write(ctx, key, reader, size, false)
	reader.CloseWithError(err)
	if err == nil {
		_ = l.AbortMultipart(ctx, key, id)
	}
	return info, err
}
func (l *Local) AbortMultipart(ctx context.Context, key, id string) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	dir, err := multipartDir(key, id)
	if err != nil {
		return err
	}
	if err = l.root.RemoveAll(dir); err != nil {
		return err
	}
	_ = l.root.Remove(path.Dir(dir))
	return nil
}

// Prune interrupted temporary writes only after upload sessions have expired.
func (l *Local) CleanupTemporary(ctx context.Context, age time.Duration) error {
	return fs.WalkDir(l.root.FS(), ".", func(name string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if err = ctx.Err(); err != nil {
			return err
		}
		if entry.IsDir() && strings.HasPrefix(name, ".multipart/") && strings.Count(name, "/") == 1 {
			info, err := entry.Info()
			if err != nil {
				return err
			}
			if time.Since(info.ModTime()) > age {
				if err := l.root.RemoveAll(name); err != nil {
					return err
				}
				return fs.SkipDir
			}
		}
		if entry.IsDir() || !strings.HasPrefix(entry.Name(), ".upload-") {
			return nil
		}
		info, err := entry.Info()
		if err != nil {
			return err
		}
		if time.Since(info.ModTime()) > age {
			return l.root.Remove(name)
		}
		return nil
	})
}

var _ Storage = (*Local)(nil)
