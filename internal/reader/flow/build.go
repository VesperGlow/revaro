package flow

import (
	"bytes"
	"fmt"
	stdhtml "html"
	"strings"

	"unicode/utf8"

	"github.com/VesperGlow/revaro/internal/reader"
	"golang.org/x/net/html"
	"golang.org/x/net/html/atom"
)

// 常量与体量预算（纯启发式，与视口/字号无关；chunk 只负责「适量」）。
const (
	// FlowFormatVersion 参与缓存键与 manifest.version：生成语义变化时必须递增。
	FlowFormatVersion = 1

	// chunkCharsTarget 是单个 chunk 的文本目标量（UTF-16 码元）。
	// 约合移动端 8~12 页 / 桌面 5~8 页，窗口化加载时够用且不过重。
	chunkCharsTarget = 7000
	// chunkBytesTarget 是单个 chunk 的 HTML 字节目标量；块是原子单位，
	// 单块超预算时允许超出（罕见大表/巨段）。
	chunkBytesTarget = 96 << 10
	// txtBlockCharsTarget 是 TXT 内容块的目标体量（行内切分，行是原子）。
	txtBlockCharsTarget = 2000
	// MaxSpines / MaxBlocks 防御性上限。
	MaxSpines = 1 << 14
	MaxBlocks = 1 << 24
)

// Chunk 是一次生成的 chunk 产物（HTML 片段 + 元数据）。
type Chunk struct {
	HTML string
	Meta ChunkMeta
}

// Built 是一次 flow 生成的完整产物。
type Built struct {
	Manifest *Manifest
	Chunks   []Chunk
}

// blockBuild 是单个顶层内容块的构建中间态。
type blockBuild struct {
	global int      // 全书 data-block 编号
	html   string   // 序列化片段（含 data-block）
	chars  int64    // 文本 UTF-16 长度
	ids    []string // 块内 id 与 data-frag-ids（目录片段定位）
}

// spineBuild 是一章的内容块。
type spineBuild struct {
	blocks      []blockBuild
	startGlobal int // 本章第一块的全书编号
}

// tocEntry 是带目标块的目录条目。
type tocEntry struct {
	label string
	depth int
	spine int
	block int
}

// Build 由解析好的 Book 生成连续 reading flow。纯函数、确定性：
// 同一输入总是产生逐字节相同的 manifest 与 chunk（用于内容级缓存）。
// book 需要保持默认解析输出不变（chunk 以它为准）。
func Build(book *reader.Book, assetBase string) (*Built, error) {
	switch book.Format {
	case "epub":
		return buildEPUB(book)
	case "txt":
		return buildTXT(book)
	default:
		return nil, fmt.Errorf("不支持的格式: %s", book.Format)
	}
}

// ---- EPUB ----

func buildEPUB(book *reader.Book) (*Built, error) {
	if len(book.Chapters) > MaxSpines {
		return nil, fmt.Errorf("EPUB 章节数过多")
	}
	spines := make([]spineBuild, 0, len(book.Chapters))
	global := 0
	for _, ch := range book.Chapters {
		blocks, err := chapterBlocks(ch.HTML)
		if err != nil {
			// 解析失败的书仍要可读：退化为一整块原文占位。
			blocks = []blockBuild{{html: "<p>" + stdhtml.EscapeString(ch.HTML) + "</p>"}}
		}
		sb := spineBuild{blocks: make([]blockBuild, 0, len(blocks)), startGlobal: global}
		for _, b := range blocks {
			b.global = global
			sb.blocks = append(sb.blocks, b)
			global++
		}
		if global > MaxBlocks {
			return nil, fmt.Errorf("内容块数量超限")
		}
		spines = append(spines, sb)
	}

	// 目录条目 → (spine, 章内块号)
	entries := make([]tocEntry, 0, len(book.TOC))
	for _, entry := range book.TOC {
		spine := chapterIndexForPath(book, entry.Path, spines)
		block := fragmentBlock(spines, spine, entry.Fragment)
		entries = append(entries, tocEntry{label: entry.Label, depth: entry.Depth, spine: spine, block: block})
	}
	return assemble(spines, entries, "epub")
}

// chapterBlocks 把一章清洗后的 HTML 切为顶层内容块；数据块必须携带
// data-block 与块内 id 索引，供目录与锚点定位。
func chapterBlocks(chapterHTML string) ([]blockBuild, error) {
	ctx := &html.Node{Type: html.ElementNode, Data: "body", DataAtom: atom.Body}
	frag, err := html.ParseFragment(strings.NewReader(chapterHTML), ctx)
	if err != nil {
		return nil, err
	}
	var out []blockBuild
	for _, node := range frag {
		if node.Type != html.ElementNode {
			continue
		}
		bb := blockBuild{}
		bb.html, bb.chars, bb.ids = serializeBlock(node)
		if bb.html == "" {
			continue
		}
		out = append(out, bb)
	}
	return out, nil
}

// serializeBlock 给顶层块注入 data-block 占位（编号由调用方补写），
// 序列化并收集块内文本长度与可定位 id。
func serializeBlock(node *html.Node) (htmlStr string, chars int64, ids []string) {
	collect := func(n *html.Node) {
		if n.Type != html.ElementNode {
			return
		}
		for _, a := range n.Attr {
			if a.Key == "id" && a.Val != "" {
				ids = append(ids, a.Val)
			}
			if a.Key == "data-frag-ids" && a.Val != "" {
				ids = append(ids, strings.Fields(a.Val)...)
			}
		}
	}
	var walk func(n *html.Node)
	walk = func(n *html.Node) {
		collect(n)
		for c := n.FirstChild; c != nil; c = c.NextSibling {
			walk(c)
		}
	}
	walk(node)
	var buf bytes.Buffer
	if err := html.Render(&buf, node); err != nil {
		return "", 0, nil
	}
	return buf.String(), textChars16(node), ids
}

// textChars16 返回子树内全部文本的 UTF-16 码元数（≈ 浏览器 textContent.length）。
func textChars16(n *html.Node) int64 {
	var total int64
	var walk func(x *html.Node)
	walk = func(x *html.Node) {
		if x.Type == html.TextNode {
			total += utf16UnitCount(x.Data)
		}
		for c := x.FirstChild; c != nil; c = c.NextSibling {
			walk(c)
		}
	}
	walk(n)
	return total
}

// chapterIndexForPath 把目录 Path 映射到章序号；找不到时回退第 0 章。
func chapterIndexForPath(book *reader.Book, tocPath string, spines []spineBuild) int {
	if tocPath == "" {
		return 0
	}
	for i, ch := range book.Chapters {
		if chapterSourcePath(ch.HTML) == normalizePath(tocPath) {
			if i < len(spines) {
				return i
			}
			return 0
		}
	}
	return 0
}

// chapterSourcePath 返回章节 HTML 首个块的 data-source-path（清洗器在每个
// 顶层块注入该属性）。
func chapterSourcePath(chapterHTML string) string {
	ctx := &html.Node{Type: html.ElementNode, Data: "body", DataAtom: atom.Body}
	frag, err := html.ParseFragment(strings.NewReader(chapterHTML), ctx)
	if err != nil {
		return ""
	}
	for _, node := range frag {
		if node.Type != html.ElementNode {
			continue
		}
		for _, a := range node.Attr {
			if a.Key == "data-source-path" && a.Val != "" {
				return a.Val
			}
		}
	}
	return ""
}

// fragmentBlock 返回某章内 fragment 目标块在章内的序号；找不到回退 0。
func fragmentBlock(spines []spineBuild, spine int, fragment string) int {
	if spine >= 0 && spine < len(spines) && fragment != "" {
		for i, b := range spines[spine].blocks {
			for _, id := range b.ids {
				if id == fragment {
					return i
				}
			}
		}
	}
	return 0
}

// ---- TXT ----

func buildTXT(book *reader.Book) (*Built, error) {
	chapters, markIndex := txtChapters(book.Text, book.TOC)
	if len(chapters) > MaxSpines {
		return nil, fmt.Errorf("文本章节数过多")
	}
	spines := make([]spineBuild, 0, len(chapters))
	global := 0
	for _, text := range chapters {
		start := global
		var blocks []blockBuild
		for _, part := range txtBlocks(text) {
			chars := utf16UnitCount(part)
			bb := blockBuild{
				html:  `<div class="txt-blk">` + stdhtml.EscapeString(part) + `</div>`,
				chars: chars,
			}
			blocks = append(blocks, bb)
			global++
		}
		if len(blocks) == 0 {
			blocks = append(blocks, blockBuild{html: `<div class="txt-blk"></div>`})
			global++
		}
		if global > MaxBlocks {
			return nil, fmt.Errorf("内容块数量超限")
		}
		spines = append(spines, spineBuild{blocks: blocks, startGlobal: start})
	}
	entries := make([]tocEntry, 0, len(markIndex))
	for _, mi := range markIndex {
		if mi.spine >= len(spines) {
			continue
		}
		entries = append(entries, tocEntry{label: mi.label, depth: mi.depth, spine: mi.spine, block: 0})
	}
	return assemble(spines, entries, "txt")
}

type txtMark struct {
	label string
	depth int
	spine int
}

// txtChapters 把 TXT 全文按目录偏移（UTF-16 码元）切成章节；
// markIndex 记录每条目录 → 章节序号。无目录且超长时按固定码元数兜底分段。
func txtChapters(text string, toc []reader.TocEntry) (chapters []string, marks []txtMark) {
	units := utf16UnitCount(text)
	type cut struct {
		byteOff int64
		label   string
		depth   int
	}
	var cuts []cut
	seen := map[int64]bool{}
	for _, entry := range toc {
		if entry.Offset <= 0 || entry.Offset >= units {
			continue
		}
		off := utf16OffsetToByte(text, entry.Offset)
		if seen[off] {
			continue
		}
		seen[off] = true
		cuts = append(cuts, cut{byteOff: off, label: entry.Label, depth: entry.Depth})
	}
	// 兜底：超长且没有目录的书按 30000 码元分段（切点对齐行尾）。
	if len(cuts) == 0 && units > 60000 {
		var byteCuts []int64
		var lastLineEnd int64
		unitsSeen := int64(0)
		for bytePos := range text {
			if text[bytePos] == '\n' {
				lastLineEnd = int64(bytePos + 1)
			}
			unitsSeen += utf16UnitsOfRune(runeAt(text, bytePos))
			if unitsSeen >= 30000 && lastLineEnd > 0 && lastLineEnd < int64(len(text)) {
				byteCuts = append(byteCuts, lastLineEnd)
				unitsSeen = 0
			}
		}
		byteCuts = append(byteCuts, int64(len(text)))
		prev := int64(0)
		for _, c := range byteCuts {
			if c <= prev {
				continue
			}
			chapters = append(chapters, text[prev:c])
			prev = c
		}
		if prev < int64(len(text)) {
			chapters = append(chapters, text[prev:])
		}
		return chapters, nil
	}
	cutBytes := make([]int64, 0, len(cuts)+1)
	cutBytes = append(cutBytes, 0)
	for _, c := range cuts {
		cutBytes = append(cutBytes, c.byteOff)
	}
	cutBytes = append(cutBytes, int64(len(text)))
	for i := 0; i < len(cutBytes)-1; i++ {
		start, end := cutBytes[i], cutBytes[i+1]
		if end <= start {
			continue
		}
		chapters = append(chapters, text[start:end])
	}
	if len(chapters) == 0 {
		chapters = []string{text}
	}
	// 偏移 0 的目录条目（标题位于全文开头）→ 第 0 章
	for _, c := range cuts {
		if c.byteOff == 0 {
			continue
		}
		idx := -1
		for j := 1; j < len(cutBytes) && idx < 0; j++ {
			if cutBytes[j] == c.byteOff {
				idx = j - 1
			}
		}
		if idx < 0 {
			continue
		}
		marks = append(marks, txtMark{label: c.label, depth: c.depth, spine: idx})
	}
	// 标题在全文开头（offset==0）的目录条目 → 第 0 章
	for _, entry := range toc {
		if entry.Offset != 0 {
			continue
		}
		marks = append(marks, txtMark{label: entry.Label, depth: entry.Depth, spine: 0})
	}
	return chapters, marks
}

// txtBlocks 把一章原文切成体量适中的行对齐文本块（行是原子，切点都在
// '\n' 之后；块与块拼接与原文逐字符一致，视觉上等价于连续 pre-wrap 文本）。
func txtBlocks(text string) []string {
	// 预扫描所有行的行尾（'\n' 后的字节位置），块边界只允许出现在这里。
	var lineEnds []int
	for i := 0; i < len(text); i++ {
		if text[i] == '\n' {
			lineEnds = append(lineEnds, i+1)
		}
	}
	var blocks []string
	start := 0
	// lastEnd：当前块结束候选游标
	for _, end := range lineEnds {
		if end <= start {
			continue
		}
		if utf16UnitCount(text[start:end]) >= txtBlockCharsTarget {
			blocks = append(blocks, text[start:end])
			start = end
			continue
		}
		// 段落末（下一个字符为 '\n' 即空行）或已到文件末尾 → 收块
		if end >= len(text) || text[end] == '\n' {
			if utf16UnitCount(text[start:end]) > 0 {
				blocks = append(blocks, text[start:end])
				start = end
			}
		}
	}
	if start < len(text) {
		blocks = append(blocks, text[start:])
	}
	return blocks
}

// ---- 装配（spines → chunk） ----

func assemble(spines []spineBuild, entries []tocEntry, format string) (*Built, error) {
	// 补写 data-block 编号到块 HTML
	manifest := &Manifest{
		Version: FlowFormatVersion,
		Format:  format,
		Spines:  make([]SpineMeta, 0, len(spines)),
	}
	spineBlockStart := make([]int, len(spines))
	var allBlocks []blockBuild
	for si := range spines {
		spineBlockStart[si] = spines[si].startGlobal
		manifest.Spines = append(manifest.Spines, SpineMeta{BlockStart: spines[si].startGlobal, BlockCount: len(spines[si].blocks)})
		for bi := range spines[si].blocks {
			b := &spines[si].blocks[bi]
			allBlocks = append(allBlocks, *b)
		}
	}
	// 全局编号由 startGlobal 链保证与 allBlocks 顺序一致（EPUB 直接连续，
	// TXT 亦然）。此处重新按序写入 data-block。
	var sb strings.Builder
	for i := range allBlocks {
		sb.Reset()
		if err := injectDataBlock(allBlocks[i].html, i, &sb); err != nil {
			return nil, err
		}
		allBlocks[i].html = sb.String()
	}
	// chunk
	chunks, totalChars := buildChunks(allBlocks)
	manifest.Chunks = chunks
	manifest.TotalChars = totalChars
	toc := make([]TOCTarget, 0, len(entries))
	for _, e := range entries {
		if e.spine < 0 || e.spine >= len(spines) {
			continue
		}
		// 章内块号 → 全书块号：章起点 + 块号
		toc = append(toc, TOCTarget{Label: e.label, Depth: e.depth, Spine: e.spine, Block: spineBlockStart[e.spine] + e.block})
	}
	manifest.TOC = toc
	built := &Built{Manifest: manifest, Chunks: make([]Chunk, 0, len(manifest.Chunks))}
	for _, cm := range manifest.Chunks {
		var buf strings.Builder
		for i := cm.BlockStart; i < cm.BlockStart+cm.BlockCount; i++ {
			buf.WriteString(allBlocks[i].html)
		}
		built.Chunks = append(built.Chunks, Chunk{HTML: buf.String(), Meta: cm})
	}
	return built, nil
}

// injectDataBlock 把 "data-block=\"N\"" 写进块 HTML 的开始标签。
// 块 HTML 由顶层元素序列化而来，第一个 '<tag' 即开始标签。
func injectDataBlock(blockHTML string, index int, out *strings.Builder) error {
	idx := strings.IndexByte(blockHTML, '>')
	if idx < 0 {
		return fmt.Errorf("非法块片段")
	}
	tag := blockHTML[:idx]
	// 只允许普通标签（<tag ...>），拒绝 '</'（不会出现）。
	if strings.HasPrefix(tag, "</") {
		return fmt.Errorf("非法块片段")
	}
	out.WriteString(tag)
	if len(tag) > 0 && tag[len(tag)-1] == '/' {
		// 自闭合标签（理论上不会出现，防御）：把 data-block 插到 '/' 之前
		out.Reset()
		out.WriteString(tag[:len(tag)-1])
		out.WriteString(fmt.Sprintf(` data-block="%d"/`, index))
		out.WriteString(blockHTML[idx+1:])
		return nil
	}
	out.WriteString(fmt.Sprintf(` data-block="%d"`, index))
	out.WriteString(blockHTML[idx:])
	return nil
}

// buildChunks 依据块体量切 chunk（块为原子单位）。
func buildChunks(blocks []blockBuild) ([]ChunkMeta, int64) {
	var chunks []ChunkMeta
	var total int64
	start := 0
	var chars, bytes int64
	flush := func(end int) {
		if end <= start {
			return
		}
		cm := ChunkMeta{
			Index:      len(chunks),
			BlockStart: start,
			BlockCount: end - start,
			Chars:      chars,
			Bytes:      int(bytes),
		}
		chunks = append(chunks, cm)
		total += chars
		start = end
		chars, bytes = 0, 0
	}
	for i, b := range blocks {
		chars += b.chars
		bytes += int64(len(b.html))
		if (chars >= chunkCharsTarget || bytes >= chunkBytesTarget) && i+1-start > 0 {
			flush(i + 1)
		}
	}
	flush(len(blocks))
	if len(chunks) == 0 {
		chunks = []ChunkMeta{{Index: 0, BlockStart: 0, BlockCount: 0}}
	}
	return chunks, total
}

// ---- UTF-16 工具（JS string.length 语义） ----

// utf16UnitCount 返回字符串的 UTF-16 码元数。
func utf16UnitCount(s string) int64 {
	n := int64(0)
	for _, r := range s {
		if r > 0xFFFF {
			n += 2
		} else {
			n++
		}
	}
	return n
}

// utf16OffsetToByte 把 UTF-16 码元偏移换算成字节偏移；偏移落在代理对
// 中间时按高位对齐（向前收），保证不会把代理对切开。
func utf16OffsetToByte(s string, offset int64) int64 {
	seen := int64(0)
	for i, r := range s {
		u := utf16UnitsOfRune(r)
		if seen+u > offset {
			return int64(i)
		}
		seen += u
	}
	return int64(len(s))
}

func utf16UnitsOfRune(r rune) int64 {
	if r > 0xFFFF {
		return 2
	}
	return 1
}

func runeAt(s string, bytePos int) rune {
	r, _ := utf8.DecodeRuneInString(s[bytePos:])
	return r
}

// normalizePath 与解析器同一语义（防御，供目录 Path 比对）。
func normalizePath(p string) string {
	trimmed := strings.TrimPrefix(p, "/")
	parts := strings.Split(trimmed, "/")
	out := make([]string, 0, len(parts))
	for _, part := range parts {
		switch part {
		case "", ".":
			continue
		case "..":
			if len(out) > 0 {
				out = out[:len(out)-1]
			}
		default:
			out = append(out, part)
		}
	}
	return strings.Join(out, "/")
}
