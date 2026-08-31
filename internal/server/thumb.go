package server

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"image"
	"image/jpeg"
	"net/http"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/VesperGlow/revaro/internal/storage"
	"github.com/go-chi/chi/v5"
	_ "golang.org/x/image/bmp"
	xdraw "golang.org/x/image/draw"
	_ "golang.org/x/image/webp"
)

// 持久化缩略图：图片与 EPUB 封面由 Go 重采样、视频由 Rust/libav 抽帧，
// 统一存入 S3 的 thumbs/ 前缀（内容寻址、条件写入），前端用带 etag 的
// 不可变 URL 请求，浏览器可长期缓存，刷新/重进目录不再重新加载。

const maxThumbBytes = 512 << 10 // 缩略图对象上限
const maxThumbSource = 64 << 20 // 生成缩略图时允许读取的源文件上限
const thumbMaxDim = 640         // 缩略图最长边

// maxThumbPixels caps the pixel dimensions of sources we decode: a small
// compressed file (bounded by maxThumbSource) can otherwise expand into
// gigabytes of bitmap memory during decode (decompression bomb).
const maxThumbPixels = 40_000_000 // 40 MP（约 160 MB RGBA）
const maxThumbSide = 30_000       // 单边像素上限（极端长条图）

var videoExts = map[string]bool{
	".mp4": true, ".webm": true, ".mov": true, ".m4v": true, ".mkv": true,
	".avi": true, ".ogv": true, ".mpg": true, ".mpeg": true, ".wmv": true, ".flv": true,
	".ts": true, ".m2ts": true, ".mts": true,
}

var imageThumbSlots = make(chan struct{}, 2)

type thumbnailScheduler struct {
	ctx    context.Context
	cancel context.CancelFunc
	slots  chan struct{}
	mu     sync.Mutex
	active map[string]struct{}
	closed bool
	wg     sync.WaitGroup
}

func newThumbnailScheduler(limit int) *thumbnailScheduler {
	ctx, cancel := context.WithCancel(context.Background())
	return &thumbnailScheduler{ctx: ctx, cancel: cancel, slots: make(chan struct{}, limit), active: make(map[string]struct{})}
}

func (q *thumbnailScheduler) schedule(key string, work func(context.Context)) bool {
	q.mu.Lock()
	if q.closed {
		q.mu.Unlock()
		return false
	}
	if _, exists := q.active[key]; exists {
		q.mu.Unlock()
		return false
	}
	q.active[key] = struct{}{}
	q.wg.Add(1)
	q.mu.Unlock()
	go func() {
		defer q.wg.Done()
		defer func() {
			q.mu.Lock()
			delete(q.active, key)
			q.mu.Unlock()
		}()
		select {
		case q.slots <- struct{}{}:
			defer func() { <-q.slots }()
		case <-q.ctx.Done():
			return
		}
		work(q.ctx)
	}()
	return true
}

func (q *thumbnailScheduler) close() {
	q.mu.Lock()
	q.closed = true
	q.cancel()
	q.mu.Unlock()
	q.wg.Wait()
}

// thumbnailKey 由不可见的 object key 派生；保存新内容会换 blob key，
// 复制/移动仍复用同一 key，因此缩略图缓存语义保持稳定。
// GC 也使用这个纯函数从数据库引用重建可达缩略图集合。
func thumbnailKey(objectKey string) string {
	sum := sha256.Sum256([]byte(objectKey + "|thumb-v2"))
	id := hex.EncodeToString(sum[:])
	return "thumbs/" + id[:2] + "/" + id[2:] + ".jpg"
}

func imageThumbnailKey(objectKey string) string {
	return derivedThumbnailKey(objectKey, "image-thumb-v1")
}

func audioThumbnailKey(objectKey string) string {
	return derivedThumbnailKey(objectKey, "audio-thumb-v1")
}

func videoThumbnailKey(objectKey string) string {
	return derivedThumbnailKey(objectKey, "video-thumb-v3")
}

func derivedThumbnailKey(objectKey, namespace string) string {
	sum := sha256.Sum256([]byte(objectKey + "|" + namespace))
	id := hex.EncodeToString(sum[:])
	return "thumbs/" + id[:2] + "/" + id[2:] + ".jpg"
}

func (s *Server) thumbKey(f File) string {
	if videoExts[strings.ToLower(filepath.Ext(f.Name))] {
		return videoThumbnailKey(f.objectKey)
	}
	if isAudioSource(f) {
		return audioThumbnailKey(f.objectKey)
	}
	return imageThumbnailKey(f.objectKey)
}

func canHaveThumbnail(name string) bool {
	ext := strings.ToLower(filepath.Ext(name))
	return ext == ".epub" || ext == ".jpg" || ext == ".jpeg" || ext == ".png" ||
		ext == ".gif" || ext == ".webp" || ext == ".bmp" || videoExts[ext]
}

func serveThumb(w http.ResponseWriter, r *http.Request, data []byte) {
	w.Header().Set("Content-Type", "image/jpeg")
	w.Header().Set("Content-Length", strconv.Itoa(len(data)))
	w.Header().Set("Cache-Control", "private, max-age=31536000, immutable")
	w.Header().Set("ETag", `"`+hexSHA256(data)+`"`)
	if r.Method == http.MethodHead {
		w.WriteHeader(http.StatusOK)
		return
	}
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(data)
}

func hexSHA256(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}

// thumbnail GET：已有缩略图直接返回。图片和书封仍按原逻辑生成；视频只
// 投递有限并发后台任务，本次请求安全返回 404，绝不等待媒体解码。
func (s *Server) thumbnail(w http.ResponseWriter, r *http.Request) {
	f, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || f.Kind != "file" || f.Status != "ready" {
		problem(w, http.StatusNotFound, "ready file not found")
		return
	}
	key := s.thumbKey(f)
	if data, err := s.storage.GetObject(r.Context(), key, maxThumbBytes); err == nil {
		serveThumb(w, r, data)
		return
	}
	// Read the pre-namespace v2 key once for backwards compatibility. Copying
	// it into the typed namespace lets subsequent GC treat it as disposable.
	if !videoExts[strings.ToLower(filepath.Ext(f.Name))] {
		if data, err := s.storage.GetObject(r.Context(), thumbnailKey(f.objectKey), maxThumbBytes); err == nil {
			_ = s.storage.PutImmutable(r.Context(), key, "image/jpeg", data)
			serveThumb(w, r, data)
			return
		}
	}
	if isAudioSource(f) {
		s.audioThumbnail(w, r, f, key)
		return
	}
	if videoExts[strings.ToLower(filepath.Ext(f.Name))] {
		s.scheduleVideoThumbnail(f)
		problem(w, http.StatusNotFound, "thumbnail is being generated")
		return
	}
	data, ok := s.generateThumb(r.Context(), f)
	if !ok {
		problem(w, http.StatusNotFound, "no thumbnail available")
		return
	}
	_ = s.storage.PutImmutable(r.Context(), key, "image/jpeg", data)
	serveThumb(w, r, data)
}

func (s *Server) audioThumbnail(w http.ResponseWriter, r *http.Request, f File, key string) {
	var hasCover bool
	if err := s.db.QueryRowContext(r.Context(), `SELECT has_cover FROM audio_media WHERE file_id=?`, f.ID).Scan(&hasCover); err != nil || !hasCover {
		problem(w, http.StatusNotFound, "audio has no cover")
		return
	}
	result := s.audioThumbGroup.DoChan(f.ID+":"+f.ETag, func() (any, error) {
		ctx, cancel := context.WithTimeout(s.audioHLSCtx, 5*time.Minute)
		defer cancel()
		if !acquireThumbSlot(ctx, s.audioThumbSlots) {
			return nil, ctx.Err()
		}
		defer func() { <-s.audioThumbSlots }()
		// Another request may have filled the cache while this call waited.
		if data, err := s.storage.GetObject(ctx, key, maxThumbBytes); err == nil {
			return data, nil
		}
		data, err := s.generateAudioCover(ctx, f)
		if errors.Is(err, storage.ErrNoCover) {
			_, _ = s.db.ExecContext(ctx, `UPDATE audio_media SET has_cover=0,updated_at=? WHERE file_id=?`, time.Now().UTC().Format(time.RFC3339Nano), f.ID)
			return nil, err
		}
		if err != nil {
			return nil, err
		}
		if len(data) > maxThumbBytes || len(data) < 2 || data[0] != 0xFF || data[1] != 0xD8 {
			return nil, errors.New("audio cover generator returned an invalid JPEG")
		}
		if err := s.storage.PutImmutable(ctx, key, "image/jpeg", data); err != nil {
			return nil, err
		}
		return data, nil
	})
	select {
	case <-r.Context().Done():
		return
	case generated := <-result:
		if generated.Err != nil {
			if !errors.Is(generated.Err, storage.ErrNoCover) && !errors.Is(generated.Err, context.Canceled) {
				s.log.Warn("audio cover regeneration failed", "file", f.ID, "error", generated.Err)
			}
			problem(w, http.StatusNotFound, "audio cover is unavailable")
			return
		}
		serveThumb(w, r, generated.Val.([]byte))
	}
}

func (s *Server) generateThumb(ctx context.Context, f File) ([]byte, bool) {
	ext := strings.ToLower(filepath.Ext(f.Name))
	switch {
	case ext == ".epub":
		if !acquireThumbSlot(ctx, imageThumbSlots) {
			return nil, false
		}
		defer func() { <-imageThumbSlots }()
		book, err := s.loadBook(ctx, f)
		if err != nil || len(book.Cover) == 0 {
			return nil, false
		}
		if resized, err := resizeToJPEG(book.Cover, thumbMaxDim); err == nil {
			return resized, true
		}
		return nil, false
	case ext == ".jpg" || ext == ".jpeg" || ext == ".png" || ext == ".gif" || ext == ".webp" || ext == ".bmp":
		if !acquireThumbSlot(ctx, imageThumbSlots) {
			return nil, false
		}
		defer func() { <-imageThumbSlots }()
		raw, err := s.storage.GetObject(ctx, f.objectKey, maxThumbSource)
		if err != nil || len(raw) == 0 {
			return nil, false
		}
		resized, err := resizeToJPEG(raw, thumbMaxDim)
		if err != nil {
			return nil, false
		}
		return resized, true
	case videoExts[ext]:
		return s.generateVideoThumb(ctx, f)
	}
	return nil, false
}

func acquireThumbSlot(ctx context.Context, slots chan struct{}) bool {
	select {
	case slots <- struct{}{}:
		return true
	case <-ctx.Done():
		return false
	}
}

// generateVideoThumb uses the same source selection as playback: direct S3
// Range for blobs and the local compatibility Reader for legacy manifests.
func (s *Server) generateVideoThumb(ctx context.Context, f File) ([]byte, bool) {
	if f.Size <= 0 {
		return nil, false
	}
	engine, ok := s.storage.(storage.MediaEngine)
	if !ok {
		return nil, false
	}
	genCtx, cancel := context.WithTimeout(ctx, 5*time.Minute)
	defer cancel()
	data, err := engine.MediaThumbnail(genCtx, f.objectKey, thumbMaxDim)
	if err != nil || len(data) > maxThumbBytes {
		return nil, false
	}
	if len(data) < 2 || data[0] != 0xFF || data[1] != 0xD8 {
		return nil, false
	}
	return data, true
}

func (s *Server) scheduleVideoThumbnail(f File) {
	if s.thumbnails == nil || f.Kind != "file" || f.Status != "ready" || f.objectKey == "" || !videoExts[strings.ToLower(filepath.Ext(f.Name))] {
		return
	}
	cacheKey := s.thumbKey(f)
	s.thumbnails.schedule(cacheKey, func(ctx context.Context) {
		// A request may have raced another producer (for example an import hook).
		if _, err := s.storage.GetObject(ctx, cacheKey, maxThumbBytes); err == nil {
			return
		}
		data, ok := s.generateVideoThumb(ctx, f)
		if !ok {
			return
		}
		if err := s.storage.PutImmutable(ctx, cacheKey, "image/jpeg", data); err != nil && !errors.Is(err, context.Canceled) {
			s.log.Warn("video thumbnail storage failed", "file", f.ID, "error", err)
		}
	})
}

// resizeToJPEG 解码任意受支持的图片并把最长边缩到 maxDim，输出 JPEG。
// 解码前先用 DecodeConfig 检查像素尺寸，拒绝会撑爆内存的超大图。
func resizeToJPEG(data []byte, maxDim int) ([]byte, error) {
	cfg, _, err := image.DecodeConfig(bytes.NewReader(data))
	if err != nil {
		return nil, err
	}
	if cfg.Width <= 0 || cfg.Height <= 0 ||
		int64(cfg.Width)*int64(cfg.Height) > maxThumbPixels ||
		cfg.Width > maxThumbSide || cfg.Height > maxThumbSide {
		return nil, errImageTooLarge
	}
	src, _, err := image.Decode(bytes.NewReader(data))
	if err != nil {
		return nil, err
	}
	bounds := src.Bounds()
	width, height := bounds.Dx(), bounds.Dy()
	if width <= 0 || height <= 0 {
		return nil, errEmptyImage
	}
	if width <= maxDim && height <= maxDim {
		return encodeJPEG(src)
	}
	longest := width
	if height > longest {
		longest = height
	}
	scale := float64(maxDim) / float64(longest)
	dw := maxInt(1, int(float64(width)*scale))
	dh := maxInt(1, int(float64(height)*scale))
	dst := image.NewRGBA(image.Rect(0, 0, dw, dh))
	xdraw.CatmullRom.Scale(dst, dst.Bounds(), src, bounds, xdraw.Over, nil)
	return encodeJPEG(dst)
}

var errEmptyImage = errors.New("empty image")
var errImageTooLarge = errors.New("image exceeds pixel limit")

func encodeJPEG(img image.Image) ([]byte, error) {
	var buf bytes.Buffer
	if err := jpeg.Encode(&buf, img, &jpeg.Options{Quality: 82}); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

func maxInt(a, b int) int {
	if a > b {
		return a
	}
	return b
}
