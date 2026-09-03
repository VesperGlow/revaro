package flow

import (
	"bytes"
	"fmt"
	stdhtml "html"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/VesperGlow/revaro/internal/reader"
	"golang.org/x/net/html"
	"golang.org/x/net/html/atom"
)

// 常量与体量预算（纯启发式，与视口/字号无关；chunk 只负责「适量」）。
const (
	// FlowFormatVersion 参与缓存键与 manifest.version：生成语义变化时必须递增。
	// v2：TOCTarget 保留 EPUB 目录 fragment（块内精确跳转），旧缓存需失效重建。
	// v3：TOC 升级为服务端 Stable NavAnchor——清洗期把 data-rv-anchor 绑定到
	//     实际可见目标节点，manifest 携带 {chunk, nav_anchor, source_path,
	//     source_fragment}，客户端目录跳转按绑定节点定栏；旧缓存需失效重建。
	// v4：文本目标不再注入空 inline 标记（toc-anchor），改为在 manifest 中
	//     保存实际文本节点的稳定 DOM path + UTF-16 偏移（text_path/
	//     text_offset），客户端解析真实 Text node 用 collapsed caret rect
	//     定栏；NavAnchor 只保留给媒体目标（真实元素 rect）。同时每个
	//     spine 起始块（全书首块除外）注入 data-spine-start，客户端 CSS
	//     对其施加 break-before:column，使 spine 起点成为稳定分页边界；
	//     旧缓存需失效重建。
	FlowFormatVersion = 4

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
	global     int        // 全书 data-block 编号
	node       *html.Node // EPUB：清洗后块的解析树（导航 locator 在序列化前解析）
	html       string     // 序列化片段（含 data-block 之前的原始块 HTML）
	chars      int64      // 文本 UTF-16 长度
	ids        []string   // 块内 id 与 data-frag-ids（目录 fragment 定位）
	navAnchors []string   // 绑定在本块内的导航目标 id（chunk 切分参考）
	spineStart bool       // spine 起始块：注入 data-spine-start（稳定分页边界）
}

// spineBuild 是一章的内容块。
type spineBuild struct {
	blocks      []blockBuild
	startGlobal int // 本章第一块的全书编号
}

// tocEntry 是带导航目标的目录条目。文本目标记录实际文本节点的稳定 DOM
// path（相对块元素的 childNodes 下标链）+ 首个可见字符的 UTF-16 偏移
//（textPath/textOffset）；媒体目标绑定 Stable NavAnchor id（真实元素
// rect）。fragment 保留 EPUB 目录原始片段（SourceFragment，调试与回退）；
// sourcePath 保留原始 href 路径。
type tocEntry struct {
	label      string
	depth      int
	spine      int
	block      int
	fragment   string
	sourcePath string
	navAnchor  string
	textPath   []int
	textOffset int
}

// chapterTree 是一章清洗后 HTML 的解析树：顶层内容块节点 + 每块可定位
// id + 章节源路径（目录 href 比对用）。序列化延后到 NavAnchor 绑定之后，
// 保证 data-rv-anchor 进入最终 chunk HTML。
type chapterTree struct {
	nodes      []*html.Node
	ids        [][]string
	sourcePath string
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

// buildEPUB 由解析好的 EPUB Book 生成连续 reading flow。纯函数、确定性：
// 同一输入总是产生逐字节相同的 manifest 与 chunk（用于内容级缓存）。
// 流程：解析各章清洗后 HTML → 目录条目解析真实内容目标并绑定 Stable
// NavAnchor（data-rv-anchor 注入块树）→ 序列化 → 切 chunk → 装配。
func buildEPUB(book *reader.Book) (*Built, error) {
	if len(book.Chapters) > MaxSpines {
		return nil, fmt.Errorf("EPUB 章节数过多")
	}
	spines := make([]spineBuild, 0, len(book.Chapters))
	trees := make([]*chapterTree, len(book.Chapters))
	global := 0
	for ci, ch := range book.Chapters {
		var blocks []blockBuild
		tree, err := parseChapter(ch.HTML)
		if err != nil {
			// 解析失败的书仍要可读：退化为一整块原文占位（无 NavAnchor）。
			trees[ci] = nil
			blocks = []blockBuild{{html: "<p>" + stdhtml.EscapeString(ch.HTML) + "</p>"}}
		} else {
			trees[ci] = tree
			blocks = make([]blockBuild, 0, len(tree.nodes))
			for bi, node := range tree.nodes {
				blocks = append(blocks, blockBuild{node: node, chars: textChars16(node), ids: tree.ids[bi]})
			}
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

	entries := resolveTOC(book, trees, spines)
	materialize(spines)
	return assemble(spines, entries, "epub")
}

// parseChapter 把一章清洗后 HTML 解析为顶层内容块树。渲染在此先做一次
// 以过滤空块；最终序列化在 NavAnchor 绑定之后（materialize），保证
// data-rv-anchor 进入最终 chunk HTML。
func parseChapter(chapterHTML string) (*chapterTree, error) {
	ctx := &html.Node{Type: html.ElementNode, Data: "body", DataAtom: atom.Body}
	frag, err := html.ParseFragment(strings.NewReader(chapterHTML), ctx)
	if err != nil {
		return nil, err
	}
	tree := &chapterTree{}
	for _, node := range frag {
		if node.Type != html.ElementNode {
			continue
		}
		var buf bytes.Buffer
		if err := html.Render(&buf, node); err != nil || buf.Len() == 0 {
			continue
		}
		if tree.sourcePath == "" {
			tree.sourcePath = attrOf(node, "data-source-path")
		}
		tree.nodes = append(tree.nodes, node)
		tree.ids = append(tree.ids, collectIDs(node))
	}
	return tree, nil
}

// collectIDs 收集节点子树内可定位 id（元素自身 id 与 data-frag-ids 清单）。
func collectIDs(node *html.Node) []string {
	var ids []string
	var walk func(n *html.Node)
	walk = func(n *html.Node) {
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
		for c := n.FirstChild; c != nil; c = c.NextSibling {
			walk(c)
		}
	}
	walk(node)
	return ids
}

func attrOf(node *html.Node, key string) string {
	for _, a := range node.Attr {
		if a.Key == key {
			return a.Val
		}
	}
	return ""
}

// findFragmentEl 在块树内按 id 查找 fragment 对应元素（含块自身），
// 按文档序取首个命中。
func findFragmentEl(block *html.Node, fragment string) *html.Node {
	if fragment == "" || block == nil {
		return nil
	}
	var walk func(n *html.Node) *html.Node
	walk = func(n *html.Node) *html.Node {
		if n.Type == html.ElementNode && attrOf(n, "id") == fragment {
			return n
		}
		for c := n.FirstChild; c != nil; c = c.NextSibling {
			if hit := walk(c); hit != nil {
				return hit
			}
		}
		return nil
	}
	return walk(block)
}

// isMediaEl 判断元素是否为整块可见媒体（清洗后单图 SVG 已拆为 img，
// 多图 SVG 保留 svg 外壳）。
func isMediaEl(n *html.Node) bool {
	return n.Data == "img" || n.Data == "svg" || n.Data == "video"
}

// docNextIn 返回 root 子树内 n 的文档序后继；越过 root 时返回 nil。
func docNextIn(root, n *html.Node) *html.Node {
	if n.FirstChild != nil {
		return n.FirstChild
	}
	for m := n; m != nil && m != root; m = m.Parent {
		if m.NextSibling != nil {
			return m.NextSibling
		}
	}
	return nil
}

// navTarget 是目录目标在清洗后块树内的绑定结果：
//   - 媒体目标：media 为 img/svg/video 元素，序列化时注入 data-rv-anchor，
//     客户端按真实元素 rect 定栏；
//   - 文本目标：text 为实际承载可见文本的 Text node，offset 是首个可见
//     字符在该节点内的 UTF-16 码元偏移。不向 DOM 注入任何标记——空
//     inline 元素在 column/page break 处可能停留在上一栏，而真实文本已
//     进入下一栏；manifest 保存 text 节点的稳定 DOM path + offset，客户端
//     加载 chunk 后解析真实 Text node，用 collapsed caret Range 的 rect
//     计算栏。
type navTarget struct {
	media  *html.Node
	text   *html.Node
	offset int
}

// resolveNavTarget 在 block 内自 start 起沿文档序寻找首个实际可见内容：
// 媒体元素直接命中；首个非空白文本节点（视觉空白如 NBSP/U+3000 跳过）
// 连同其可见字符偏移命中。目标子树没有可见内容（空 inline 锚点、纯空白
// 块等）时返回 false，客户端回退块起点。
func resolveNavTarget(block, start *html.Node) (navTarget, bool) {
	if block == nil || start == nil {
		return navTarget{}, false
	}
	for n := start; n != nil; n = docNextIn(block, n) {
		if n.Type == html.ElementNode && isMediaEl(n) {
			return navTarget{media: n}, true
		}
		if n.Type == html.TextNode {
			if offset, ok := firstVisibleOffset(n.Data); ok {
				return navTarget{text: n, offset: offset}, true
			}
		}
	}
	return navTarget{}, false
}

// firstVisibleOffset 返回 s 内首个非空白字符的 UTF-16 码元偏移；全是
// 空白时返回 false。与客户端 visualStart 的 /\S/ 语义对齐（跳过 Unicode
// 空白，不跳过正文里的任何可见字符）。
func firstVisibleOffset(s string) (int, bool) {
	units := 0
	for _, r := range s {
		if !unicode.IsSpace(r) {
			return units, true
		}
		units += int(utf16UnitsOfRune(r))
	}
	return 0, false
}

// childIndexOf 返回 child 在 parent 的 childNodes 下标；非直接子节点返回 -1。
func childIndexOf(parent, child *html.Node) int {
	i := 0
	for c := parent.FirstChild; c != nil; c = c.NextSibling {
		if c == child {
			return i
		}
		i++
	}
	return -1
}

// pathToNode 计算从 block 元素出发的 childNodes 下标链（与客户端
// readingAnchor 的 path 语义一致：含文本节点，空路径 = 块元素自身）。
// node 不在 block 子树内时返回 nil。
func pathToNode(block, node *html.Node) []int {
	if block == nil || node == nil {
		return nil
	}
	var path []int
	for cur := node; cur != nil && cur != block; cur = cur.Parent {
		if cur.Parent == nil {
			return nil
		}
		path = append([]int{childIndexOf(cur.Parent, cur)}, path...)
	}
	if node == block {
		return []int{}
	}
	return path
}

// resolveTOC 把目录条目解析为 (spine, 块) 并解析导航 locator：对每个
// href/path+fragment 先在清洗后 DOM 中定位真实目标元素，再解析其首个
// 实际可见内容——文本目标记录实际文本节点的稳定 DOM path + UTF-16 偏移
//（不注入任何 DOM 标记）；媒体目标把 data-rv-anchor 绑到媒体元素上。
// 无 fragment 条目解析目标 spine 首个真实可见内容（fragment 解析失败/
// 被清洗丢弃时同样落到目标块首可见内容，原始 fragment 保留为
// SourceFragment 供回退）。locator 解析失败（无可绑定节点）时全部留空，
// 客户端回退块起点。id 按目录序分配（rvn-<首现目录序号>），同一目标
// 去重共享，保证确定性；navAnchors 同时作为 chunk 切分的参考（目标块
// 尽量成为 chunk 首块）。
func resolveTOC(book *reader.Book, trees []*chapterTree, spines []spineBuild) []tocEntry {
	type binding struct {
		id     string
		target navTarget
		path   []int
	}
	entries := make([]tocEntry, 0, len(book.TOC))
	navIDs := map[string]binding{} // (spine, fragment) → 绑定结果
	navSeq := 0
	for _, entry := range book.TOC {
		spine := chapterIndexForPath(book, entry.Path, trees)
		blockIdx := fragmentBlock(spines, spine, entry.Fragment)
		var bound binding
		if spine >= 0 && spine < len(trees) && trees[spine] != nil && blockIdx < len(spines[spine].blocks) {
			key := strconv.Itoa(spine) + "\x00" + entry.Fragment
			if existing, ok := navIDs[key]; ok {
				bound = existing
			} else {
				b := &spines[spine].blocks[blockIdx]
				start := findFragmentEl(b.node, entry.Fragment)
				if start == nil {
					start = b.node
				}
				id := fmt.Sprintf("rvn-%d", navSeq)
				if target, ok := resolveNavTarget(b.node, start); ok {
					navSeq++
					if target.media != nil {
						target.media.Attr = append(target.media.Attr, html.Attribute{Key: "data-rv-anchor", Val: id})
						bound = binding{id: id, target: target}
					} else {
						bound = binding{id: id, target: target, path: pathToNode(b.node, target.text)}
					}
					b.navAnchors = append(b.navAnchors, id)
				}
				navIDs[key] = bound
			}
		}
		e := tocEntry{
			label:      entry.Label,
			depth:      entry.Depth,
			spine:      spine,
			block:      blockIdx,
			fragment:   entry.Fragment,
			sourcePath: entry.Path,
		}
		if bound.target.media != nil {
			e.navAnchor = bound.id
		} else if bound.target.text != nil {
			e.textPath = bound.path
			e.textOffset = bound.target.offset
		}
		entries = append(entries, e)
	}
	return entries
}

// materialize 在 NavAnchor 绑定完成后把 EPUB 块解析树序列化为块 HTML。
func materialize(spines []spineBuild) {
	for si := range spines {
		for bi := range spines[si].blocks {
			b := &spines[si].blocks[bi]
			if b.node == nil || b.html != "" {
				continue
			}
			var buf bytes.Buffer
			if err := html.Render(&buf, b.node); err != nil {
				continue
			}
			b.html = buf.String()
		}
	}
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
// chapters 的源路径来自解析树首个块的 data-source-path（清洗器在每个
// 顶层块注入该属性）。
func chapterIndexForPath(book *reader.Book, tocPath string, trees []*chapterTree) int {
	if tocPath == "" {
		return 0
	}
	normalized := normalizePath(tocPath)
	for i := range book.Chapters {
		if i < len(trees) && trees[i] != nil && trees[i].sourcePath == normalized {
			return i
		}
	}
	return 0
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
	// 补写 data-block 编号到块 HTML。spine 起始块（全书首块除外）同时注入
	// data-spine-start：客户端 CSS 对其施加 break-before:column，使每章
	// 起点成为稳定分栏边界——窗口虚拟化只允许以该边界（spine 起始块所在
	// chunk）为排版原点，当前 spine 的 page boundary 与窗口增删无关。
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
			b := spines[si].blocks[bi]
			if si > 0 && bi == 0 {
				b.spineStart = true
			}
			allBlocks = append(allBlocks, b)
		}
	}
	// 全局编号由 startGlobal 链保证与 allBlocks 顺序一致（EPUB 直接连续，
	// TXT 亦然）。此处重新按序写入 data-block。
	var sb strings.Builder
	for i := range allBlocks {
		sb.Reset()
		if err := injectDataBlock(allBlocks[i].html, i, allBlocks[i].spineStart, &sb); err != nil {
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
		block := spineBlockStart[e.spine] + e.block
		toc = append(toc, TOCTarget{
			Label:          e.label,
			Depth:          e.depth,
			Spine:          e.spine,
			Block:          block,
			NavAnchor:      e.navAnchor,
			TextPath:       e.textPath,
			TextOffset:     e.textOffset,
			Chunk:          chunkIndexForBlock(chunks, block),
			SourcePath:     e.sourcePath,
			SourceFragment: e.fragment,
		})
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

// injectDataBlock 把 "data-block=\"N\""（spine 起始块加
// "data-spine-start"）写进块 HTML 的开始标签。块 HTML 由顶层元素序列化
// 而来，第一个 '<tag' 即开始标签。
func injectDataBlock(blockHTML string, index int, spineStart bool, out *strings.Builder) error {
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
		// 自闭合标签（void 元素序列化形如 <img .../>）：把属性插到 '/' 之前
		// 并补回完整的 '/>'——漏掉 '>' 会让浏览器把后续块全部吞进属性里。
		out.Reset()
		out.WriteString(tag[:len(tag)-1])
		out.WriteString(fmt.Sprintf(` data-block="%d"`, index))
		if spineStart {
			out.WriteString(` data-spine-start`)
		}
		out.WriteString("/>")
		out.WriteString(blockHTML[idx+1:])
		return nil
	}
	out.WriteString(fmt.Sprintf(` data-block="%d"`, index))
	if spineStart {
		out.WriteString(` data-spine-start`)
	}
	out.WriteString(blockHTML[idx:])
	return nil
}

// chunkIndexForBlock 返回包含全书块 b 的 chunk 序号；找不到回退 0。
func chunkIndexForBlock(chunks []ChunkMeta, b int) int {
	for i := range chunks {
		c := &chunks[i]
		if b >= c.BlockStart && b < c.BlockStart+c.BlockCount {
			return i
		}
	}
	return 0
}

// buildChunks 依据块体量切 chunk（块为原子单位）。目录 NavAnchor 所在块
// 尽量作为新 chunk 的首块（当前累计已过半时先收块），目录跳转只需加载
// 目标 chunk 即可从其头部附近开始定位；切分只发生在块边界，不破坏
// inline/段落语义。
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
		if len(b.navAnchors) > 0 && i > start && (chars >= chunkCharsTarget/2 || bytes >= chunkBytesTarget/2) {
			flush(i)
		}
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
