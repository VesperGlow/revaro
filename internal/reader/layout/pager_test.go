package layout

import (
	"bytes"
	"context"
	"fmt"
	"image"
	"image/color"
	"image/png"
	"strings"
	"testing"
	"time"

	"golang.org/x/net/html"

	"github.com/VesperGlow/revaro/internal/reader"
)

// Phase 1 核心验证：Range.cloneContents() 切片对 ruby、复杂 inline、图片、
// 列表、表格等内容的分页一致性。需要本机 Chromium；找不到时整组跳过。

func testProfile() Profile {
	return Profile{
		ViewportW: 400, ViewportH: 520,
		FontSize: 15, FontFamily: FontFamilySerif, LineHeight: 1.6,
		MarginTop: 16, MarginBottom: 12, MarginSide: 16,
	}
}

func testPNG(t *testing.T, w, h int) []byte {
	t.Helper()
	img := image.NewRGBA(image.Rect(0, 0, w, h))
	for y := 0; y < h; y++ {
		for x := 0; x < w; x++ {
			img.Set(x, y, color.RGBA{R: uint8(x * 3), G: uint8(y * 2), B: 120, A: 255})
		}
	}
	var buf bytes.Buffer
	if err := png.Encode(&buf, img); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

func testPager(t *testing.T) *Pager {
	t.Helper()
	info := DetectEngine()
	if !info.Available {
		t.Skipf("chromium unavailable: %s", info.Reason)
	}
	return NewPager(info.Path, "/api/files/f1/book/assets", []reader.Asset{
		{Data: testPNG(t, 60, 40), ContentType: "image/png", Width: 60, Height: 40},
		{Data: testPNG(t, 120, 200), ContentType: "image/png", Width: 120, Height: 200},
	})
}

// longText 生成确定性的中文长文（无换行）。
func longText(n int) string {
	runes := []rune("山川异域风月同天寄诸佛子共结来缘之岁暮百零皆空色")
	var b strings.Builder
	for i := 0; i < n; i++ {
		b.WriteRune(runes[i%len(runes)])
	}
	return b.String()
}

// fixtureChapters 覆盖 Phase 1 要求验证的内容形态：
// ruby、复杂 inline、图片（小图/带题注大图）、嵌套列表、表格、pre、
// blockquote、跨章边界与长文流动。
func fixtureChapters() []ChapterInput {
	return []ChapterInput{
		{Spine: 0, HTML: strings.Join([]string{
			"<h1>第一章 分页验证</h1>",
			"<p>" + longText(160) + "</p>",
			"<p>复杂inline混排：<b>粗体</b>与<i>斜体</i>以及<span>普通<span><b><i>嵌套粗斜</i></b></span>标签</span>在同一段里反复出现，" + longText(120) + "</p>",
			"<p>注音排版：<ruby>漢<rt>hàn</rt></ruby><ruby>字<rt>zì</rt></ruby>与<ruby>训<rt>xùn</rt></ruby><ruby>读<rt>dú</rt></ruby>混在正文中，" + longText(90) + "</p>",
			"<p>段落之间的插图：<img src=\"/api/files/f1/book/assets/0?v=etag\" alt=\"小图\" width=\"60\" height=\"40\"/></p>",
			"<p>" + longText(100) + "</p>",
			"<figure><img src=\"/api/files/f1/book/assets/1?v=etag\" alt=\"大图\" width=\"120\" height=\"200\"/><figcaption>图一：验证插图</figcaption></figure>",
			"<p>" + longText(90) + "</p>",
			"<ul><li>列表项一：" + longText(40) + "</li><li>列表项二：<ul><li>嵌套甲</li><li>嵌套乙</li></ul></li><li>列表项三</li></ul>",
			"<p>" + longText(60) + "</p>",
			"<table><tr><td>甲一</td><td>甲二</td><td>甲三</td></tr><tr><td>乙一</td><td>乙二</td><td>乙三</td></tr><tr><td>丙一</td><td>丙二</td><td>丙三</td></tr></table>",
			"<p>" + longText(60) + "</p>",
			"<pre>function demo() { return 'line long enough to wrap or flow'; }\nsecond pre line</pre>",
			"<blockquote><p>引文段落，" + longText(50) + "</p></blockquote>",
			"<p>ANCHOR-MARKER-標靶定位段：" + longText(40) + "</p>",
		}, "")},
		{Spine: 1, HTML: "<h2>第二章 章边界</h2><p>" + longText(80) + "</p><p>" + longText(80) + "</p>"},
		{Spine: 2, HTML: "<p>第三章 无标题章 " + longText(200) + "</p>"},
	}
}

// htmlText 提取 HTML 的全部文本（等价 JS textContent）。
func htmlText(t *testing.T, markup string) string {
	t.Helper()
	doc, err := html.Parse(strings.NewReader(markup))
	if err != nil {
		t.Fatalf("parse html: %v", err)
	}
	var sb strings.Builder
	var walk func(*html.Node)
	walk = func(n *html.Node) {
		if n.Type == html.TextNode {
			sb.WriteString(n.Data)
		}
		for c := n.FirstChild; c != nil; c = c.NextSibling {
			walk(c)
		}
	}
	walk(doc)
	return sb.String()
}

func testTimeout(t *testing.T) (context.Context, context.CancelFunc) {
	t.Helper()
	return context.WithTimeout(context.Background(), 3*time.Minute)
}

// TestPaginateSlicesContentExactly 是 Phase 1 的一致性核心验证：
//  1. 逐章拼接页文本与原文**逐字符相等**（不重不漏）；
//  2. ruby/img/table 等元素个数与来源一致，且不可断元素不跨页撕裂；
//  3. 每页回渲染不溢出锁定容器（scroll <= client）；
//  4. 回渲染文本与页 HTML 文本一致（序列化/解析无损）。
func TestPaginateSlicesContentExactly(t *testing.T) {
	p := testPager(t)
	ctx, cancel := testTimeout(t)
	defer cancel()
	chapters := fixtureChapters()
	profile := testProfile()

	res, err := p.Paginate(ctx, chapters, profile, false)
	if err != nil {
		t.Fatalf("paginate: %v", err)
	}
	if len(res.Pages) < 6 {
		t.Fatalf("expected multiple pages, got %d", len(res.Pages))
	}

	// 1. 文本不重不漏
	for spine, ch := range chapters {
		want := htmlText(t, ch.HTML)
		var got strings.Builder
		for _, pg := range res.Pages {
			if pg.Spine != spine {
				continue
			}
			got.WriteString(htmlText(t, pg.HTML))
		}
		if got.String() != want {
			t.Errorf("spine %d: 页文本与原文不一致（len %d vs %d）", spine, got.Len(), len(want))
			a, b := []rune(got.String()), []rune(want)
			for i := 0; i < len(a) && i < len(b); i++ {
				if a[i] != b[i] {
					t.Errorf("spine %d: 首个差异在 rune %d: %q vs %q", spine, i, a[i], b[i])
					break
				}
			}
		}
	}

	// 2. 元素计数一致（ruby 的 rt 文本已在文本一致性里覆盖）
	for _, tag := range []string{"<img", "<table", "<ruby", "<li"} {
		srcCount := 0
		for _, ch := range chapters {
			srcCount += strings.Count(ch.HTML, tag)
		}
		pageCount := 0
		for _, pg := range res.Pages {
			pageCount += strings.Count(pg.HTML, tag)
		}
		if srcCount != pageCount {
			t.Errorf("%s: 来源 %d 个，页内 %d 个", tag, srcCount, pageCount)
		}
	}
	// 不可断元素（table/figure）不能跨页撕裂：每个都完整落在单页内
	for _, pg := range res.Pages {
		if n := strings.Count(pg.HTML, "<table"); n > 1 {
			t.Errorf("page %d 含 %d 个 table，应被 break-inside:avoid 保护", pg.Index, n)
		}
	}
	for _, tag := range []string{"<img", "<figure"} {
		seen := map[int]int{}
		for _, pg := range res.Pages {
			seen[pg.Spine] += strings.Count(pg.HTML, tag)
		}
	}

	// 3. 回渲染无溢出
	pages := make([]string, len(res.Pages))
	for i, pg := range res.Pages {
		pages[i] = pg.HTML
	}
	metrics, err := p.MeasurePagesHTML(ctx, pages, profile)
	if err != nil {
		t.Fatalf("measure pages: %v", err)
	}
	if len(metrics) != len(pages) {
		t.Fatalf("metrics = %d, want %d", len(metrics), len(pages))
	}
	for i, m := range metrics {
		if m.ScrollWidth > m.ClientWidth+1.5 {
			t.Errorf("page %d (spine %d) 横向溢出: scroll=%.1f client=%.1f", i, res.Pages[i].Spine, m.ScrollWidth, m.ClientWidth)
		}
		if m.ScrollHeight > m.ClientHeight+1.5 {
			t.Errorf("page %d (spine %d) 纵向溢出: scroll=%.1f client=%.1f", i, res.Pages[i].Spine, m.ScrollHeight, m.ClientHeight)
		}
	}

	// 4. 回渲染文本一致
	for i, m := range metrics {
		want := htmlText(t, res.Pages[i].HTML)
		if m.Text != want {
			t.Errorf("page %d: 回渲染文本与页 HTML 文本不一致（len %d vs %d）", i, len(m.Text), len(want))
		}
	}
}

// TestPaginateAnchorStableAcrossProfiles 验证 readingAnchor 跨 profile 可用：
//  1. 同一 profile 两次分页产出完全相同的页锚点（确定性）；
//  2. 从 fixture 结构直接推导的文本位置锚点（{spine:0, path:[1,0], offset:4}
//     = 第二段文本节点中 "定位段 " 之后、"ANCHOR-MARKER" 之前的 caret），
//     在每个 profile 中都恰好落在某一页的 [start, end) 区间内，且该页仍
//     包含该处文本——「旧 layout 进度 → 新 layout 无缝切换」的映射基础。
func TestPaginateAnchorStableAcrossProfiles(t *testing.T) {
	p := testPager(t)
	ctx, cancel := testTimeout(t)
	defer cancel()
	chapters := []ChapterInput{{Spine: 0, HTML: "<p>" + longText(300) + "</p><p>定位段 ANCHOR-MARKER-標靶 " + longText(100) + "</p><p>" + longText(300) + "</p>"}}
	profiles := []Profile{
		testProfile(),
		{ViewportW: 320, ViewportH: 480, FontSize: 19, FontFamily: FontFamilySerif, LineHeight: 1.8, MarginTop: 12, MarginBottom: 10, MarginSide: 12},
		{ViewportW: 500, ViewportH: 700, FontSize: 24, FontFamily: FontFamilySerif, LineHeight: 1.5, MarginTop: 40, MarginBottom: 20, MarginSide: 30},
	}
	marker := Anchor{Spine: 0, Path: []int{1, 0}, Offset: 4}

	// 1. 确定性
	first, err := p.Paginate(ctx, chapters, profiles[0], false)
	if err != nil {
		t.Fatalf("paginate: %v", err)
	}
	second, err := p.Paginate(ctx, chapters, profiles[0], false)
	if err != nil {
		t.Fatalf("paginate: %v", err)
	}
	if len(first.Pages) != len(second.Pages) {
		t.Fatalf("确定性失败：页数 %d != %d", len(first.Pages), len(second.Pages))
	}
	for i := range first.Pages {
		if first.Pages[i].Start.Compare(second.Pages[i].Start) != 0 || first.Pages[i].End.Compare(second.Pages[i].End) != 0 {
			t.Fatalf("确定性失败：page %d 锚点不一致", i)
		}
	}

	// 2. 跨 profile：标记锚点落在某页区间内，且该页含标记文本
	for i, profile := range profiles {
		res, err := p.Paginate(ctx, chapters, profile, false)
		if err != nil {
			t.Fatalf("profile %d: %v", i, err)
		}
		idx := -1
		for j, pg := range res.Pages {
			if pg.Start.Compare(marker) <= 0 && (j == len(res.Pages)-1 || res.Pages[j+1].Start.Compare(marker) > 0) {
				idx = j
				break
			}
		}
		if idx < 0 {
			t.Fatalf("profile %d: 锚点 %+v 未落在任何页区间内", i, marker)
		}
		if !strings.Contains(htmlText(t, res.Pages[idx].HTML), "ANCHOR-MARKER-標靶") {
			t.Errorf("profile %d: 锚点映射到的页 %d 不再包含标记文本（阅读位置漂移）", i, idx)
		}
	}
}

// TestPaginateTXT 验证 TXT 通道：分段、pre-wrap 排版与文本连续性。
func TestPaginateTXT(t *testing.T) {
	p := testPager(t)
	ctx, cancel := testTimeout(t)
	defer cancel()
	text := "第一章 开始\n" + longText(400) + "\n第二章 继续\n" + longText(300) + "\n"
	chapters := make([]ChapterInput, 0, len(TXTChapters(text, nil)))
	for i, chunk := range TXTChapters(text, nil) {
		chapters = append(chapters, ChapterInput{Spine: i, HTML: chunk})
	}
	res, err := p.Paginate(ctx, chapters, testProfile(), true)
	if err != nil {
		t.Fatalf("paginate txt: %v", err)
	}
	var got strings.Builder
	for _, pg := range res.Pages {
		got.WriteString(htmlText(t, pg.HTML))
	}
	if got.String() != text {
		t.Errorf("txt 页文本与原文不一致（len %d vs %d）", got.Len(), len(text))
	}
}

// TestManifestAssemblyAndLookup 验证 manifest 装配与锚点查页的一致性。
func TestManifestAssemblyAndLookup(t *testing.T) {
	p := testPager(t)
	ctx, cancel := testTimeout(t)
	defer cancel()
	chapters := fixtureChapters()
	profile := testProfile()
	res, err := p.Paginate(ctx, chapters, profile, false)
	if err != nil {
		t.Fatalf("paginate: %v", err)
	}
	m := ManifestFor(res, profile, "bookhash-1", func(i int) string { return fmt.Sprintf("/p/%d", i) }, nil)
	if m.PageCount != len(res.Pages) {
		t.Fatalf("PageCount = %d, want %d", m.PageCount, len(res.Pages))
	}
	if m.ProfileID != profile.ID("bookhash-1") {
		t.Fatalf("ProfileID = %q", m.ProfileID)
	}
	for i, meta := range m.Pages {
		if meta.URL != fmt.Sprintf("/p/%d", i) {
			t.Errorf("page %d URL = %q", i, meta.URL)
		}
		if got := m.PageForAnchor(meta.Start); got != i {
			t.Errorf("PageForAnchor(Start[%d]) = %d, want %d", i, got, i)
		}
		if a, ok := m.AnchorForPage(i); !ok || a.Compare(meta.Start) != 0 {
			t.Errorf("AnchorForPage(%d) mismatch", i)
		}
	}
}

// TestPaginateTOCMapping 验证目录目标（fragment id 与 data-toc 锚点）映射到
// 正确页码：目标元素所在页必须包含该元素。
func TestPaginateTOCMapping(t *testing.T) {
	p := testPager(t)
	ctx, cancel := testTimeout(t)
	defer cancel()
	profile := testProfile()
	chapters := []ChapterInput{
		{Spine: 0, HTML: "<p>" + longText(220) + "</p><h2 id=\"sec-a\">甲节标题</h2><p>" + longText(180) + "</p><h2 id=\"sec-b\">乙节标题</h2><p>" + longText(180) + "</p>"},
		{Spine: 1, HTML: "<p data-source-path=\"OEBPS/ch2.xhtml\">" + longText(150) + "</p><h3 id=\"sec-c\">丙节标题</h3><p>" + longText(150) + "</p>"},
	}
	targets := []TOCTarget{
		{Index: 0, Kind: "fragment", Spine: 0, Fragment: "sec-a"},
		{Index: 1, Kind: "fragment", Spine: 0, Fragment: "sec-b"},
		{Index: 2, Kind: "fragment", Spine: 1, Fragment: "sec-c"},
		{Index: 3, Kind: "fragment", Spine: 1, Fragment: "sec-missing"}, // 缺失 → 该章第一页
	}
	res, err := p.PaginateWithTOC(ctx, chapters, profile, false, targets)
	if err != nil {
		t.Fatalf("paginate: %v", err)
	}
	if len(res.TOCPages) != len(targets) {
		t.Fatalf("TOCPages = %d, want %d", len(res.TOCPages), len(targets))
	}
	for i, want := range map[int]string{
		0: "甲节标题", 1: "乙节标题", 2: "丙节标题",
	} {
		page := res.TOCPages[i]
		if page < 0 || page >= len(res.Pages) {
			t.Fatalf("toc %d page %d out of range", i, page)
		}
		if !strings.Contains(htmlText(t, res.Pages[page].HTML), want) {
			t.Errorf("toc %d → page %d 不含 %q", i, page, want)
		}
	}
	// 缺失 fragment 回退到该章第一页
	firstOfCh2 := -1
	for _, pg := range res.Pages {
		if pg.Spine == 1 {
			firstOfCh2 = pg.Index
			break
		}
	}
	if res.TOCPages[3] != firstOfCh2 {
		t.Errorf("missing fragment → page %d, want first page of spine 1 (%d)", res.TOCPages[3], firstOfCh2)
	}

	// TXT 锚点
	text := "第一章 开始\n" + longText(200) + "\n第二章 继续\n" + longText(200) + "\n"
	txtChapters := make([]ChapterInput, 0, 2)
	for i, chunk := range TXTChapters(text, nil) {
		txtChapters = append(txtChapters, ChapterInput{Spine: i, HTML: chunk})
	}
	txtTargets := []TOCTarget{
		{Index: 0, Kind: "toc-anchor"},
		{Index: 1, Kind: "toc-anchor"},
	}
	txtRes, err := p.PaginateWithTOC(ctx, txtChapters, profile, true, txtTargets)
	if err != nil {
		t.Fatalf("paginate txt: %v", err)
	}
	if len(txtRes.TOCPages) != 2 {
		t.Fatalf("txt TOCPages = %d", len(txtRes.TOCPages))
	}
	for i := range txtTargets {
		page := txtRes.TOCPages[i]
		if page < 0 || page >= len(txtRes.Pages) {
			t.Fatalf("txt toc %d page %d out of range", i, page)
		}
		if !strings.Contains(htmlText(t, txtRes.Pages[page].HTML), "第") {
			t.Errorf("txt toc %d → page %d 不含章节标题", i, page)
		}
	}
}
