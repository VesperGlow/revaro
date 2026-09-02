package layout

import (
	"context"
	_ "embed"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/chromedp/cdproto/emulation"
	"github.com/chromedp/cdproto/fetch"
	"github.com/chromedp/cdproto/network"
	"github.com/chromedp/cdproto/runtime"
	"github.com/chromedp/chromedp"

	"github.com/VesperGlow/revaro/internal/reader"
)

//go:embed reader.css
var sharedCSS string

//go:embed pager.js
var pagerJS string

// SharedCSS 返回共享样式表（服务端分页与客户端页面渲染共用同一份规则）。
func SharedCSS() string { return sharedCSS }

// PagerJS 返回分页脚本（调试/内省用）。
func PagerJS() string { return pagerJS }

// TOCTarget 是目录条目在内容树里的定位目标，由服务端在分页前解析好。
// Kind：
//   - "fragment"（EPUB）：在 Spine 章内找 id 或 data-frag-ids 含 Fragment 的元素；
//   - "toc-anchor"（TXT）：在章内找 data-toc=Index 的锚点 span。
type TOCTarget struct {
	Index    int    `json:"index"` // 目录条目序号（与 Book.TOC 对齐）
	Kind     string `json:"kind"`
	Spine    int    `json:"spine"`
	Fragment string `json:"fragment,omitempty"`
}

// Page 是一个固定页：内容切片 HTML + 覆盖范围锚点。
type Page struct {
	Spine int
	Index int
	Start Anchor
	End   Anchor
	HTML  string
}

// Diagnostic 是分页过程中的非致命诊断（空栏、空章等）。
type Diagnostic struct {
	Type  string
	Spine int
	Col   int
}

// Result 是一次分页的完整产物（不含 manifest 与 URL 填充）。
// TOCPages 与输入的 TOCTarget 一一对应。
type Result struct {
	Pages       []Page
	SectionCols []int
	TOCPages    []int
	Diagnostics []Diagnostic
}

// Pager 用单个 headless Chromium 实例把整本 spine 切成固定页。
type Pager struct {
	// ChromePath 是 Chromium 可执行文件路径；空则自动探测。
	ChromePath string
	// AssetPathPrefix 是内嵌图片 URL 前缀（/api/files/{id}/book/assets），
	// 与 Assets 下标一一对应。
	AssetPathPrefix string
	Assets          []reader.Asset
}

// NewPager 构造分页器。
func NewPager(chromePath, assetPathPrefix string, assets []reader.Asset) *Pager {
	if chromePath == "" {
		chromePath = DetectEngine().Path
	}
	return &Pager{ChromePath: chromePath, AssetPathPrefix: strings.TrimRight(assetPathPrefix, "/"), Assets: assets}
}

// Available 报告分页器当前是否可用。
func (p *Pager) Available() bool { return p.ChromePath != "" }

// paginateTimeout 是单次分页的硬上限；大书由调用方用更短的 ctx 控制。
const paginateTimeout = 15 * time.Minute

type jsAnchor struct {
	Spine  int   `json:"spine"`
	Path   []int `json:"path"`
	Offset int   `json:"offset"`
}

type jsPage struct {
	Spine int      `json:"spine"`
	Index int      `json:"index"`
	Start jsAnchor `json:"start"`
	End   jsAnchor `json:"end"`
	HTML  string   `json:"html"`
}

type jsSection struct {
	Spine int `json:"spine"`
	Cols  int `json:"cols"`
	Pages int `json:"pages"`
}

type jsDiagnostic struct {
	Type  string `json:"type"`
	Spine int    `json:"spine,omitempty"`
	Col   int    `json:"col,omitempty"`
}

type jsTOC struct {
	Index int `json:"index"`
	Page  int `json:"page"`
}

type jsResult struct {
	OK          bool           `json:"ok"`
	Error       string         `json:"error"`
	Pages       []jsPage       `json:"pages"`
	Sections    []jsSection    `json:"sections"`
	TOC         []jsTOC        `json:"toc"`
	Diagnostics []jsDiagnostic `json:"diagnostics"`
}

// maxPageCount 与 maxResultBytes 是防御性上限：分页产物受内存与传输约束。
const maxPageCount = 20000
const maxResultBytes = 256 << 20

// Paginate 执行整本分页（不含目录映射）。chapters 必须与 Book.Chapters
// 顺序一致（Spine 下标）。
func (p *Pager) Paginate(ctx context.Context, chapters []ChapterInput, profile Profile, txt bool) (*Result, error) {
	return p.PaginateWithTOC(ctx, chapters, profile, txt, nil)
}

// PaginateWithTOC 执行整本分页，并把 targets 里的目录目标映射成页码
// （Result.TOCPages 与 targets 一一对应）。
func (p *Pager) PaginateWithTOC(ctx context.Context, chapters []ChapterInput, profile Profile, txt bool, targets []TOCTarget) (*Result, error) {
	if !p.Available() {
		return nil, fmt.Errorf("layout engine unavailable: %s", DetectEngine().Reason)
	}
	if err := profile.Validate(); err != nil {
		return nil, fmt.Errorf("invalid layout profile: %w", err)
	}
	if len(chapters) == 0 {
		return &Result{}, nil
	}
	ctx, cancel := context.WithTimeout(ctx, paginateTimeout)
	defer cancel()

	wrapper := BuildWrapper(chapters, profile, txt, sharedCSS, pagerJS, targets)
	session, sessionCancel, err := p.openSession(ctx)
	if err != nil {
		return nil, err
	}
	defer sessionCancel()
	p.listenRequests(session, p.defaultResolver(wrapper))

	if err := chromedp.Run(session,
		fetch.Enable().WithPatterns([]*fetch.RequestPattern{{URLPattern: pagerOrigin + "/*"}}),
		emulation.SetDeviceMetricsOverride(int64(profile.ViewportW), int64(profile.ViewportH), 1, false),
		chromedp.Navigate(PagerMainURL()),
		chromedp.WaitReady("#revaro-book", chromedp.ByID),
	); err != nil {
		return nil, fmt.Errorf("load pagination page: %w", err)
	}

	var raw json.RawMessage
	if err := chromedp.Run(session, chromedp.Evaluate("window.__revaroPaginate()", &raw, evalAwait)); err != nil {
		return nil, fmt.Errorf("run pagination: %w", err)
	}
	if len(raw) > maxResultBytes {
		return nil, fmt.Errorf("pagination result exceeds %d MiB limit", maxResultBytes>>20)
	}
	var out jsResult
	if err := json.Unmarshal(raw, &out); err != nil {
		return nil, fmt.Errorf("decode pagination result: %w", err)
	}
	if !out.OK {
		return nil, fmt.Errorf("pagination failed: %s", out.Error)
	}
	if len(out.Pages) > maxPageCount {
		return nil, fmt.Errorf("page count %d exceeds limit %d", len(out.Pages), maxPageCount)
	}

	res := &Result{SectionCols: make([]int, len(chapters)), TOCPages: make([]int, len(out.TOC))}
	for _, s := range out.Sections {
		if s.Spine >= 0 && s.Spine < len(res.SectionCols) {
			res.SectionCols[s.Spine] = s.Cols
		}
	}
	for _, t := range out.TOC {
		if t.Index >= 0 && t.Index < len(res.TOCPages) {
			res.TOCPages[t.Index] = t.Page
		}
	}
	for _, pg := range out.Pages {
		res.Pages = append(res.Pages, Page{
			Spine: pg.Spine,
			Index: pg.Index,
			Start: Anchor{Spine: pg.Spine, Path: pg.Start.Path, Offset: pg.Start.Offset},
			End:   Anchor{Spine: pg.Spine, Path: pg.End.Path, Offset: pg.End.Offset},
			HTML:  pg.HTML,
		})
	}
	for _, d := range out.Diagnostics {
		res.Diagnostics = append(res.Diagnostics, Diagnostic{Type: d.Type, Spine: d.Spine, Col: d.Col})
	}
	if err := validateResult(res); err != nil {
		return nil, err
	}
	return res, nil
}

// SpineResult 是单章分页结果。Pages 的 Index 是章内栏序号（0..Cols-1），
// 全局页码由服务端按「已生成章的前缀和」装配；同一章在任何分页模式下
// （整本/单章）产出完全相同的锚点。
type SpineResult struct {
	Spine       int
	Cols        int
	Pages       []Page
	TOCPages    []int // 与 targets 对齐（章内栏号）
	Diagnostics []Diagnostic
}

// PaginateSpine 在调度器的共享浏览器里分页单章（渐进式分页的原子单位）。
func (p *Pager) PaginateSpine(ctx context.Context, sched *Scheduler, chapter ChapterInput, profile Profile, txt bool, targets []TOCTarget) (*SpineResult, error) {
	if sched == nil || !sched.Available() {
		return nil, fmt.Errorf("layout engine unavailable: %s", DetectEngine().Reason)
	}
	if err := profile.Validate(); err != nil {
		return nil, fmt.Errorf("invalid layout profile: %w", err)
	}
	wrapper := BuildWrapper([]ChapterInput{chapter}, profile, txt, sharedCSS, pagerJS, targets)

	var out jsResult
	err := sched.Run(ctx, p.defaultResolver(wrapper), func(session context.Context) error {
		if err := chromedp.Run(session,
			fetch.Enable().WithPatterns([]*fetch.RequestPattern{{URLPattern: pagerOrigin + "/*"}}),
			emulation.SetDeviceMetricsOverride(int64(profile.ViewportW), int64(profile.ViewportH), 1, false),
			chromedp.Navigate(PagerMainURL()),
			chromedp.WaitReady("#revaro-book", chromedp.ByID),
		); err != nil {
			return fmt.Errorf("load pagination page: %w", err)
		}
		var raw json.RawMessage
		if err := chromedp.Run(session, chromedp.Evaluate("window.__revaroPaginate()", &raw, evalAwait)); err != nil {
			return fmt.Errorf("run pagination: %w", err)
		}
		if len(raw) > maxResultBytes {
			return fmt.Errorf("pagination result exceeds %d MiB limit", maxResultBytes>>20)
		}
		if err := json.Unmarshal(raw, &out); err != nil {
			return fmt.Errorf("decode pagination result: %w", err)
		}
		if !out.OK {
			return fmt.Errorf("pagination failed: %s", out.Error)
		}
		if len(out.Pages) > maxPageCount {
			return fmt.Errorf("page count %d exceeds limit %d", len(out.Pages), maxPageCount)
		}
		return nil
	})
	if err != nil {
		return nil, err
	}

	res := &Result{SectionCols: make([]int, 1), TOCPages: make([]int, len(out.TOC))}
	for _, s := range out.Sections {
		if s.Spine == 0 {
			res.SectionCols[0] = s.Cols
		}
	}
	for _, t := range out.TOC {
		if t.Index >= 0 && t.Index < len(res.TOCPages) {
			res.TOCPages[t.Index] = t.Page
		}
	}
	for _, pg := range out.Pages {
		res.Pages = append(res.Pages, Page{
			Spine: chapter.Spine,
			Index: pg.Index,
			Start: Anchor{Spine: chapter.Spine, Path: pg.Start.Path, Offset: pg.Start.Offset},
			End:   Anchor{Spine: chapter.Spine, Path: pg.End.Path, Offset: pg.End.Offset},
			HTML:  pg.HTML,
		})
	}
	for _, d := range out.Diagnostics {
		res.Diagnostics = append(res.Diagnostics, Diagnostic{Type: d.Type, Spine: d.Spine, Col: d.Col})
	}
	if err := validateResult(res); err != nil {
		return nil, err
	}
	outRes := &SpineResult{Spine: chapter.Spine, Cols: res.SectionCols[0], Pages: res.Pages, TOCPages: res.TOCPages, Diagnostics: res.Diagnostics}
	sched.AddPages(int64(len(res.Pages)))
	return outRes, nil
}

// evalAwait 以 awaitPromise + returnByValue 方式求值表达式。
var evalAwait = func(prm *runtime.EvaluateParams) *runtime.EvaluateParams {
	return prm.WithAwaitPromise(true).WithReturnByValue(true)
}

// openSession 启动一个独立 headless Chromium 会话（独立 profile 目录，
// 关闭时清理）。每次分页/度量一个会话，串行调度由上层队列保证。
func (p *Pager) openSession(ctx context.Context) (context.Context, context.CancelFunc, error) {
	profileDir, err := os.MkdirTemp("", "revaro-layout-*")
	if err != nil {
		return nil, nil, fmt.Errorf("create chromium profile dir: %w", err)
	}
	opts := append(chromedp.DefaultExecAllocatorOptions[:],
		chromedp.ExecPath(p.ChromePath),
		chromedp.UserDataDir(profileDir),
		chromedp.NoSandbox, // 只渲染白名单清洗内容，且容器内非 root 无用户命名空间
		chromedp.Flag("hide-scrollbars", true),
		chromedp.Flag("font-render-hinting", "none"),
	)
	allocCtx, allocCancel := chromedp.NewExecAllocator(ctx, opts...)
	taskCtx, taskCancel := chromedp.NewContext(allocCtx, chromedp.WithErrorf(func(string, ...any) {}))
	cancel := func() {
		taskCancel()
		allocCancel()
		_ = os.RemoveAll(profileDir)
	}
	return taskCtx, cancel, nil
}

// PageMetrics 是一页固定 HTML 在锁定容器内的渲染度量。
type PageMetrics struct {
	ScrollWidth  float64 `json:"scrollWidth"`
	ScrollHeight float64 `json:"scrollHeight"`
	ClientWidth  float64 `json:"clientWidth"`
	ClientHeight float64 `json:"clientHeight"`
	Text         string  `json:"text"`
}

// MeasurePageHTML 把一页固定 HTML 放回同 profile 的视口里渲染并度量。
// 断言页面内容不超出锁定容器（scroll <= client）是切片正确性的核心验证。
func (p *Pager) MeasurePageHTML(ctx context.Context, pageHTML string, profile Profile) (PageMetrics, error) {
	wrapper := pageShell(pageHTML)
	session, cancel, err := p.openSession(ctx)
	if err != nil {
		return PageMetrics{}, err
	}
	defer cancel()
	p.listenRequests(session, p.defaultResolver(wrapper))
	if err := chromedp.Run(session,
		fetch.Enable().WithPatterns([]*fetch.RequestPattern{{URLPattern: pagerOrigin + "/*"}}),
		emulation.SetDeviceMetricsOverride(int64(profile.ViewportW), int64(profile.ViewportH), 1, false),
		chromedp.Navigate(PagerMainURL()),
	); err != nil {
		return PageMetrics{}, err
	}
	var out PageMetrics
	expr := `(async () => {
		await document.fonts.ready;
		await Promise.all(Array.from(document.images).map(img => img.decode().catch(() => {})));
		const el = document.querySelector('.revaro-page');
		if (!el) return null;
		return {
			scrollWidth: el.scrollWidth,
			scrollHeight: el.scrollHeight,
			clientWidth: el.clientWidth,
			clientHeight: el.clientHeight,
			text: el.textContent,
		};
	})()`
	if err := chromedp.Run(session, chromedp.Evaluate(expr, &out, evalAwait)); err != nil {
		return PageMetrics{}, err
	}
	return out, nil
}

// MeasurePagesHTML 在同一会话内依次回渲染多页并度量（每页一个独立
// wrapper URL /page/{i}），避免逐页开浏览器的开销。
func (p *Pager) MeasurePagesHTML(ctx context.Context, pages []string, profile Profile) ([]PageMetrics, error) {
	if len(pages) == 0 {
		return nil, nil
	}
	session, cancel, err := p.openSession(ctx)
	if err != nil {
		return nil, err
	}
	defer cancel()
	resolve := func(url string) ([]byte, string, bool) {
		if url == PagerFontURL() {
			return serifFont, "font/woff2", true
		}
		if strings.HasPrefix(url, pagerOrigin+"/page/") {
			idx, convErr := strconv.Atoi(strings.TrimPrefix(url, pagerOrigin+"/page/"))
			if convErr == nil && idx >= 0 && idx < len(pages) {
				return []byte(pageShell(pages[idx])), "text/html; charset=utf-8", true
			}
		}
		// 页面内嵌图片仍走默认资产解析
		return p.defaultResolver("")(url)
	}
	p.listenRequests(session, resolve)
	if err := chromedp.Run(session,
		fetch.Enable().WithPatterns([]*fetch.RequestPattern{{URLPattern: pagerOrigin + "/*"}}),
		emulation.SetDeviceMetricsOverride(int64(profile.ViewportW), int64(profile.ViewportH), 1, false),
	); err != nil {
		return nil, err
	}
	out := make([]PageMetrics, len(pages))
	for i := range pages {
		if err := chromedp.Run(session, chromedp.Navigate(pagerOrigin+"/page/"+strconv.Itoa(i))); err != nil {
			return nil, err
		}
		expr := `(async () => {
			await document.fonts.ready;
			await Promise.all(Array.from(document.images).map(img => img.decode().catch(() => {})));
			const el = document.querySelector('.revaro-page');
			if (!el) return null;
			return {
				scrollWidth: el.scrollWidth,
				scrollHeight: el.scrollHeight,
				clientWidth: el.clientWidth,
				clientHeight: el.clientHeight,
				text: el.textContent,
			};
		})()`
		if err := chromedp.Run(session, chromedp.Evaluate(expr, &out[i], evalAwait)); err != nil {
			return nil, err
		}
	}
	return out, nil
}

// pageShell 把一页固定 HTML 包进度量用的最小文档。
func pageShell(pageHTML string) string {
	return "<!doctype html><html><head><meta charset=\"utf-8\"><base href=\"" + pagerOrigin + "/\">" +
		"<style>" + FontFaceCSS(PagerFontURL()) + "\n" + sharedCSS + "</style></head>" +
		"<body style=\"margin:0\">" + pageHTML + "</body></html>"
}

// validateResult 断言分页产物的硬性不变量：页序、锚点单调、相邻页无缝衔接。
func validateResult(res *Result) error {
	for i, pg := range res.Pages {
		if pg.Index != i {
			return fmt.Errorf("page %d has out-of-order index %d", i, pg.Index)
		}
		if !pg.Start.Valid() || !pg.End.Valid() {
			return fmt.Errorf("page %d has invalid anchor", i)
		}
		if pg.Start.Compare(pg.End) > 0 {
			return fmt.Errorf("page %d has reversed anchors", i)
		}
		if i > 0 {
			prev := res.Pages[i-1]
			if prev.Start.Compare(pg.Start) >= 0 {
				return fmt.Errorf("page anchors not strictly increasing at %d", i)
			}
			if prev.Spine == pg.Spine && prev.End.Compare(pg.Start) != 0 {
				return fmt.Errorf("page boundary mismatch between %d and %d", i-1, i)
			}
			if prev.Spine != pg.Spine {
				// 章边界：上一页止于章末（after-last-child caret），下一页
				// 起于新章首（before-first-child caret）。
				if len(pg.Start.Path) != 1 || pg.Start.Path[0] != 0 || pg.Start.Offset != -1 {
					return fmt.Errorf("page %d does not start at chapter boundary", i)
				}
				if len(prev.End.Path) != 1 || prev.End.Offset != -1 {
					return fmt.Errorf("page %d does not end at chapter boundary", i-1)
				}
			}
		}
	}
	return nil
}

// listenRequests 安装 Fetch 拦截：resolve 为每个请求 URL 提供响应体，
// 未命中的请求直接 Fail。Chromium 全程不发起真实网络请求。
func (p *Pager) listenRequests(ctx context.Context, resolve func(url string) ([]byte, string, bool)) {
	chromedp.ListenTarget(ctx, func(ev any) {
		e, ok := ev.(*fetch.EventRequestPaused)
		if !ok {
			return
		}
		go p.fulfill(ctx, e.RequestID, e.Request.URL, resolve)
	})
}

// defaultResolver 返回 (wrapper 文档 + 字体 + 内嵌图片) 的标准解析器。
func (p *Pager) defaultResolver(wrapper string) func(url string) ([]byte, string, bool) {
	return func(url string) ([]byte, string, bool) {
		switch {
		case url == PagerMainURL():
			return []byte(wrapper), "text/html; charset=utf-8", true
		case url == PagerFontURL():
			return serifFont, "font/woff2", true
		case strings.HasPrefix(url, pagerOrigin) && strings.HasPrefix(strings.TrimPrefix(url, pagerOrigin), p.AssetPathPrefix+"/"):
			rest := strings.TrimPrefix(url, pagerOrigin+p.AssetPathPrefix+"/")
			if q := strings.IndexByte(rest, '?'); q >= 0 {
				rest = rest[:q]
			}
			idx, err := strconv.Atoi(rest)
			if err != nil || idx < 0 || idx >= len(p.Assets) {
				return nil, "", false
			}
			asset := p.Assets[idx]
			return asset.Data, asset.ContentType, true
		}
		return nil, "", false
	}
}

func (p *Pager) fulfill(ctx context.Context, id fetch.RequestID, url string, resolve func(string) ([]byte, string, bool)) {
	body, mime, ok := resolve(url)
	if !ok {
		_ = chromedp.Run(ctx, fetch.FailRequest(id, network.ErrorReasonBlockedByClient))
		return
	}
	headers := []*fetch.HeaderEntry{{Name: "Content-Type", Value: mime}}
	_ = chromedp.Run(ctx, fetch.FulfillRequest(id, 200).
		WithResponseHeaders(headers).
		WithBody(base64.StdEncoding.EncodeToString(body)))
}

// ManifestFor 把分页结果装配成 Manifest；pageURL 返回第 n 页的相对 URL，
// toc 是已映射页码的目录条目。
func ManifestFor(res *Result, profile Profile, bookHash string, pageURL func(index int) string, toc []TOCMeta) *Manifest {
	m := &Manifest{
		Version:     1,
		BookHash:    bookHash,
		ProfileID:   profile.ID(bookHash),
		Profile:     profile.normalized(),
		PageCount:   len(res.Pages),
		GeneratedAt: time.Now().UTC().Format(time.RFC3339),
		Pages:       make([]PageMeta, 0, len(res.Pages)),
		TOC:         toc,
	}
	for _, pg := range res.Pages {
		meta := PageMeta{Index: pg.Index, Spine: pg.Spine, Start: pg.Start, End: pg.End, Bytes: int64(len(pg.HTML))}
		if pageURL != nil {
			meta.URL = pageURL(pg.Index)
		}
		m.Pages = append(m.Pages, meta)
	}
	return m
}

// SpinePageObjectKey 是对象存储里「某章第 col 栏」页面产物的键。渐进式
// 分页按章生成：URL 与索引一经写入即永久稳定，全局页码只在 manifest
// 快照里按前缀和装配，随生成推进变化的是快照而非页面对象。
func SpinePageObjectKey(bookObjectKey, profileID string, spine, col int) string {
	return filepath.ToSlash(filepath.Join("layouts", bookObjectKey, profileID, "spines", fmt.Sprintf("%06d", spine), "pages", fmt.Sprintf("%06d.html", col)))
}

// LayoutPrefix 是该书所有 layout 产物的对象键前缀（随书删除用）。
func LayoutPrefix(bookObjectKey string) string {
	return filepath.ToSlash(filepath.Join("layouts", bookObjectKey)) + "/"
}

// ManifestObjectKey 是 manifest 的对象键。
func ManifestObjectKey(bookObjectKey, profileID string) string {
	return filepath.ToSlash(filepath.Join("layouts", bookObjectKey, profileID, "manifest.json"))
}
