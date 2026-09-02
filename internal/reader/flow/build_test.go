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
	b1, err := Build(book, "/api/assets")
	if err != nil {
		t.Fatal(err)
	}
	b2, err := Build(book, "/api/assets")
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
	built, err := Build(book, "")
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
	built, err := Build(book, "")
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
