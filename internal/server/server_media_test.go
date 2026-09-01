package server

import (
	"bytes"
	"context"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/storage"
)

func TestResponseMimeRecognizesYAML(t *testing.T) {
	got := responseMime(File{Name: "profile.yaml", MimeType: "application/octet-stream"})
	if got != "application/yaml; charset=utf-8" {
		t.Fatalf("yaml content type=%q", got)
	}
}

func TestMediaPreviewAndStorageStats(t *testing.T) {
	a := newTestApp(t)
	files := []struct {
		name, mime string
		size       int64
	}{
		{"clip.mp4", "application/octet-stream", 2048},
		{"song.wav", "application/octet-stream", 4096},
		{"animated.gif", "image/gif", 1024},
	}
	created := make([]File, 0, len(files))
	for _, f := range files {
		created = append(created, a.readyFile(t, f.name, bytes.Repeat([]byte("x"), int(f.size))))
	}
	folderRR := a.request("POST", "/api/directories", map[string]any{"parent_id": RootID, "name": "Nested"}, true)
	folder := decode[File](t, folderRR)
	if moved := a.request("PATCH", "/api/files/"+created[1].ID, map[string]any{"parent_id": folder.ID}, true); moved.Code != http.StatusOK {
		t.Fatalf("move nested media=%d: %s", moved.Code, moved.Body.String())
	}
	rr := a.request("GET", "/api/files/"+RootID+"/children", nil, true)
	if rr.Code != http.StatusOK {
		t.Fatalf("children=%d: %s", rr.Code, rr.Body.String())
	}
	items := decode[struct {
		Items      []File `json:"items"`
		TotalBytes int64  `json:"total_bytes"`
		FileCount  int64  `json:"file_count"`
	}](t, rr)
	if items.TotalBytes != 7168 || items.FileCount != 3 {
		t.Fatalf("recursive directory stats=%+v", items)
	}
	for _, item := range items.Items {
		if item.Kind != "file" {
			continue
		}
		preview := a.request("GET", "/api/files/"+item.ID+"/preview", nil, true)
		if preview.Code != http.StatusOK {
			t.Fatalf("preview %s=%d: %s", item.Name, preview.Code, preview.Body.String())
		}
	}
	if got := responseMime(File{Name: "song.mp3", MimeType: "application/octet-stream"}); got != "audio/mpeg" {
		t.Fatalf("mp3 content type=%q", got)
	}
	statsRR := a.request("GET", "/api/storage/stats", nil, true)
	if statsRR.Code != http.StatusOK {
		t.Fatalf("storage stats=%d: %s", statsRR.Code, statsRR.Body.String())
	}
	stats := decode[struct {
		TotalBytes int64 `json:"total_bytes"`
		FileCount  int64 `json:"file_count"`
	}](t, statsRR)
	if stats.TotalBytes != 7168 || stats.FileCount != 3 {
		t.Fatalf("storage stats=%+v", stats)
	}
}

func TestSafeDeliveryMimeBlocksActiveWebContent(t *testing.T) {
	for _, value := range []string{"text/html; charset=utf-8", "image/svg+xml", "application/javascript", "application/xhtml+xml"} {
		if got := safeDeliveryMime(value); got != "application/octet-stream" {
			t.Fatalf("safeDeliveryMime(%q)=%q", value, got)
		}
	}
	if got := safeDeliveryMime("image/png"); got != "image/png" {
		t.Fatalf("safe image MIME changed to %q", got)
	}
}

func TestBookEndpointsTXT(t *testing.T) {
	a := newTestApp(t)
	content := []byte("第一章 开始\n正文一行\n第二章 继续\n")
	f := a.readyFile(t, "book.txt", content)
	info := a.request("GET", "/api/files/"+f.ID+"/book", nil, true)
	if info.Code != http.StatusOK {
		t.Fatalf("book info=%d: %s", info.Code, info.Body.String())
	}
	meta := decode[struct {
		Format string `json:"format"`
		Title  string `json:"title"`
		Cover  bool   `json:"cover"`
		TOC    []struct {
			Label  string `json:"label"`
			Offset int64  `json:"offset"`
		} `json:"toc"`
	}](t, info)
	if meta.Format != "txt" || meta.Cover || len(meta.TOC) != 2 || meta.TOC[0].Label != "第一章 开始" {
		t.Fatalf("meta=%+v", meta)
	}
	body := a.request("GET", "/api/files/"+f.ID+"/book/content", nil, true)
	if body.Code != http.StatusOK {
		t.Fatalf("content=%d: %s", body.Code, body.Body.String())
	}
	model := decode[struct {
		Kind string `json:"kind"`
		Text string `json:"text"`
	}](t, body)
	if model.Kind != "txt" || model.Text != string(content) {
		t.Fatalf("model=%+v", model)
	}
	put := a.request("PUT", "/api/files/"+f.ID+"/book/progress", map[string]any{"page": 3, "total_pages": 10}, true)
	if put.Code != http.StatusNoContent {
		t.Fatalf("save progress=%d: %s", put.Code, put.Body.String())
	}
	got := a.request("GET", "/api/files/"+f.ID+"/book/progress", nil, true)
	progress := decode[struct {
		Page       int64  `json:"page"`
		TotalPages *int64 `json:"total_pages"`
	}](t, got)
	if progress.Page != 3 || progress.TotalPages == nil || *progress.TotalPages != 10 {
		t.Fatalf("progress=%+v", progress)
	}
	pdf := a.readyFile(t, "doc.pdf", []byte("x"))
	if rr := a.request("GET", "/api/files/"+pdf.ID+"/book", nil, true); rr.Code != http.StatusUnsupportedMediaType {
		t.Fatalf("pdf book=%d: %s", rr.Code, rr.Body.String())
	}
}

func TestBookEndpointsEPUB(t *testing.T) {
	a := newTestApp(t)
	fixture := buildEPUB(t)
	f := a.readyFile(t, "book.epub", fixture)
	info := a.request("GET", "/api/files/"+f.ID+"/book", nil, true)
	if info.Code != http.StatusOK {
		t.Fatalf("book info=%d: %s", info.Code, info.Body.String())
	}
	meta := decode[struct {
		Format string `json:"format"`
		Title  string `json:"title"`
		Cover  bool   `json:"cover"`
	}](t, info)
	if meta.Format != "epub" || meta.Title != "测试书" || !meta.Cover {
		t.Fatalf("meta=%+v", meta)
	}
	body := a.request("GET", "/api/files/"+f.ID+"/book/content", nil, true)
	if body.Code != http.StatusOK {
		t.Fatalf("content=%d: %s", body.Code, body.Body.String())
	}
	model := decode[struct {
		Kind     string `json:"kind"`
		Chapters []struct {
			HTML string `json:"html"`
		} `json:"chapters"`
	}](t, body)
	if model.Kind != "epub" || len(model.Chapters) != 1 || !strings.Contains(model.Chapters[0].HTML, "你好世界") || strings.Contains(model.Chapters[0].HTML, "<script") {
		t.Fatalf("chapters=%+v", model.Chapters)
	}
	asset := a.request("GET", "/api/files/"+f.ID+"/book/assets/0", nil, true)
	if asset.Code != http.StatusOK || asset.Header().Get("Content-Type") != "image/png" || asset.Body.Len() != 33 {
		t.Fatalf("asset=%d type=%q bytes=%d", asset.Code, asset.Header().Get("Content-Type"), asset.Body.Len())
	}
	cover := a.request("GET", "/api/files/"+f.ID+"/book/cover", nil, true)
	if cover.Code != http.StatusOK || cover.Header().Get("Content-Type") != "image/png" || cover.Body.Len() == 0 {
		t.Fatalf("cover=%d type=%q bytes=%d", cover.Code, cover.Header().Get("Content-Type"), cover.Body.Len())
	}
	if missing := a.request("GET", "/api/files/"+f.ID+"/book/assets/9", nil, true); missing.Code != http.StatusNotFound {
		t.Fatalf("missing asset=%d", missing.Code)
	}
}

func TestThumbnails(t *testing.T) {
	a := newTestApp(t)

	// 图片：服务端生成 JPEG 缩略图并持久化到 S3（内容寻址），缓存头可长期缓存。
	photo := a.readyFile(t, "photo.png", realPNG(t, 640, 320))
	rr := a.request("GET", "/api/files/"+photo.ID+"/thumbnail", nil, true)
	if rr.Code != http.StatusOK || rr.Header().Get("Content-Type") != "image/jpeg" {
		t.Fatalf("thumb=%d type=%q", rr.Code, rr.Header().Get("Content-Type"))
	}
	if !bytes.HasPrefix(rr.Body.Bytes(), []byte{0xFF, 0xD8}) {
		t.Fatalf("thumb is not a JPEG: % x", rr.Body.Bytes()[:4])
	}
	if cc := rr.Header().Get("Cache-Control"); !strings.Contains(cc, "immutable") {
		t.Fatalf("cache-control=%q", cc)
	}
	// 第二次请求应直接命中已落盘的缩略图对象
	if again := a.request("GET", "/api/files/"+photo.ID+"/thumbnail", nil, true); again.Code != http.StatusOK || !bytes.Equal(again.Body.Bytes(), rr.Body.Bytes()) {
		t.Fatal("cached thumbnail not served consistently")
	}

	// 视频：缓存未命中时只排入后台队列，请求本身不等待解码。
	video := a.readyFile(t, "clip.mp4", []byte("video-bytes"))
	if missing := a.request("GET", "/api/files/"+video.ID+"/thumbnail", nil, true); missing.Code != http.StatusNotFound {
		t.Fatalf("missing video thumb=%d", missing.Code)
	}
	if put := a.rawRequest("PUT", "/api/files/"+video.ID+"/thumbnail", realJPEG(t, 48, 27), true); put.Code != http.StatusMethodNotAllowed {
		t.Fatalf("client thumbnail upload remains enabled: %d", put.Code)
	}
	// The invalid-video background attempt reads mockStorage's in-memory map;
	// let it unwind before this test adds the legacy audio-cover fixture.
	a.srv.thumbnails.close()

	// 音频封面沿用原有通用 v2 缓存，不受视频抽帧缓存版本影响。
	audio := a.readyFile(t, "asmr.flac", []byte("audio-bytes"))
	audioCover := realJPEG(t, 64, 64)
	a.store.raw[thumbnailKey(audio.objectKey)] = audioCover
	audioThumb := a.request("GET", "/api/files/"+audio.ID+"/thumbnail", nil, true)
	if audioThumb.Code != http.StatusOK || !bytes.Equal(audioThumb.Body.Bytes(), audioCover) {
		t.Fatalf("existing audio cover was not served: status=%d", audioThumb.Code)
	}
	if imageThumbnailKey(video.objectKey) == audioThumbnailKey(video.objectKey) || imageThumbnailKey(video.objectKey) == videoThumbnailKey(video.objectKey) || audioThumbnailKey(video.objectKey) == videoThumbnailKey(video.objectKey) {
		t.Fatal("typed thumbnail cache namespaces overlap")
	}
	now := time.Now().UTC().Format(time.RFC3339Nano)
	if _, err := a.db.Exec(`INSERT INTO audio_media(file_id,duration_ms,chapters_json,stream_object_key,stream_size,stream_etag,has_cover,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?)`, audio.ID, 1000, `[]`, audio.objectKey, audio.Size, audio.ETag, true, now, now); err != nil {
		t.Fatal(err)
	}
	plainAudio := a.readyFile(t, "plain.mp3", []byte("plain-audio"))
	children := a.request("GET", "/api/files/"+RootID+"/children", nil, true)
	listing := decode[struct {
		Items []File `json:"items"`
	}](t, children)
	coverFlags := make(map[string]bool)
	for _, item := range listing.Items {
		coverFlags[item.ID] = item.HasCover
	}
	if !coverFlags[audio.ID] || coverFlags[plainAudio.ID] {
		t.Fatalf("audio cover flags: covered=%v plain=%v", coverFlags[audio.ID], coverFlags[plainAudio.ID])
	}

	// EPUB：缩略图 = 缩小后的内嵌封面。
	epub := a.readyFile(t, "book.epub", buildEPUB(t))
	et := a.request("GET", "/api/files/"+epub.ID+"/thumbnail", nil, true)
	if et.Code != http.StatusOK || !bytes.HasPrefix(et.Body.Bytes(), []byte{0xFF, 0xD8}) {
		t.Fatalf("epub thumb=%d", et.Code)
	}

	// 无封面/不支持的类型返回 404，前端回退原图或图标。
	txt := a.readyFile(t, "notes.txt", []byte("hello"))
	if rr := a.request("GET", "/api/files/"+txt.ID+"/thumbnail", nil, true); rr.Code != http.StatusNotFound {
		t.Fatalf("txt thumb=%d", rr.Code)
	}
}

func TestAudioThumbnailCacheSelfHealing(t *testing.T) {
	a := newTestApp(t)
	audio := a.readyFile(t, "recoverable.m4a", []byte("audio-source"))
	now := time.Now().UTC().Format(time.RFC3339Nano)
	if _, err := a.db.Exec(`INSERT INTO audio_media(file_id,duration_ms,chapters_json,stream_object_key,stream_size,stream_etag,has_cover,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?)`, audio.ID, 1000, `[]`, audio.objectKey, audio.Size, audio.ETag, true, now, now); err != nil {
		t.Fatal(err)
	}
	cover := realJPEG(t, 80, 80)
	key := audioThumbnailKey(audio.objectKey)
	var generated atomic.Int32
	a.srv.generateAudioCover = func(context.Context, File) ([]byte, error) {
		generated.Add(1)
		return cover, nil
	}

	a.store.raw[key] = cover
	if rr := a.request("GET", "/api/files/"+audio.ID+"/thumbnail", nil, true); rr.Code != http.StatusOK {
		t.Fatalf("audio cache hit=%d", rr.Code)
	}
	if generated.Load() != 0 {
		t.Fatal("audio cover regenerated on cache hit")
	}

	delete(a.store.raw, key)
	if rr := a.request("GET", "/api/files/"+audio.ID+"/thumbnail", nil, true); rr.Code != http.StatusOK {
		t.Fatalf("audio cache miss=%d: %s", rr.Code, rr.Body.String())
	}
	delete(a.store.raw, key)
	if rr := a.request("GET", "/api/files/"+audio.ID+"/thumbnail", nil, true); rr.Code != http.StatusOK {
		t.Fatalf("audio cache re-delete=%d", rr.Code)
	}
	if generated.Load() != 2 {
		t.Fatalf("audio generations=%d", generated.Load())
	}

	delete(a.store.raw, key)
	started := make(chan struct{})
	release := make(chan struct{})
	a.srv.generateAudioCover = func(context.Context, File) ([]byte, error) {
		generated.Add(1)
		select {
		case <-started:
		default:
			close(started)
		}
		<-release
		return cover, nil
	}
	const callers = 6
	responses := make(chan int, callers)
	for range callers {
		go func() {
			req := httptest.NewRequest(http.MethodGet, "/", nil)
			rr := httptest.NewRecorder()
			a.srv.audioThumbnail(rr, req, audio, key)
			responses <- rr.Code
		}()
	}
	<-started
	close(release)
	for range callers {
		if code := <-responses; code != http.StatusOK {
			t.Fatalf("concurrent audio thumbnail=%d", code)
		}
	}
	if generated.Load() != 3 {
		t.Fatalf("concurrent requests generated %d total covers", generated.Load())
	}

	delete(a.store.raw, key)
	a.srv.generateAudioCover = func(context.Context, File) ([]byte, error) {
		generated.Add(1)
		return nil, storage.ErrNoCover
	}
	if rr := a.request("GET", "/api/files/"+audio.ID+"/thumbnail", nil, true); rr.Code != http.StatusNotFound {
		t.Fatalf("stale cover metadata response=%d", rr.Code)
	}
	var hasCover bool
	if err := a.db.QueryRow(`SELECT has_cover FROM audio_media WHERE file_id=?`, audio.ID).Scan(&hasCover); err != nil || hasCover {
		t.Fatalf("stale has_cover was not repaired: value=%v err=%v", hasCover, err)
	}
	before := generated.Load()
	if rr := a.request("GET", "/api/files/"+audio.ID+"/thumbnail", nil, true); rr.Code != http.StatusNotFound {
		t.Fatalf("no-cover response=%d", rr.Code)
	}
	if generated.Load() != before {
		t.Fatal("has_cover=false triggered extraction")
	}
}

func TestVideoThumbnailWithFFmpeg(t *testing.T) {
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		t.Skip("ffmpeg not available")
	}
	a := newTestApp(t)
	a.requireMediaEngine(t)
	// 用 ffmpeg 现场生成一段 2 秒测试视频，确保服务端在第 1 秒抽帧时
	// 不会刚好落在媒体结尾。
	tmp := t.TempDir() + "/test.mp4"
	cmd := exec.Command("ffmpeg", "-hide_banner", "-loglevel", "error",
		"-f", "lavfi", "-i", "testsrc=size=160x90:rate=10", "-t", "2", "-pix_fmt", "yuv420p", tmp)
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("ffmpeg fixture failed: %v %s", err, out)
	}
	data, err := os.ReadFile(tmp)
	if err != nil {
		t.Fatal(err)
	}
	f := a.readyFile(t, "clip.mp4", data)
	rr := a.request("GET", "/api/files/"+f.ID+"/thumbnail", nil, true)
	deadline := time.Now().Add(10 * time.Second)
	for rr.Code == http.StatusNotFound && time.Now().Before(deadline) {
		time.Sleep(50 * time.Millisecond)
		rr = a.request("GET", "/api/files/"+f.ID+"/thumbnail", nil, true)
	}
	if rr.Code != http.StatusOK || rr.Header().Get("Content-Type") != "image/jpeg" {
		t.Fatalf("video thumb=%d type=%q", rr.Code, rr.Header().Get("Content-Type"))
	}
	if !bytes.HasPrefix(rr.Body.Bytes(), []byte{0xFF, 0xD8}) {
		t.Fatalf("video thumb is not a JPEG: % x", rr.Body.Bytes()[:4])
	}
	// 第二次应直接命中持久化对象
	if again := a.request("GET", "/api/files/"+f.ID+"/thumbnail", nil, true); again.Code != http.StatusOK || !bytes.Equal(again.Body.Bytes(), rr.Body.Bytes()) {
		t.Fatal("persisted video thumbnail not served consistently")
	}
}

func TestMediaProgressSync(t *testing.T) {
	a := newTestApp(t)
	audio := a.readyFile(t, "episode.mp3", []byte("audio"))

	empty := a.request("GET", "/api/files/"+audio.ID+"/media/progress", nil, true)
	if empty.Code != http.StatusOK {
		t.Fatalf("empty progress=%d: %s", empty.Code, empty.Body.String())
	}
	put := a.request("PUT", "/api/files/"+audio.ID+"/media/progress", map[string]any{"position": 123.456, "duration": 600}, true)
	if put.Code != http.StatusOK {
		t.Fatalf("save progress=%d: %s", put.Code, put.Body.String())
	}
	got := a.request("GET", "/api/files/"+audio.ID+"/media/progress", nil, true)
	progress := decode[mediaProgressResponse](t, got)
	if progress.Position != 123.456 || progress.Duration != 600 || progress.UpdatedAt == "" {
		t.Fatalf("progress=%+v", progress)
	}
}
