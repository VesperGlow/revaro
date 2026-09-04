package server

import (
	"context"
	"io"
	"net/http"
	"strings"
	"testing"
	"unicode/utf16"

	"github.com/VesperGlow/revaro/internal/reader"
	"github.com/VesperGlow/revaro/internal/reader/flow"
)

// reading flow 服务端契约：manifest/chunk 端点、缓存幂等、进度 anchor、
// 旧 Chromium 分页端点已删除。

// storeFlowPuts 返回写入对象存储的 flows/ 前缀对象键（构建即写入）。
func (a *testApp) storeFlowPuts() []string {
	a.store.mu.RLock()
	defer a.store.mu.RUnlock()
	var out []string
	for _, key := range a.store.putKeys {
		if strings.HasPrefix(key, "flows/") {
			out = append(out, key)
		}
	}
	return out
}

func TestReaderFlowManifestAndChunksEPUB(t *testing.T) {
	a := newTestApp(t)
	f := a.readyFile(t, "book.epub", buildEPUB(t))

	rr := a.request("GET", "/api/files/"+f.ID+"/book/flow", nil, true)
	if rr.Code != http.StatusOK {
		t.Fatalf("flow=%d: %s", rr.Code, rr.Body.String())
	}
	m := decode[flow.Manifest](t, rr)
	if m.Format != "epub" || m.Version != flow.FlowFormatVersion {
		t.Fatalf("manifest meta=%+v", m)
	}
	if m.TotalBlocks() == 0 || len(m.Chunks) == 0 {
		t.Fatalf("empty flow: %+v", m)
	}
	if cc := rr.Header().Get("Cache-Control"); !strings.Contains(cc, "no-cache") {
		t.Fatalf("manifest cache-control=%q", cc)
	}
	// 首个 chunk 可直接拉取：数据块编号从 0 开始且文本完整
	cr := a.request("GET", "/api/files/"+f.ID+"/book/flow/chunks/0", nil, true)
	if cr.Code != http.StatusOK {
		t.Fatalf("chunk0=%d: %s", cr.Code, cr.Body.String())
	}
	body := cr.Body.String()
	if !strings.Contains(body, `data-block="0"`) || !strings.Contains(body, "你好世界") {
		t.Fatalf("chunk0 content missing: %.400s", body)
	}
	if cc := cr.Header().Get("Cache-Control"); !strings.Contains(cc, "immutable") {
		t.Fatalf("chunk cache-control=%q", cc)
	}
	if ct := cr.Header().Get("Content-Type"); !strings.HasPrefix(ct, "text/html") {
		t.Fatalf("chunk content-type=%q", ct)
	}

	// 越界 chunk → 404
	if rr := a.request("GET", "/api/files/"+f.ID+"/book/flow/chunks/99999", nil, true); rr.Code != http.StatusNotFound {
		t.Fatalf("chunk out of range=%d", rr.Code)
	}
	// 非法 index
	if rr := a.request("GET", "/api/files/"+f.ID+"/book/flow/chunks/-1", nil, true); rr.Code != http.StatusBadRequest {
		t.Fatalf("chunk negative=%d", rr.Code)
	}

	// 二次打开（对象缓存命中）→ manifest 逐字节一致
	rr2 := a.request("GET", "/api/files/"+f.ID+"/book/flow", nil, true)
	if rr2.Code != http.StatusOK || rr2.Body.String() != rr.Body.String() {
		t.Fatal("flow manifest not stable across requests")
	}
	cr2 := a.request("GET", "/api/files/"+f.ID+"/book/flow/chunks/0", nil, true)
	if cr2.Body.String() != body {
		t.Fatal("chunk not stable across requests")
	}
	stats := a.srv.cache.Stats()
	manifestStats := stats.Classes[cacheClassReaderFlowManifest]
	chunkStats := stats.Classes[cacheClassReaderFlowChunk]
	if manifestStats.MemoryEntries == 0 || manifestStats.DiskBytes != 0 || manifestStats.DiskEntries != 0 {
		t.Fatalf("manifest cache tiers = %+v", manifestStats)
	}
	if chunkStats.MemoryEntries == 0 || chunkStats.DiskBytes != 0 || chunkStats.DiskEntries != 0 {
		t.Fatalf("chunk cache tiers = %+v", chunkStats)
	}
}

// flow 产物是内容寻址 immutable 缓存：manifest 命中（服务端 memory 副本
// 或对象存储 HEAD 命中）即直接复用，第二次打开同一本书不重新 Build
// flow、不重新写对象。
func TestReaderFlowSecondOpenDoesNotRebuild(t *testing.T) {
	a := newTestApp(t)
	f := a.readyFile(t, "book.epub", buildEPUB(t))
	if rr := a.request("GET", "/api/files/"+f.ID+"/book/flow", nil, true); rr.Code != http.StatusOK {
		t.Fatalf("flow=%d", rr.Code)
	}
	if cr := a.request("GET", "/api/files/"+f.ID+"/book/flow/chunks/0", nil, true); cr.Code != http.StatusOK {
		t.Fatalf("chunk0=%d", cr.Code)
	}
	putsAfterFirst := a.storeFlowPuts()

	// 进程内二次打开：解析缓存与 flow memory cache 均在 → 零构建、零对象写入
	a.srv.books = reader.NewCache(1, 1) // 逐出解析 Book，确保不是靠 Book 缓存蒙混
	if rr := a.request("GET", "/api/files/"+f.ID+"/book/flow", nil, true); rr.Code != http.StatusOK || rr.Body.String() == "" {
		t.Fatalf("second flow=%d", rr.Code)
	}
	if cr := a.request("GET", "/api/files/"+f.ID+"/book/flow/chunks/0", nil, true); cr.Code != http.StatusOK || cr.Body.String() == "" {
		t.Fatalf("second chunk0=%d", cr.Code)
	}
	if got := a.storeFlowPuts(); len(got) != len(putsAfterFirst) {
		t.Fatalf("second open rebuilt flow: puts %d → %d", len(putsAfterFirst), len(got))
	}

	// 「重启」语义：清空服务端 manifest memory cache，manifest 对象仍在
	// 对象存储 → HEAD 命中同样不重建
	if err := a.srv.cache.Delete(cacheClassReaderFlowManifest, flowCacheKey(f.objectKey)); err != nil {
		t.Fatal(err)
	}
	if rr := a.request("GET", "/api/files/"+f.ID+"/book/flow", nil, true); rr.Code != http.StatusOK {
		t.Fatalf("restart flow=%d", rr.Code)
	}
	if got := a.storeFlowPuts(); len(got) != len(putsAfterFirst) {
		t.Fatalf("restart rebuilt flow: puts %d → %d", len(putsAfterFirst), len(got))
	}
}

// manifest 存在但 chunk 对象被容量回收时，chunk 请求触发一次自愈重建。
func TestReaderFlowSelfHealsMissingChunk(t *testing.T) {
	a := newTestApp(t)
	f := a.readyFile(t, "book.epub", buildEPUB(t))
	if rr := a.request("GET", "/api/files/"+f.ID+"/book/flow", nil, true); rr.Code != http.StatusOK {
		t.Fatalf("flow=%d", rr.Code)
	}
	a.store.mu.Lock()
	delete(a.store.raw, flow.ChunkObjectKey(f.objectKey, 0))
	a.store.mu.Unlock()
	if err := a.srv.cache.Delete(cacheClassReaderFlowChunk, flowChunkCacheKey(f.objectKey, 0)); err != nil {
		t.Fatal(err)
	}
	cr := a.request("GET", "/api/files/"+f.ID+"/book/flow/chunks/0", nil, true)
	if cr.Code != http.StatusOK || !strings.Contains(cr.Body.String(), `data-block="0"`) {
		t.Fatalf("chunk0 after self-heal=%d: %.200s", cr.Code, cr.Body.String())
	}
}

// 书源 blob 走 reader/source L2：内容寻址 immutable，二次解析不再回源。
func TestReaderBookSourceL2(t *testing.T) {
	a := newTestApp(t)
	f := a.readyFile(t, "book.epub", buildEPUB(t))
	rc, err := a.srv.openBookSource(context.Background(), f)
	if err != nil {
		t.Fatal(err)
	}
	data, err := io.ReadAll(rc)
	rc.Close()
	if err != nil || len(data) == 0 {
		t.Fatalf("first read: len=%d err=%v", len(data), err)
	}
	if !a.srv.cache.Has(cacheClassReaderSource, f.objectKey) {
		t.Fatal("book source not cached in L2")
	}
	rc2, err := a.srv.openBookSource(context.Background(), f)
	if err != nil {
		t.Fatal(err)
	}
	data2, err := io.ReadAll(rc2)
	rc2.Close()
	if err != nil || string(data) != string(data2) {
		t.Fatalf("second read mismatch: %d vs %d (err=%v)", len(data), len(data2), err)
	}
}

func TestReaderFlowTXT(t *testing.T) {
	a := newTestApp(t)
	text := "第一章 开始\n内容第一行\n内容第二行\n\n第二章 继续\n更多内容行。\n"
	f := a.readyFile(t, "book.txt", []byte(text))
	m := decode[flow.Manifest](t, a.request("GET", "/api/files/"+f.ID+"/book/flow", nil, true))
	if m.Format != "txt" {
		t.Fatalf("format=%q", m.Format)
	}
	var units int64
	for _, r := range text {
		units += int64(utf16.RuneLen(r))
	}
	if m.TotalChars != units {
		t.Fatalf("total_chars=%d want %d", m.TotalChars, units)
	}
	if len(m.TOC) < 2 {
		t.Fatalf("txt toc=%+v", m.TOC)
	}
	// TXT chunk 文本可读且含标题
	cr := a.request("GET", "/api/files/"+f.ID+"/book/flow/chunks/0", nil, true)
	if cr.Code != http.StatusOK || !strings.Contains(cr.Body.String(), "第一章") {
		t.Fatalf("txt chunk0=%d", cr.Code)
	}
}

func TestReaderFlowProgressAnchor(t *testing.T) {
	a := newTestApp(t)
	f := a.readyFile(t, "book.epub", buildEPUB(t))

	anchor := flow.Anchor{Spine: 0, Block: 3, Path: []int{0, 1}, Offset: 5}
	rr := a.request("PUT", "/api/files/"+f.ID+"/book/progress", map[string]any{
		"anchor": map[string]any{"spine": 0, "block": 3, "path": []int{0, 1}, "offset": 5},
	}, true)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("save=%d: %s", rr.Code, rr.Body.String())
	}
	got := decode[bookProgress](t, a.request("GET", "/api/files/"+f.ID+"/book/progress", nil, true))
	if got.Anchor == nil || got.Anchor.Compare(anchor) != 0 {
		t.Fatalf("anchor round trip: %+v", got)
	}

	// 空进度
	if rr := a.request("PUT", "/api/files/"+f.ID+"/book/progress", map[string]any{}, true); rr.Code != http.StatusNoContent {
		t.Fatalf("empty save=%d", rr.Code)
	}
	got = decode[bookProgress](t, a.request("GET", "/api/files/"+f.ID+"/book/progress", nil, true))
	if got.Anchor != nil {
		t.Fatalf("empty round trip: %+v", got)
	}

	// 非法 anchor 拒绝；旧格式（无 block 字段）可写并迁移
	for name, body := range map[string]map[string]any{
		"bad spine":  {"anchor": map[string]any{"spine": -1, "block": 0, "offset": -1}},
		"bad block":  {"anchor": map[string]any{"spine": 0, "block": -2, "offset": -1}},
		"bad offset": {"anchor": map[string]any{"spine": 0, "block": 1, "offset": -2}},
	} {
		if rr := a.request("PUT", "/api/files/"+f.ID+"/book/progress", body, true); rr.Code != http.StatusBadRequest {
			t.Errorf("%s: status=%d want 400", name, rr.Code)
		}
	}
	rr = a.request("PUT", "/api/files/"+f.ID+"/book/progress", map[string]any{
		"anchor": map[string]any{"spine": 0, "path": []int{7, 2}, "offset": 1},
	}, true)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("legacy save=%d", rr.Code)
	}
	got = decode[bookProgress](t, a.request("GET", "/api/files/"+f.ID+"/book/progress", nil, true))
	if got.Anchor == nil || got.Anchor.Block != 7 || len(got.Anchor.Path) != 1 || got.Anchor.Path[0] != 2 || got.Anchor.Offset != 1 {
		t.Fatalf("legacy migration: %+v", got.Anchor)
	}
}

// 旧架构端点（layout 提交/分页/共享样式/字体）必须整体消失。
func TestReaderLegacyEndpointsRemoved(t *testing.T) {
	a := newTestApp(t)
	f := a.readyFile(t, "book.epub", buildEPUB(t))
	for _, path := range []string{
		"/api/files/" + f.ID + "/book/layouts/capabilities",
		"/api/files/" + f.ID + "/book/layouts",
		"/api/reader.css",
		"/api/reader/fonts/revaro-serif.woff2",
	} {
		if rr := a.request("GET", path, nil, true); rr.Code != http.StatusNotFound {
			t.Errorf("%s status=%d want 404", path, rr.Code)
		}
	}
}

// flow 对象缓存回收：被引用的书保留，孤儿（书已删除）随 GC 清理。
func TestReaderFlowCacheGC(t *testing.T) {
	a := newTestApp(t)
	f := a.readyFile(t, "book.epub", buildEPUB(t))
	if rr := a.request("GET", "/api/files/"+f.ID+"/book/flow", nil, true); rr.Code != http.StatusOK {
		t.Fatalf("flow=%d", rr.Code)
	}
	referenced := map[string]bool{f.objectKey: true}
	if deleted := a.srv.collectFlowCache(context.Background(), referenced); deleted != 0 {
		t.Fatalf("referenced flow deleted: %d", deleted)
	}
	// 生成完成后对象仍在
	if rr := a.request("GET", "/api/files/"+f.ID+"/book/flow/chunks/0", nil, true); rr.Code != http.StatusOK {
		t.Fatalf("chunk0 after gc=%d", rr.Code)
	}

	// 孤儿 flow 对象（书已删除）→ 立即回收
	orphanKey := "flows/blobs/00000000-0000-0000-0000-000000000001/f1/chunks/0.html"
	if _, err := a.srv.objects.Put(context.Background(), orphanKey, "text/html", []byte("<p>x</p>")); err != nil {
		t.Fatal(err)
	}
	if deleted := a.srv.collectFlowCache(context.Background(), map[string]bool{f.objectKey: true}); deleted != 1 {
		t.Fatalf("orphan cleanup deleted=%d", deleted)
	}
	if _, err := a.srv.objects.Get(context.Background(), orphanKey, 1<<20); err == nil {
		t.Fatal("orphan flow object survived GC")
	}
}
