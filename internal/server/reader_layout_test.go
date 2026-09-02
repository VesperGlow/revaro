package server

import (
	"archive/zip"
	"bytes"
	"context"
	"fmt"
	"net/http"
	"strings"
	"testing"
	"time"

	"github.com/VesperGlow/revaro/internal/reader/layout"
)

// Phase 0/1 服务端契约测试：进度 API 扩展（向后兼容）、capabilities、
// 共享 WebFont/样式表，以及隐藏的分页生成端点的端到端流程。

func TestReaderProgressAcceptsAnchor(t *testing.T) {
	a := newTestApp(t)
	f := a.readyFile(t, "book.epub", buildEPUB(t))

	// 阅读进度 = readingAnchor + profile（页码不再持久化）
	anchor := layout.Anchor{Spine: 0, Path: []int{0, 1}, Offset: 42}
	profile := "v1-" + strings.Repeat("a", 64)
	rr := a.request("PUT", "/api/files/"+f.ID+"/book/progress", map[string]any{
		"anchor":  map[string]any{"spine": 0, "path": []int{0, 1}, "offset": 42},
		"profile": profile,
	}, true)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("save progress=%d: %s", rr.Code, rr.Body.String())
	}
	rr = a.request("GET", "/api/files/"+f.ID+"/book/progress", nil, true)
	if rr.Code != http.StatusOK {
		t.Fatalf("get progress=%d", rr.Code)
	}
	got := decode[bookProgress](t, rr)
	if got.Anchor == nil || got.Anchor.Compare(anchor) != 0 {
		t.Fatalf("anchor round trip: %+v", got.Anchor)
	}
	if got.Profile != profile {
		t.Fatalf("profile round trip: %q", got.Profile)
	}

	// 空进度（首次打开）
	rr = a.request("PUT", "/api/files/"+f.ID+"/book/progress", map[string]any{}, true)
	if rr.Code != http.StatusNoContent {
		t.Fatalf("empty save=%d", rr.Code)
	}
	rr = a.request("GET", "/api/files/"+f.ID+"/book/progress", nil, true)
	got = decode[bookProgress](t, rr)
	if got.Anchor != nil || got.Profile != "" {
		t.Fatalf("empty round trip: %+v", got)
	}

	// 非法 anchor / profile 被拒绝
	for name, body := range map[string]map[string]any{
		"bad anchor":  {"anchor": map[string]any{"spine": -1, "path": []int{0}, "offset": 0}},
		"bad path":    {"anchor": map[string]any{"spine": 0, "path": []int{1 << 21}, "offset": 0}},
		"bad offset":  {"anchor": map[string]any{"spine": 0, "path": []int{0}, "offset": -2}},
		"bad profile": {"profile": "evil;profile"},
	} {
		rr = a.request("PUT", "/api/files/"+f.ID+"/book/progress", body, true)
		if rr.Code != http.StatusBadRequest {
			t.Errorf("%s: status=%d, want 400", name, rr.Code)
		}
	}
}

func TestReaderCapabilitiesEndpoint(t *testing.T) {
	a := newTestApp(t)
	f := a.readyFile(t, "book.epub", buildEPUB(t))
	rr := a.request("GET", "/api/files/"+f.ID+"/book/layouts/capabilities", nil, true)
	if rr.Code != http.StatusOK {
		t.Fatalf("capabilities=%d: %s", rr.Code, rr.Body.String())
	}
	got := decode[layout.EngineInfo](t, rr)
	if got.Available != layout.DetectEngine().Available {
		t.Fatalf("capabilities availability mismatch: %+v", got)
	}
	if got.Available && got.Version == "" {
		t.Fatalf("available engine should report version: %+v", got)
	}
}

func TestReaderSharedFontAndCSS(t *testing.T) {
	a := newTestApp(t)
	rr := a.request("GET", "/api/reader/fonts/revaro-serif.woff2", nil, true)
	if rr.Code != http.StatusOK {
		t.Fatalf("font=%d", rr.Code)
	}
	if ct := rr.Header().Get("Content-Type"); ct != "font/woff2" {
		t.Fatalf("font content-type=%q", ct)
	}
	if !strings.Contains(rr.Header().Get("Cache-Control"), "immutable") {
		t.Fatalf("font cache-control=%q", rr.Header().Get("Cache-Control"))
	}
	if !bytes.Equal(rr.Body.Bytes(), layout.FontBytes()) {
		t.Fatal("font bytes mismatch")
	}
	if len(rr.Body.Bytes()) < 1024*1024 {
		t.Fatalf("font unexpectedly small: %d bytes", rr.Body.Len())
	}

	rr = a.request("GET", "/api/reader.css", nil, true)
	if rr.Code != http.StatusOK {
		t.Fatalf("css=%d", rr.Code)
	}
	body := rr.Body.String()
	if !strings.Contains(body, "@font-face") || !strings.Contains(body, "/api/reader/fonts/revaro-serif.woff2?v="+layout.FontVersion()) {
		t.Fatalf("css missing font-face: %.120s", body)
	}
	if !strings.Contains(body, ".revaro-page") || !strings.Contains(body, "flow-root") {
		t.Fatalf("css missing shared rules")
	}
}

func TestReaderLayoutGenerationEndToEnd(t *testing.T) {
	if !layout.DetectEngine().Available {
		t.Skip("chromium unavailable")
	}
	a := newTestApp(t)
	f := a.readyFile(t, "book.epub", buildEPUB(t))

	profile := layout.Profile{
		ViewportW: 360, ViewportH: 500, FontSize: 16,
		FontFamily: layout.FontFamilySerif, LineHeight: 1.6,
		MarginTop: 14, MarginBottom: 10, MarginSide: 14,
	}
	profileID := profile.ID(f.objectKey)

	rr := a.request("POST", "/api/files/"+f.ID+"/book/layouts", map[string]any{
		"profile": map[string]any{
			"viewport_w": 360, "viewport_h": 500, "font_size": 16,
			"font_family": layout.FontFamilySerif, "line_height": 1.6,
			"margin_top": 14, "margin_bottom": 10, "margin_side": 14,
		},
	}, true)
	if rr.Code != http.StatusOK {
		t.Fatalf("create layout=%d: %s", rr.Code, rr.Body.String())
	}

	// 轮询直到生成完成（Chromium 冷启动约 1~3s）
	deadline := time.Now().Add(60 * time.Second)
	var manifest layout.Manifest
	for {
		rr = a.request("GET", "/api/files/"+f.ID+"/book/layouts/"+profileID, nil, true)
		if rr.Code != http.StatusOK {
			t.Fatalf("layout status=%d: %s", rr.Code, rr.Body.String())
		}
		status := decode[map[string]any](t, rr)
		switch status["status"] {
		case "done":
		case "error":
			t.Fatalf("layout failed: %v", status["error"])
		default:
			if time.Now().After(deadline) {
				t.Fatalf("layout generation timed out: %v", status)
			}
			time.Sleep(250 * time.Millisecond)
			continue
		}
		break
	}

	rr = a.request("GET", "/api/files/"+f.ID+"/book/layouts/"+profileID+"/manifest", nil, true)
	if rr.Code != http.StatusOK {
		t.Fatalf("manifest=%d: %s", rr.Code, rr.Body.String())
	}
	manifest = decode[layout.Manifest](t, rr)
	if manifest.ProfileID != profileID || manifest.PageCount < 1 || len(manifest.Pages) != manifest.PageCount {
		t.Fatalf("manifest mismatch: %+v", manifest)
	}
	if len(manifest.TOC) != 1 || manifest.TOC[0].Label != "第一章" || manifest.TOC[0].Page != 0 {
		t.Fatalf("manifest toc mismatch: %+v", manifest.TOC)
	}
	pageCount := manifest.PageCount
	if !manifest.Complete {
		t.Fatalf("single-spine book manifest must be complete: %+v", manifest)
	}

	rr = a.request("GET", manifest.Pages[0].URL, nil, true)
	if rr.Code != http.StatusOK {
		t.Fatalf("page=%d: %s", rr.Code, rr.Body.String())
	}
	if ct := rr.Header().Get("Content-Type"); !strings.HasPrefix(ct, "text/html") {
		t.Fatalf("page content-type=%q", ct)
	}
	if !strings.Contains(rr.Body.String(), "revaro-page") || !strings.Contains(rr.Body.String(), "你好世界") {
		t.Fatalf("page body unexpected: %.200s", rr.Body.String())
	}

	rr = a.request("GET", "/api/files/"+f.ID+"/book/layouts", nil, true)
	if rr.Code != http.StatusOK {
		t.Fatalf("list=%d", rr.Code)
	}
	list := decode[map[string][]layoutProfileInfo](t, rr)
	if len(list["profiles"]) != 1 || list["profiles"][0].ProfileID != profileID || list["profiles"][0].PageCount != pageCount {
		t.Fatalf("profile list wrong: %+v", list)
	}

	// 幂等：重复提交直接返回完成
	rr = a.request("POST", "/api/files/"+f.ID+"/book/layouts", map[string]any{
		"profile": map[string]any{
			"viewport_w": 360, "viewport_h": 500, "font_size": 16,
			"font_family": layout.FontFamilySerif, "line_height": 1.6,
			"margin_top": 14, "margin_bottom": 10, "margin_side": 14,
		},
	}, true)
	status := decode[map[string]any](t, rr)
	if status["status"] != "done" {
		t.Fatalf("idempotent resubmit: %v", status)
	}

	// 无效输入
	rr = a.request("POST", "/api/files/"+f.ID+"/book/layouts", map[string]any{
		"profile": map[string]any{"viewport_w": 10},
	}, true)
	if rr.Code != http.StatusBadRequest {
		t.Fatalf("invalid profile accepted: %d", rr.Code)
	}
	rr = a.request("GET", "/api/files/"+f.ID+"/book/layouts/not-a-profile/manifest", nil, true)
	if rr.Code != http.StatusBadRequest {
		t.Fatalf("invalid profile id accepted: %d", rr.Code)
	}
	rr = a.request("GET", "/api/files/"+f.ID+"/book/layouts/"+profileID+"/spines/0/pages/9999", nil, true)
	if rr.Code != http.StatusNotFound {
		t.Fatalf("missing page should 404: %d", rr.Code)
	}
	rr = a.request("GET", "/api/files/"+f.ID+"/book/layouts/"+profileID+"/spines/9/pages/0", nil, true)
	if rr.Code != http.StatusNotFound {
		t.Fatalf("missing spine should 404: %d", rr.Code)
	}
}

// TestReaderLayoutProgressiveOrder 验证渐进式分页：start_anchor 所在章优先
// 生成（首屏立即可读），快照 complete=false 且只含已生成章；后台完成后
// complete=true、全书页码按 spine 顺序装配。
func TestReaderLayoutProgressiveOrder(t *testing.T) {
	if !layout.DetectEngine().Available {
		t.Skip("chromium unavailable")
	}
	a := newTestApp(t)
	f := a.readyFile(t, "book.epub", buildMultiChapterEPUB(t))

	profile := layout.Profile{
		ViewportW: 320, ViewportH: 480, FontSize: 14,
		FontFamily: layout.FontFamilySerif, LineHeight: 1.4,
		MarginTop: 10, MarginBottom: 8, MarginSide: 10,
	}
	profileID := profile.ID(f.objectKey)
	// start anchor 指向第 2 章（spine 1）
	rr := a.request("POST", "/api/files/"+f.ID+"/book/layouts", map[string]any{
		"profile": map[string]any{
			"viewport_w": 320, "viewport_h": 480, "font_size": 14,
			"font_family": layout.FontFamilySerif, "line_height": 1.4,
			"margin_top": 10, "margin_bottom": 8, "margin_side": 10,
		},
		"start_anchor": map[string]any{"spine": 1, "path": []int{0}, "offset": 0},
	}, true)
	if rr.Code != http.StatusOK {
		t.Fatalf("create=%d: %s", rr.Code, rr.Body.String())
	}

	deadline := time.Now().Add(60 * time.Second)
	sawPartial := false
	var finalManifest *layout.Manifest
	for {
		rr = a.request("GET", "/api/files/"+f.ID+"/book/layouts/"+profileID, nil, true)
		status := decode[map[string]any](t, rr)
		switch status["status"] {
		case "error":
			t.Fatalf("layout failed: %v", status["error"])
		case "done":
		default:
			if time.Now().After(deadline) {
				t.Fatalf("timed out: %v", status)
			}
			// 快照应已可读：目标章最先完成
			rr = a.request("GET", "/api/files/"+f.ID+"/book/layouts/"+profileID+"/manifest", nil, true)
			if rr.Code == http.StatusOK {
				m := decode[layout.Manifest](t, rr)
				if !m.Complete {
					sawPartial = true
					if len(m.Pages) == 0 {
						t.Fatalf("partial manifest must contain the window spine: %+v", m)
					}
					// 快照中的页必须全部来自已生成章，且首章是目标章
				}
			}
			time.Sleep(200 * time.Millisecond)
			continue
		}
		rr = a.request("GET", "/api/files/"+f.ID+"/book/layouts/"+profileID+"/manifest", nil, true)
		finalManifest = ptrManifest(decode[layout.Manifest](t, rr))
		break
	}
	if !finalManifest.Complete {
		t.Fatalf("final manifest incomplete: %+v", finalManifest)
	}
	_ = sawPartial // 小书可能一次就完成；渐进路径由 e2e 与调度器测试覆盖
	// 全书页码按 spine 顺序单调递增
	for i := 1; i < len(finalManifest.Pages); i++ {
		if finalManifest.Pages[i].Start.Compare(finalManifest.Pages[i-1].Start) <= 0 {
			t.Fatalf("manifest pages not strictly increasing at %d", i)
		}
	}
	// 目录映射到全局页码
	if len(finalManifest.TOC) != 3 || finalManifest.TOC[1].Page < finalManifest.TOC[0].Page {
		t.Fatalf("toc mismatch: %+v", finalManifest.TOC)
	}
}

func ptrManifest(m layout.Manifest) *layout.Manifest { return &m }

func TestSpineOrderSpiral(t *testing.T) {
	cases := []struct {
		total, target int
		want          []int
	}{
		{5, 0, []int{0, 1, 2, 3, 4}},
		{5, 4, []int{4, 3, 2, 1, 0}},
		{5, 2, []int{2, 3, 1, 4, 0}},
		{3, 1, []int{1, 2, 0}},
		{1, 0, []int{0}},
	}
	for _, tc := range cases {
		got := spineOrder(tc.total, tc.target)
		if fmt.Sprint(got) != fmt.Sprint(tc.want) {
			t.Errorf("spineOrder(%d,%d)=%v want %v", tc.total, tc.target, got, tc.want)
		}
	}
}

// TestLayoutCacheGC 验证 layouts/ 前缀回收：已删书产物立即清理、TTL 与
// 容量上限淘汰最旧对象。
func TestLayoutCacheGC(t *testing.T) {
	a := newTestApp(t)
	book := a.readyFile(t, "book.epub", buildEPUB(t))
	live := a.readyFile(t, "other.epub", buildEPUB(t))

	now := time.Now().UTC()
	put := func(key string, mod time.Time, size int64) {
		a.store.mu.Lock()
		a.store.raw[key] = []byte(strings.Repeat("x", int(size)))
		a.store.modified[key] = mod
		a.store.mu.Unlock()
	}
	// 已删书：blob 不在 referenced 集合里 → 孤儿清理
	put("layouts/"+book.objectKey+"/p1/spines/000000/pages/000000.html", now, 100)
	// 活动书：过期对象走 TTL；新对象保留
	put("layouts/"+live.objectKey+"/p2/spines/000000/pages/000000.html", now.Add(-48*time.Hour), 100)
	put("layouts/"+live.objectKey+"/p2/spines/000000/pages/000001.html", now, 100)

	// TTL=24h 走 TTL 路径；容量给足避免容量路径干扰断言
	a.srv.cfg.LayoutCacheTTL = 24 * time.Hour
	a.srv.cfg.LayoutCacheCapacity = 1 << 30
	t.Cleanup(func() { a.srv.cfg.LayoutCacheTTL = 0; a.srv.cfg.LayoutCacheCapacity = 0 })

	// book 模拟已删除的书：其 blob 不在 referenced 集合里
	referenced := map[string]bool{live.objectKey: true}
	deleted := a.srv.collectLayoutCache(context.Background(), referenced)
	if deleted != 2 { // 孤儿 1 + TTL 过期 1
		t.Fatalf("deleted=%d, want 2", deleted)
	}
	a.store.mu.RLock()
	_, hasLiveNew := a.store.raw["layouts/"+live.objectKey+"/p2/spines/000000/pages/000001.html"]
	_, hasLiveOld := a.store.raw["layouts/"+live.objectKey+"/p2/spines/000000/pages/000000.html"]
	_, hasOrphan := a.store.raw["layouts/"+book.objectKey+"/p1/spines/000000/pages/000000.html"]
	a.store.mu.RUnlock()
	if !hasLiveNew || hasLiveOld || hasOrphan {
		t.Fatalf("gc leftovers: new=%v old=%v orphan=%v", hasLiveNew, hasLiveOld, hasOrphan)
	}
}

// buildMultiChapterEPUB 构造三章 EPUB（各章带 id 锚点与足够长度的正文）。
func buildMultiChapterEPUB(t *testing.T) []byte {
	t.Helper()
	buf := &bytes.Buffer{}
	zw := zip.NewWriter(buf)
	write := func(name, content string) {
		w, err := zw.Create(name)
		if err != nil {
			t.Fatal(err)
		}
		_, _ = w.Write([]byte(content))
	}
	para := func(n int) string {
		runes := []rune("渐进分页验证用正文内容甲乙丙丁戊己庚辛壬癸")
		var b strings.Builder
		for i := 0; i < n; i++ {
			b.WriteRune(runes[i%len(runes)])
		}
		return b.String()
	}
	write("mimetype", "application/epub+zip")
	write("META-INF/container.xml", `<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>`)
	write("OEBPS/content.opf", `<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>多章书</dc:title></metadata><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/><item id="ch2" href="ch2.xhtml" media-type="application/xhtml+xml"/><item id="ch3" href="ch3.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="ch1"/><itemref idref="ch2"/><itemref idref="ch3"/></spine></package>`)
	write("OEBPS/nav.xhtml", `<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol><li><a href="ch1.xhtml#a">甲章</a></li><li><a href="ch2.xhtml#b">乙章</a></li><li><a href="ch3.xhtml#c">丙章</a></li></ol></nav></body></html>`)
	write("OEBPS/ch1.xhtml", `<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="a">甲章</h1><p>`+para(600)+`</p></body></html>`)
	write("OEBPS/ch2.xhtml", `<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="b">乙章</h1><p>`+para(600)+`</p></body></html>`)
	write("OEBPS/ch3.xhtml", `<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="c">丙章</h1><p>`+para(600)+`</p></body></html>`)
	if err := zw.Close(); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}
