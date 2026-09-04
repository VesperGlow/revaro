package flow

import (
	"encoding/json"
	"fmt"
	"strings"
	"testing"

	"github.com/VesperGlow/revaro/internal/reader"
	"golang.org/x/net/html"
	"golang.org/x/net/html/atom"
)

// sampleEPUBBook 构造解析后的 EPUB Book：三章、含目录片段与内嵌 id，
// 文本量足以产生多个 chunk。
func sampleEPUBBook(t *testing.T) *reader.Book {
	t.Helper()
	repeat := func(text string, n int) string {
		var b strings.Builder
		for i := 0; i < n; i++ {
			b.WriteString("<p data-source-path=\"OEBPS/ch.xhtml\">")
			b.WriteString(fmt.Sprintf("%s %d。", text, i))
			b.WriteString("</p>")
		}
		return b.String()
	}
	long := "很长很长的段落内容，用于把章节文本量撑到可以切出多个chunk，每个段落都足够普通。"
	ch1 := `<h1 id="sec1" data-source-path="OEBPS/ch1.xhtml">第一章 开始</h1>` +
		repeat(long, 500)
	ch2 := `<h1 id="sec2" data-source-path="OEBPS/ch2.xhtml">第二章</h1>` +
		`<p data-source-path="OEBPS/ch2.xhtml">开头<b>加粗<span id="mid">中段</span></b>结尾</p>` +
		repeat(long+"第二章专用", 500)
	ch3 := `<h1 id="sec3" data-source-path="OEBPS/ch3.xhtml">第三章</h1>` +
		`<p data-source-path="OEBPS/ch3.xhtml"><img src="/api/assets/1" width="10" height="20" alt="图"></p>` +
		repeat(long+"第三章", 200)
	return &reader.Book{
		Format: "epub",
		Title:  "测试书",
		Chapters: []reader.Chapter{
			{HTML: ch1}, {HTML: ch2}, {HTML: ch3},
		},
		TOC: []reader.TocEntry{
			{Label: "第一章", Path: "OEBPS/ch1.xhtml", Fragment: "sec1", Depth: 0},
			{Label: "第二章 中段", Path: "OEBPS/ch2.xhtml", Fragment: "mid", Depth: 0},
			{Label: "第三章", Path: "OEBPS/ch3.xhtml", Fragment: "sec3", Depth: 0},
		},
	}
}

func TestBuildEPUBDeterministicAndInvariants(t *testing.T) {
	book := sampleEPUBBook(t)
	b1, err := Build(book)
	if err != nil {
		t.Fatal(err)
	}
	b2, err := Build(book)
	if err != nil {
		t.Fatal(err)
	}
	m1, _ := json.Marshal(b1.Manifest)
	m2, _ := json.Marshal(b2.Manifest)
	if string(m1) != string(m2) {
		t.Fatal("manifest not deterministic")
	}
	if len(b1.Chunks) != len(b2.Chunks) {
		t.Fatalf("chunk count changed: %d vs %d", len(b1.Chunks), len(b2.Chunks))
	}
	for i := range b1.Chunks {
		if b1.Chunks[i].HTML != b2.Chunks[i].HTML {
			t.Fatalf("chunk %d not deterministic", i)
		}
	}

	m := b1.Manifest
	if m.Format != "epub" || m.Version != FlowFormatVersion {
		t.Fatalf("manifest meta: %+v", m)
	}
	total := 0
	for _, sp := range m.Spines {
		total += sp.BlockCount
	}
	if got := m.TotalBlocks(); got != total || total == 0 {
		t.Fatalf("total blocks=%d (sum=%d)", got, total)
	}
	// chunk 区间连续不重叠
	if len(m.Chunks) < 2 {
		t.Fatalf("期望多个 chunk，实际 %d", len(m.Chunks))
	}
	for i, c := range m.Chunks {
		if c.Index != i || c.BlockCount <= 0 {
			t.Fatalf("chunk %d meta broken: %+v", i, c)
		}
		if i > 0 {
			prev := m.Chunks[i-1]
			if c.BlockStart != prev.BlockStart+prev.BlockCount {
				t.Fatalf("chunk %d range not contiguous: %+v (prev %+v)", i, c, prev)
			}
		}
		if c.BlockStart+c.BlockCount > total {
			t.Fatalf("chunk %d overflows blocks: %+v", i, c)
		}
		if c.Chars <= 0 {
			t.Fatalf("chunk %d empty chars", i)
		}
	}
	last := m.Chunks[len(m.Chunks)-1]
	if last.BlockStart+last.BlockCount != total {
		t.Fatalf("chunks do not cover all blocks: last=%+v total=%d", last, total)
	}
	// 全局 data-block 编号连续唯一，文本无丢失
	seenBlocks := map[int]bool{}
	var text strings.Builder
	for _, ch := range b1.Chunks {
		nodes := parseChunk(ch.HTML, t)
		for _, n := range nodes {
			if n.Type != html.ElementNode {
				t.Fatalf("chunk top-level is not element")
			}
			blk := -1
			for _, a := range n.Attr {
				if a.Key == "data-block" {
					fmt.Sscanf(a.Val, "%d", &blk)
				}
			}
			if blk < 0 || seenBlocks[blk] {
				t.Fatalf("data-block duplicate/invalid: %d", blk)
			}
			seenBlocks[blk] = true
			text.WriteString(nodeText(n))
		}
	}
	if len(seenBlocks) != total {
		t.Fatalf("block coverage: %d/%d", len(seenBlocks), total)
	}
	// 文本完整性：章节正文逐字保留
	var want strings.Builder
	for _, ch := range book.Chapters {
		want.WriteString(textOfFragment(ch.HTML, t))
	}
	if want.String() != text.String() {
		t.Fatalf("text mismatch:\nwant=%q\ngot =%q", want.String(), text.String())
	}
	// totalChars 与文本 utf16 一致（ASCII 每字符一码元）
	if m.TotalChars != int64(utf16UnitCount(want.String())) {
		t.Fatalf("total_chars=%d want=%d", m.TotalChars, utf16UnitCount(want.String()))
	}
	// 目录目标正确：mid 在第二章第 0 块（h1）之后的块 0/1…，
	// 其块号应落在第二章块区间内且可被 ChunkForBlock 找到
	spine1Start := m.Spines[1].BlockStart
	if len(m.TOC) != 3 {
		t.Fatalf("toc=%+v", m.TOC)
	}
	if m.TOC[1].Spine != 1 || m.TOC[1].Block < spine1Start || m.TOC[1].Block >= spine1Start+m.Spines[1].BlockCount {
		t.Fatalf("toc[1] mapping wrong: %+v", m.TOC[1])
	}
	for _, e := range m.TOC {
		if m.ChunkForBlock(e.Block) < 0 {
			t.Fatalf("toc target not in any chunk: %+v", e)
		}
	}
}

// TestBuildEPUBTOCFragmentsInSameBlock 验证同一顶层块内的多个目录
// fragment：manifest 的 Block 相同，SourceFragment 各自保留，且服务端为
// 每个目标绑定 Stable NavAnchor（data-rv-anchor 标记落在目标文本前）。
func TestBuildEPUBTOCFragmentsInSameBlock(t *testing.T) {
	ch1 := `<h1 id="sec1" data-source-path="OEBPS/ch1.xhtml">第一章</h1>` +
		`<p data-source-path="OEBPS/ch1.xhtml">第一章正文。</p>`
	filler := strings.Repeat("同一块内的填充文本，用于隔开两个目录目标。", 30)
	ch2 := `<div data-source-path="OEBPS/ch2.xhtml">` +
		`<span id="frag-a">甲处</span>` + filler +
		`<span id="frag-b">乙处</span>` + filler + `</div>`
	book := &reader.Book{
		Format:   "epub",
		Title:    "片段书",
		Chapters: []reader.Chapter{{HTML: ch1}, {HTML: ch2}},
		TOC: []reader.TocEntry{
			{Label: "第一章", Path: "OEBPS/ch1.xhtml", Fragment: "sec1", Depth: 0},
			{Label: "甲处", Path: "OEBPS/ch2.xhtml", Fragment: "frag-a", Depth: 1},
			{Label: "乙处", Path: "OEBPS/ch2.xhtml", Fragment: "frag-b", Depth: 1},
		},
	}
	built, err := Build(book)
	if err != nil {
		t.Fatal(err)
	}
	m := built.Manifest
	if len(m.TOC) != 3 {
		t.Fatalf("toc=%+v", m.TOC)
	}
	a, b := m.TOC[1], m.TOC[2]
	if a.SourceFragment != "frag-a" || b.SourceFragment != "frag-b" {
		t.Fatalf("source fragments not preserved: %+v / %+v", a, b)
	}
	if a.Block != b.Block {
		t.Fatalf("same-block fragments should share one block: %d vs %d", a.Block, b.Block)
	}
	if a.Spine != 1 || m.TOC[0].SourceFragment != "sec1" {
		t.Fatalf("toc mapping broken: %+v", m.TOC)
	}
	// NavAnchor id 按目录序分配（文本目标不再进入 DOM，仅媒体目标注入
	// data-rv-anchor），原始 source path 保留
	if a.NavAnchor != "" || b.NavAnchor != "" || m.TOC[0].NavAnchor != "" {
		t.Fatalf("text targets must not bind DOM markers: %+v / %+v / %+v", a, b, m.TOC[0])
	}
	if a.SourcePath != "OEBPS/ch2.xhtml" || m.TOC[0].SourcePath != "OEBPS/ch1.xhtml" {
		t.Fatalf("source path not preserved: %+v", m.TOC[0])
	}
	// 文本目标携带实际文本节点的 DOM path + UTF-16 偏移
	if len(a.TextPath) == 0 || a.TextOffset != 0 || len(b.TextPath) == 0 {
		t.Fatalf("text locator missing: %+v / %+v", a, b)
	}
	// Block 仍可定位 chunk，且 manifest.Chunk 与块所在 chunk 一致
	ci := m.ChunkForBlock(a.Block)
	if ci < 0 {
		t.Fatalf("block %d not in any chunk", a.Block)
	}
	if got := resolveChunkText(t, built.Chunks[ci].HTML, a.Block, a.TextPath); got != "甲处" {
		t.Fatalf("text path resolves to %q, want 甲处", got)
	}
	if got := resolveChunkText(t, built.Chunks[ci].HTML, b.Block, b.TextPath); got != "乙处" {
		t.Fatalf("text path resolves to %q, want 乙处", got)
	}
	if a.Chunk != ci || b.Chunk != ci || m.TOC[0].Chunk != m.ChunkForBlock(m.TOC[0].Block) {
		t.Fatalf("toc chunk field wrong: %+v", m.TOC)
	}
	html := built.Chunks[ci].HTML
	if !strings.Contains(html, `id="frag-a"`) || !strings.Contains(html, `id="frag-b"`) {
		t.Fatalf("chunk html lost fragment ids: %s", html)
	}
	// 文本目标不再注入空 inline 标记
	if strings.Contains(html, "toc-anchor") || strings.Contains(html, "data-rv-anchor") {
		t.Fatalf("text target must not inject inline markers: %s", html)
	}
	// data-block 编号仍落在该块上（阅读进度体系不变）
	if !strings.Contains(html, fmt.Sprintf(`data-block="%d"`, a.Block)) {
		t.Fatalf("block %d missing data-block in chunk %d", a.Block, ci)
	}
	// fragment 解析失败（清洗丢弃）时 Block 回退 spine 首块，locator 解析
	// 其首个真实可见内容，SourceFragment 原样保留
	book.TOC = append(book.TOC, reader.TocEntry{Label: "丢失", Path: "OEBPS/ch2.xhtml", Fragment: "no-such-id", Depth: 1})
	again, err := Build(book)
	if err != nil {
		t.Fatal(err)
	}
	lost := again.Manifest.TOC[3]
	if lost.SourceFragment != "no-such-id" || lost.Block != again.Manifest.Spines[1].BlockStart {
		t.Fatalf("missing fragment fallback wrong: %+v (spine start %d)", lost, again.Manifest.Spines[1].BlockStart)
	}
	if len(lost.TextPath) == 0 {
		t.Fatalf("lost fragment should resolve first visible text: %+v", lost)
	}
	if got := resolveChunkText(t, again.Chunks[lost.Chunk].HTML, lost.Block, lost.TextPath); got != "甲处" {
		t.Fatalf("lost fragment text locator resolves %q, want 甲处", got)
	}
}

// navAnchorAttr 在块 HTML 中查找 data-rv-anchor 属性值等于 want 的元素，
// 返回其标签名；找不到返回 ""。
func navAnchorAttr(t *testing.T, chunkHTML, want string) string {
	t.Helper()
	for _, n := range parseChunk(chunkHTML, t) {
		var walk func(x *html.Node) string
		walk = func(x *html.Node) string {
			if x.Type == html.ElementNode {
				for _, a := range x.Attr {
					if a.Key == "data-rv-anchor" && a.Val == want {
						return x.Data
					}
				}
			}
			for c := x.FirstChild; c != nil; c = c.NextSibling {
				if got := walk(c); got != "" {
					return got
				}
			}
			return ""
		}
		if got := walk(n); got != "" {
			return got
		}
	}
	return ""
}

// resolveChunkText 把 chunk HTML 重新解析（等价浏览器 innerHTML），按块
// data-block + text path 走到目标节点，返回其文本。验证 text locator 的
// DOM path 跨「服务端解析树 → 序列化 → 客户端再解析」往返一致。
func resolveChunkText(t *testing.T, chunkHTML string, block int, path []int) string {
	t.Helper()
	for _, n := range parseChunk(chunkHTML, t) {
		blk, ok := findDataBlock(n, block)
		if !ok {
			continue
		}
		cur := blk
		for _, idx := range path {
			i := 0
			var next *html.Node
			for c := cur.FirstChild; c != nil; c = c.NextSibling {
				if i == idx {
					next = c
					break
				}
				i++
			}
			if next == nil {
				t.Fatalf("path %v dead-ends in block %d", path, block)
			}
			cur = next
		}
		if cur.Type != html.TextNode {
			t.Fatalf("path %v lands on %v node, want text", path, cur.Type)
		}
		return cur.Data
	}
	t.Fatalf("block %d not found in chunk", block)
	return ""
}

// findDataBlock 在子树内查找 data-block 属性等于 want 的元素。
func findDataBlock(root *html.Node, want int) (*html.Node, bool) {
	var walk func(n *html.Node) *html.Node
	walk = func(n *html.Node) *html.Node {
		if n.Type == html.ElementNode {
			for _, a := range n.Attr {
				if a.Key == "data-block" {
					var got int
					fmt.Sscanf(a.Val, "%d", &got)
					if got == want {
						return n
					}
				}
			}
		}
		for c := n.FirstChild; c != nil; c = c.NextSibling {
			if hit := walk(c); hit != nil {
				return hit
			}
		}
		return nil
	}
	if hit := walk(root); hit != nil {
		return hit, true
	}
	return nil, false
}

// TestBuildEPUBNavAnchorBinding 验证导航 locator 解析规则：
//   - 图片/SVG 目标 → data-rv-anchor 直接落在媒体元素上（NavAnchor 输出）；
//   - 文本目标 → 不注入任何 DOM 标记，manifest 保存实际文本节点的
//     DOM path + UTF-16 偏移（跨序列化往返可解析到同一文本）；
//   - 目标无可见内容（空 inline 锚点）→ 沿文档序向后解析块内下一可见文本；
//   - 无 fragment → 目标 spine 首个真实可见内容；
//   - 同一目标去重共享 id；manifest 携带 chunk / source_path / source_fragment。
func TestBuildEPUBNavAnchorBinding(t *testing.T) {
	ch1 := `<p data-source-path="OEBPS/a.xhtml">开篇段落。</p>`
	ch2 := `<p data-source-path="OEBPS/b.xhtml">章首文字。</p>` +
		`<p class="main" id="cover-t" data-source-path="OEBPS/b.xhtml"><img src="/assets/7" width="100" height="200" alt="整页标题图"></p>` +
		`<p data-source-path="OEBPS/b.xhtml">图后文字<span id="empty-t"></span>锚点后正文。</p>`
	ch3 := `<h1 id="text-t" data-source-path="OEBPS/c.xhtml">第三章 标题</h1>` +
		`<p data-source-path="OEBPS/c.xhtml">第三章正文。</p>`
	book := &reader.Book{
		Format:   "epub",
		Title:    "导航书",
		Chapters: []reader.Chapter{{HTML: ch1}, {HTML: ch2}, {HTML: ch3}},
		TOC: []reader.TocEntry{
			{Label: "图章", Path: "OEBPS/b.xhtml", Fragment: "cover-t", Depth: 0},
			{Label: "章首", Path: "OEBPS/b.xhtml", Depth: 0},
			{Label: "文本", Path: "OEBPS/c.xhtml", Fragment: "text-t", Depth: 0},
			{Label: "图章二", Path: "OEBPS/b.xhtml", Fragment: "cover-t", Depth: 0},
			{Label: "空锚", Path: "OEBPS/b.xhtml", Fragment: "empty-t", Depth: 0},
		},
	}
	built, err := Build(book)
	if err != nil {
		t.Fatal(err)
	}
	m := built.Manifest
	if len(m.TOC) != 5 {
		t.Fatalf("toc=%+v", m.TOC)
	}
	chunkOf := func(block int) string {
		ci := m.ChunkForBlock(block)
		if ci < 0 {
			t.Fatalf("block %d not in any chunk", block)
		}
		return built.Chunks[ci].HTML
	}
	// 图片目标：绑定直接落在媒体元素上（唯一进 DOM 的标记）
	if m.TOC[0].NavAnchor != "rvn-0" || navAnchorAttr(t, chunkOf(m.TOC[0].Block), "rvn-0") != "img" {
		t.Fatalf("media target should bind on img: %+v", m.TOC[0])
	}
	// 无 fragment：解析 spine 首个真实可见文本
	if m.TOC[1].NavAnchor != "" || len(m.TOC[1].TextPath) == 0 {
		t.Fatalf("no-fragment target should carry text locator: %+v", m.TOC[1])
	}
	if m.TOC[1].Block != m.Spines[1].BlockStart {
		t.Fatalf("no-fragment block should be spine start: %+v", m.TOC[1])
	}
	if got := resolveChunkText(t, chunkOf(m.TOC[1].Block), m.TOC[1].Block, m.TOC[1].TextPath); got != "章首文字。" {
		t.Fatalf("no-fragment text locator resolves %q", got)
	}
	// 文本目标：locator 落在标题文本节点
	if m.TOC[2].NavAnchor != "" || len(m.TOC[2].TextPath) == 0 || m.TOC[2].TextOffset != 0 {
		t.Fatalf("text target should carry text locator: %+v", m.TOC[2])
	}
	if got := resolveChunkText(t, chunkOf(m.TOC[2].Block), m.TOC[2].Block, m.TOC[2].TextPath); got != "第三章 标题" {
		t.Fatalf("text locator resolves %q, want title text", got)
	}
	// 同一目标去重共享 id
	if m.TOC[3].NavAnchor != "rvn-0" {
		t.Fatalf("duplicate target should share nav anchor: %+v", m.TOC[3])
	}
	// 空 inline 锚点：沿文档序向后解析到下一可见文本
	if m.TOC[4].NavAnchor != "" || len(m.TOC[4].TextPath) == 0 {
		t.Fatalf("empty inline target should resolve forward: %+v", m.TOC[4])
	}
	if got := resolveChunkText(t, chunkOf(m.TOC[4].Block), m.TOC[4].Block, m.TOC[4].TextPath); got != "锚点后正文。" {
		t.Fatalf("empty inline locator resolves %q", got)
	}
	// chunk 字段与块所在 chunk 一致；source 字段保留
	for _, e := range m.TOC {
		if e.Chunk != m.ChunkForBlock(e.Block) {
			t.Fatalf("toc chunk mismatch: %+v", e)
		}
	}
	if m.TOC[0].SourcePath != "OEBPS/b.xhtml" || m.TOC[0].SourceFragment != "cover-t" || m.TOC[1].SourceFragment != "" {
		t.Fatalf("source fields wrong: %+v / %+v", m.TOC[0], m.TOC[1])
	}
	// id 分配确定：重建一致
	again, err := Build(book)
	if err != nil {
		t.Fatal(err)
	}
	for i := range m.TOC {
		if again.Manifest.TOC[i].NavAnchor != m.TOC[i].NavAnchor || again.Manifest.TOC[i].Chunk != m.TOC[i].Chunk {
			t.Fatalf("nav anchor not deterministic: %+v vs %+v", m.TOC[i], again.Manifest.TOC[i])
		}
		if anchorsEqual(again.Manifest.TOC[i].TextPath, m.TOC[i].TextPath) == false || again.Manifest.TOC[i].TextOffset != m.TOC[i].TextOffset {
			t.Fatalf("text locator not deterministic: %+v vs %+v", m.TOC[i], again.Manifest.TOC[i])
		}
	}
	// 文本目标零 DOM 改动：total_chars 与无目录构建一致；唯一可能进 DOM
	// 的标记是媒体目标上的 data-rv-anchor（无 toc-anchor 内联标记）
	bookNoTOC := *book
	bookNoTOC.TOC = nil
	clean, err := Build(&bookNoTOC)
	if err != nil {
		t.Fatal(err)
	}
	if clean.Manifest.TotalChars != m.TotalChars {
		t.Fatalf("locators changed total_chars: %d vs %d", m.TotalChars, clean.Manifest.TotalChars)
	}
	for i := range built.Chunks {
		if strings.Contains(built.Chunks[i].HTML, "toc-anchor") {
			t.Fatalf("chunk %d still carries inline marker", i)
		}
	}
}

func anchorsEqual(a, b []int) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

// TestBuildChunksNavAnchorSplit 验证 chunk 在 NavAnchor 附近的安全边界
// 切分：目标块之前的累计文本过半时，NavAnchor 块作为新 chunk 首块，
// 目录跳转只需加载该 chunk。
func TestBuildChunksNavAnchorSplit(t *testing.T) {
	para := strings.Repeat("目录跳转目标之前的普通段落内容。", 12) // ≈180 字/段
	var b strings.Builder
	for i := 0; i < 30; i++ { // ≈5400 字：超过半目标、未达整目标
		fmt.Fprintf(&b, `<p data-source-path="OEBPS/s.xhtml">%s（%d）</p>`, para, i)
	}
	b.WriteString(`<h1 id="target" data-source-path="OEBPS/s.xhtml">目标标题</h1>`)
	for i := 0; i < 10; i++ {
		fmt.Fprintf(&b, `<p data-source-path="OEBPS/s.xhtml">目标后的段落 %d。</p>`, i)
	}
	book := &reader.Book{
		Format:   "epub",
		Title:    "切分书",
		Chapters: []reader.Chapter{{HTML: b.String()}},
		TOC:      []reader.TocEntry{{Label: "目标", Path: "OEBPS/s.xhtml", Fragment: "target", Depth: 0}},
	}
	built, err := Build(book)
	if err != nil {
		t.Fatal(err)
	}
	m := built.Manifest
	if len(m.Chunks) < 2 {
		t.Fatalf("expected multiple chunks, got %d", len(m.Chunks))
	}
	target := m.TOC[0].Block
	ci := m.ChunkForBlock(target)
	if m.Chunks[ci].BlockStart != target {
		t.Fatalf("nav target block should start its chunk: target=%d chunk=%+v", target, m.Chunks[ci])
	}
	if m.TOC[0].Chunk != ci || m.TOC[0].NavAnchor != "" || len(m.TOC[0].TextPath) == 0 {
		t.Fatalf("toc nav fields wrong: %+v", m.TOC[0])
	}
	if got := resolveChunkText(t, built.Chunks[ci].HTML, target, m.TOC[0].TextPath); got != "目标标题" {
		t.Fatalf("target text locator resolves %q", got)
	}
}

func parseChunk(htmlStr string, t *testing.T) []*html.Node {
	t.Helper()
	ctx := &html.Node{Type: html.ElementNode, Data: "body", DataAtom: atom.Body}
	nodes, err := html.ParseFragment(strings.NewReader(htmlStr), ctx)
	if err != nil {
		t.Fatal(err)
	}
	return nodes
}

// textOfFragment 解析片段并拼接其全部文本（与浏览器 textContent 对齐）。
func textOfFragment(htmlStr string, t *testing.T) string {
	var b strings.Builder
	for _, n := range parseChunk(htmlStr, t) {
		b.WriteString(nodeText(n))
	}
	return b.String()
}

func nodeText(n *html.Node) string {
	var b strings.Builder
	var walk func(x *html.Node)
	walk = func(x *html.Node) {
		if x.Type == html.TextNode {
			b.WriteString(x.Data)
		}
		for c := x.FirstChild; c != nil; c = c.NextSibling {
			walk(c)
		}
	}
	walk(n)
	return b.String()
}

// TestBuildEPUBNestedInlineTextLocator 验证文本 locator 在嵌套 inline 结构
// 中指向真实文本节点：path 逐层穿过 strong/span，偏移跳过前导空白；且
// 「服务端解析树 → 序列化 → 客户端再解析」往返后仍解析到同一文本。
func TestBuildEPUBNestedInlineTextLocator(t *testing.T) {
	ch1 := `<p data-source-path="OEBPS/a.xhtml">前言<strong>粗体<span id="deep">目标文本</span></strong>尾文</p>`
	ch2 := `<h2 id="ws" data-source-path="OEBPS/b.xhtml"> 标题带前导空白</h2>`
	book := &reader.Book{
		Format:   "epub",
		Title:    "嵌套书",
		Chapters: []reader.Chapter{{HTML: ch1}, {HTML: ch2}},
		TOC: []reader.TocEntry{
			{Label: "深层", Path: "OEBPS/a.xhtml", Fragment: "deep", Depth: 0},
			{Label: "空白", Path: "OEBPS/b.xhtml", Fragment: "ws", Depth: 0},
		},
	}
	built, err := Build(book)
	if err != nil {
		t.Fatal(err)
	}
	m := built.Manifest
	deep, ws := m.TOC[0], m.TOC[1]
	if !anchorsEqual(deep.TextPath, []int{1, 1, 0}) || deep.TextOffset != 0 {
		t.Fatalf("deep locator path/offset wrong: %+v", deep)
	}
	if got := resolveChunkText(t, built.Chunks[deep.Chunk].HTML, deep.Block, deep.TextPath); got != "目标文本" {
		t.Fatalf("deep locator resolves %q", got)
	}
	if ws.TextOffset != 1 {
		t.Fatalf("leading whitespace offset = %d, want 1", ws.TextOffset)
	}
	if got := resolveChunkText(t, built.Chunks[ws.Chunk].HTML, ws.Block, ws.TextPath); got != " 标题带前导空白" {
		t.Fatalf("ws locator resolves %q", got)
	}
}

// TestBuildSpineStartBoundary 验证每个 spine 起始块（全书首块除外）注入
// data-spine-start（客户端以其为稳定分页边界，施加 break-before:column）。
func TestBuildSpineStartBoundary(t *testing.T) {
	sp := func(id string) string {
		return fmt.Sprintf(`<h1 id="%s" data-source-path="OEBPS/%s.xhtml">%s 标题</h1>`, id, id, id) +
			`<p data-source-path="OEBPS/s.xhtml">` + strings.Repeat("章内正文内容，用来形成多个内容块。", 30) + `</p>`
	}
	book := &reader.Book{
		Format:   "epub",
		Title:    "边界书",
		Chapters: []reader.Chapter{{HTML: sp("c1")}, {HTML: sp("c2")}, {HTML: sp("c3")}},
		TOC: []reader.TocEntry{
			{Label: "c1", Path: "OEBPS/c1.xhtml", Depth: 0},
			{Label: "c2", Path: "OEBPS/c2.xhtml", Depth: 0},
			{Label: "c3", Path: "OEBPS/c3.xhtml", Depth: 0},
		},
	}
	built, err := Build(book)
	if err != nil {
		t.Fatal(err)
	}
	m := built.Manifest
	if len(m.Spines) != 3 {
		t.Fatalf("spines=%+v", m.Spines)
	}
	hasAttr := func(block int, attr string) bool {
		html := built.Chunks[m.ChunkForBlock(block)].HTML
		return strings.Contains(html, fmt.Sprintf(`data-block="%d" %s`, block, attr))
	}
	if hasAttr(0, "data-spine-start") {
		t.Fatal("book start must not carry data-spine-start")
	}
	if !hasAttr(m.Spines[1].BlockStart, "data-spine-start") || !hasAttr(m.Spines[2].BlockStart, "data-spine-start") {
		t.Fatal("spine start blocks (except book start) must carry data-spine-start")
	}
	// 非 spine 起始块不得携带
	if hasAttr(m.Spines[1].BlockStart+1, "data-spine-start") {
		t.Fatal("non-start block carries data-spine-start")
	}

	// TXT 章节等价：每章起始块（首章除外）同样注入
	text := "第一章 开头\n" + strings.Repeat("第一章正文行。\n", 30) +
		"\n第二章 继续\n" + strings.Repeat("第二章正文行。\n", 30)
	tbook, err := reader.Parse("book.txt", strings.NewReader(text), int64(len(text)), "", "")
	if err != nil {
		t.Fatal(err)
	}
	tbuilt, err := Build(tbook)
	if err != nil {
		t.Fatal(err)
	}
	tm := tbuilt.Manifest
	if len(tm.Spines) < 2 {
		t.Fatalf("txt spines=%+v", tm.Spines)
	}
	txtHasAttr := func(block int, attr string) bool {
		html := tbuilt.Chunks[tm.ChunkForBlock(block)].HTML
		return strings.Contains(html, fmt.Sprintf(`data-block="%d" %s`, block, attr))
	}
	if txtHasAttr(0, "data-spine-start") || !txtHasAttr(tm.Spines[1].BlockStart, "data-spine-start") {
		t.Fatalf("txt spine-start injection wrong: spine[1].start=%d", tm.Spines[1].BlockStart)
	}
}

// TestBuildVoidBlockSerialization void 元素块（img）的序列化必须携带完整
// 的 '/>'：此前 injectDataBlock 漏掉结尾 '>'，浏览器解析 chunk HTML 时会把
// 后续块全部吞进属性（真实书目中目录跳转目标整块丢失）。
func TestBuildVoidBlockSerialization(t *testing.T) {
	book := &reader.Book{
		Format: "epub",
		Title:  "图册",
		Chapters: []reader.Chapter{
			{HTML: `<p data-source-path="OEBPS/i.xhtml">第一段</p>` +
				`<p data-source-path="OEBPS/i.xhtml"><img src="/a/0" width="10" height="20" alt="x"></p>` +
				`<p data-source-path="OEBPS/i.xhtml">图后文字</p>`},
		},
	}
	built, err := Build(book)
	if err != nil {
		t.Fatal(err)
	}
	html := built.Chunks[0].HTML
	if !strings.Contains(html, `/>`) {
		t.Fatalf("void block not self-closed: %s", html)
	}
	// 重新解析（等价浏览器 innerHTML）：三个顶层块都在，data-block 连续
	nodes := parseChunk(html, t)
	if len(nodes) != 3 {
		t.Fatalf("chunk top-level elements = %d, want 3 (swallowed by malformed tag)", len(nodes))
	}
	for want := 0; want < 3; want++ {
		if _, ok := findDataBlock(nodes[want], want); !ok {
			t.Fatalf("top-level element %d lost its data-block: %s", want, html)
		}
	}
}

func TestBuildTXTContinuityAndTOC(t *testing.T) {
	// 三章结构的文本，正文行足够多以便分行分块。
	var b strings.Builder
	b.WriteString("第一章 开头\n")
	for i := 0; i < 160; i++ {
		fmt.Fprintf(&b, "这是第一章第 %d 行的正文内容，用于验证分块与连续性。\n", i)
	}
	b.WriteString("\n第二章 内容\n")
	for i := 0; i < 220; i++ {
		fmt.Fprintf(&b, "第二章正文第 %d 行，同样是比较长的句子内容以撑大文本量。\n", i)
	}
	b.WriteString("\n第三章 结尾\n结尾段落，很短。\n")
	text := b.String()
	// 用 reader 的探测正则确保标题被识别，避免手算偏移
	book, err := reader.Parse("book.txt", strings.NewReader(text), int64(len(text)), "", "")
	if err != nil {
		t.Fatal(err)
	}
	built, err := Build(book)
	if err != nil {
		t.Fatal(err)
	}
	m := built.Manifest
	if m.Format != "txt" || len(book.TOC) < 2 {
		t.Fatalf("txt toc=%+v", book.TOC)
	}
	// 文本逐字保留（chunk 拼接）
	var joined strings.Builder
	for _, ch := range built.Chunks {
		for _, n := range parseChunk(ch.HTML, t) {
			joined.WriteString(nodeText(n))
		}
	}
	if joined.String() != text {
		t.Fatalf("txt text mismatch:\nwant=%q\ngot =%q", text, joined.String())
	}
	if m.TotalChars != utf16UnitCount(text) {
		t.Fatalf("txt total_chars=%d want %d", m.TotalChars, utf16UnitCount(text))
	}
	// 每个目录目标 = 对应章首块
	for _, e := range m.TOC {
		sp := m.Spines[e.Spine]
		if e.Block != sp.BlockStart {
			t.Fatalf("txt toc %q spine=%d block=%d want blockStart=%d", e.Label, e.Spine, e.Block, sp.BlockStart)
		}
	}
	// TXT 目录目标没有 fragment 语义：JSON 不应出现该字段（omitempty）
	raw, _ := json.Marshal(m.TOC[0])
	if strings.Contains(string(raw), "fragment") {
		t.Fatalf("txt toc should not carry fragment: %s", raw)
	}
	// 全部块号连续
	prev := -1
	for _, c := range m.Chunks {
		for i := c.BlockStart; i < c.BlockStart+c.BlockCount; i++ {
			if i != prev+1 {
				t.Fatalf("block numbering gap at %d (prev %d)", i, prev)
			}
			prev = i
		}
	}
}

func TestBuildEPUBImageBlockIsAddressable(t *testing.T) {
	book := &reader.Book{
		Format:   "epub",
		Title:    "图册",
		Chapters: []reader.Chapter{{HTML: `<p data-source-path="OEBPS/i.xhtml">第一段</p><p data-source-path="OEBPS/i.xhtml"><img src="/a/0" width="10" height="20" alt="x"></p><p data-source-path="OEBPS/i.xhtml">图后文字</p>`}},
		TOC:      nil,
	}
	built, err := Build(book)
	if err != nil {
		t.Fatal(err)
	}
	m := built.Manifest
	if m.TotalBlocks() != 3 {
		t.Fatalf("blocks=%+v", m.Spines)
	}
	chunk := built.Chunks[0].HTML
	if !strings.Contains(chunk, `data-block="1"`) {
		t.Fatalf("img block missing data-block: %s", chunk)
	}
	if !strings.Contains(chunk, `width="10"`) || !strings.Contains(chunk, `height="20"`) {
		t.Fatalf("image intrinsic size lost: %s", chunk)
	}
}

func TestAnchorLegacyJSONMigration(t *testing.T) {
	// 新格式往返
	a := Anchor{Spine: 1, Block: 42, Path: []int{0, 2}, Offset: 7}
	raw, _ := json.Marshal(a)
	var back Anchor
	if err := json.Unmarshal(raw, &back); err != nil {
		t.Fatal(err)
	}
	if back.Compare(a) != 0 {
		t.Fatalf("round trip: %+v", back)
	}
	// 旧格式（无 block）：path[0] → block
	legacy := []byte(`{"spine":2,"path":[5,1,3],"offset":9}`)
	if err := json.Unmarshal(legacy, &back); err != nil {
		t.Fatal(err)
	}
	if back.Spine != 2 || back.Block != 5 || len(back.Path) != 2 || back.Path[0] != 1 || back.Offset != 9 {
		t.Fatalf("legacy migration: %+v", back)
	}
	// 旧格式空 path（章根边界）→ block 0
	legacy = []byte(`{"spine":3,"path":[],"offset":-1}`)
	if err := json.Unmarshal(legacy, &back); err != nil {
		t.Fatal(err)
	}
	if back.Spine != 3 || back.Block != 0 || len(back.Path) != 0 || back.Offset != -1 {
		t.Fatalf("legacy empty migration: %+v", back)
	}
	if !a.Valid() {
		t.Fatal("valid anchor rejected")
	}
}

func TestAnchorCompareAndValid(t *testing.T) {
	ordered := []Anchor{
		{Spine: 0, Block: 0, Offset: -1},
		{Spine: 0, Block: 1, Offset: -1},
		{Spine: 0, Block: 1, Path: []int{0}, Offset: 0},
		{Spine: 0, Block: 1, Path: []int{0}, Offset: 5},
		{Spine: 0, Block: 2, Path: []int{0}, Offset: 0},
		{Spine: 1, Block: 0, Offset: -1},
	}
	for i := 0; i < len(ordered); i++ {
		if ordered[i].Compare(ordered[i]) != 0 {
			t.Fatalf("self compare %d", i)
		}
		for j := i + 1; j < len(ordered); j++ {
			if ordered[i].Compare(ordered[j]) >= 0 || ordered[j].Compare(ordered[i]) <= 0 {
				t.Fatalf("order violated at %d,%d", i, j)
			}
		}
	}
	for _, bad := range []Anchor{
		{Spine: -1}, {Block: -1}, {Offset: -2}, {Offset: 1 << 27},
		{Path: []int{-1}}, {Path: []int{1 << 21}},
	} {
		if bad.Valid() {
			t.Fatalf("invalid anchor accepted: %+v", bad)
		}
	}
}

func TestManifestLookups(t *testing.T) {
	m := &Manifest{
		Spines: []SpineMeta{{BlockStart: 0, BlockCount: 10}, {BlockStart: 10, BlockCount: 5}},
		Chunks: []ChunkMeta{
			{Index: 0, BlockStart: 0, BlockCount: 7, Chars: 100},
			{Index: 1, BlockStart: 7, BlockCount: 8, Chars: 200},
		},
	}
	if m.SpineForBlock(0) != 0 || m.SpineForBlock(9) != 0 || m.SpineForBlock(10) != 1 || m.SpineForBlock(14) != 1 {
		t.Fatalf("SpineForBlock wrong")
	}
	if m.ChunkForBlock(6) != 0 || m.ChunkForBlock(7) != 1 || m.ChunkForBlock(99) != -1 {
		t.Fatalf("ChunkForBlock wrong")
	}
	if m.CharsBeforeChunk(1) != 100 {
		t.Fatalf("CharsBeforeChunk=%d", m.CharsBeforeChunk(1))
	}
	if m.TotalBlocks() != 15 {
		t.Fatal("TotalBlocks")
	}
}
