package server

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"image"
	"image/jpeg"
	"io"
	"net/http"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/go-chi/chi/v5"
	_ "golang.org/x/image/bmp"
	xdraw "golang.org/x/image/draw"
	_ "golang.org/x/image/webp"
)

// 持久化缩略图：图片与 EPUB 封面由 Go 重采样、视频由 ffmpeg 抽帧，
// 统一存入 S3 的 thumbs/ 前缀（内容寻址、条件写入），前端用带 etag 的
// 不可变 URL 请求，浏览器可长期缓存，刷新/重进目录不再重新加载。

const maxThumbBytes = 512 << 10 // 缩略图对象上限
const maxThumbSource = 64 << 20 // 生成缩略图时允许读取的源文件上限
const thumbMaxDim = 480         // 缩略图最长边

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

// ffmpeg 抽帧全局并发上限：目录里多个视频首次生成时不至于打满 CPU/磁盘。
var videoThumbSlots = make(chan struct{}, 1)
var imageThumbSlots = make(chan struct{}, 2)

// thumbnailKey 由不可见的 object key 派生；保存新内容会换 blob key，
// 复制/移动仍复用同一 key，因此缩略图缓存语义保持稳定。
// GC 也使用这个纯函数从数据库引用重建可达缩略图集合。
func thumbnailKey(objectKey string) string {
	sum := sha256.Sum256([]byte(objectKey + "|thumb-v2"))
	id := hex.EncodeToString(sum[:])
	return "thumbs/" + id[:2] + "/" + id[2:] + ".jpg"
}

func (s *Server) thumbKey(f File) string { return thumbnailKey(f.objectKey) }

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

// thumbnail GET：已有缩略图直接返回（长期缓存）；否则按类型生成一次并落盘。
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
	data, ok := s.generateThumb(r.Context(), f)
	if !ok {
		problem(w, http.StatusNotFound, "no thumbnail available")
		return
	}
	_ = s.storage.PutImmutable(r.Context(), key, "image/jpeg", data)
	serveThumb(w, r, data)
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
	if _, err := exec.LookPath(s.cfg.FFmpegPath); err != nil {
		return nil, false // 无 ffmpeg：回退前端抽帧上传
	}
	select {
	case videoThumbSlots <- struct{}{}:
		defer func() { <-videoThumbSlots }()
	case <-ctx.Done():
		return nil, false
	}
	genCtx, cancel := context.WithTimeout(ctx, 5*time.Minute)
	defer cancel()
	sourceURL, closeSource, err := s.startMediaHLSSource(genCtx, f)
	if err != nil {
		return nil, false
	}
	defer closeSource()
	cmd := exec.CommandContext(genCtx, s.cfg.FFmpegPath,
		"-hide_banner", "-loglevel", "error",
		"-ss", "1", "-i", sourceURL,
		"-frames:v", "1",
		"-vf", fmt.Sprintf("scale='min(%d,iw)':-2", thumbMaxDim),
		"-f", "image2", "-")
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, false
	}
	if err := cmd.Start(); err != nil {
		return nil, false
	}
	data, err := io.ReadAll(io.LimitReader(stdout, maxThumbBytes+1))
	if err != nil {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		return nil, false
	}
	// 输出达到上限说明异常（正常缩略图远小于此）：立即终止 ffmpeg，
	// 否则它会继续写满 stdout 管道并阻塞到 CommandContext 超时。
	if len(data) > maxThumbBytes {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		return nil, false
	}
	if err := cmd.Wait(); err != nil {
		return nil, false
	}
	if len(data) < 2 || data[0] != 0xFF || data[1] != 0xD8 {
		return nil, false
	}
	return data, true
}

// saveThumbnail PUT：接收前端抽帧生成的视频缩略图（小 JPEG），落盘到
// 内容寻址的 thumbs/ 对象，之后的请求直接命中。
func (s *Server) saveThumbnail(w http.ResponseWriter, r *http.Request) {
	f, err := s.readableFile(r.Context(), chi.URLParam(r, "id"))
	if err != nil || f.Kind != "file" || f.Status != "ready" {
		problem(w, http.StatusNotFound, "ready file not found")
		return
	}
	if r.ContentLength > maxThumbBytes {
		problem(w, http.StatusRequestEntityTooLarge, "thumbnail is too large")
		return
	}
	data, err := io.ReadAll(io.LimitReader(r.Body, maxThumbBytes+1))
	if err != nil || len(data) > maxThumbBytes {
		problem(w, http.StatusBadRequest, "thumbnail data is invalid")
		return
	}
	if len(data) < 3 || data[0] != 0xFF || data[1] != 0xD8 || data[2] != 0xFF {
		problem(w, http.StatusBadRequest, "thumbnail must be a JPEG image")
		return
	}
	normalized, err := resizeToJPEG(data, thumbMaxDim)
	if err != nil || len(normalized) > maxThumbBytes {
		problem(w, http.StatusBadRequest, "thumbnail data is invalid")
		return
	}
	if err := s.storage.PutImmutable(r.Context(), s.thumbKey(f), "image/jpeg", normalized); err != nil {
		s.log.Warn("thumbnail upload failed", "file", f.ID, "error", err)
		problem(w, http.StatusBadGateway, "could not store thumbnail")
		return
	}
	w.WriteHeader(http.StatusNoContent)
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
