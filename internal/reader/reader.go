// Package reader 把网盘里的 EPUB/TXT 文件解析成可供前端阅读器渲染的内容：
// EPUB 在服务端完成解包、目录提取、封面抽取与正文白名单清洗（等价于
// VesperGlow/reader 的 Rust 解析管线），TXT 做编码探测（UTF-8/GBK）与
// 章节标题识别。解析结果按文件内容哈希（清单键）缓存，天然不可变。
package reader

import (
	"archive/zip"
	"bytes"
	"container/list"
	"encoding/binary"
	"encoding/xml"
	"fmt"
	stdhtml "html"
	"io"
	"net/url"
	"path"
	"regexp"
	"strconv"
	"strings"
	"sync"
	unicodeutf16 "unicode/utf16"
	"unicode/utf8"

	"golang.org/x/net/html"
	"golang.org/x/text/encoding/simplifiedchinese"
)

const (
	// MaxTXT caps the decoded text size the reader handles.
	MaxTXT = 16 << 20
	// MaxEPUB caps the source EPUB size.
	MaxEPUB = 128 << 20
	// maxDecompressedTotal caps the total bytes the EPUB parser will
	// decompress across all zip entries. The source archive is limited to
	// 128 MiB compressed, but a pathological archive (zip bomb) can expand
	// to many times that; without this cap parsing would exhaust memory.
	maxDecompressedTotal = 256 << 20
	maxDecompressedEntry = 64 << 20
	maxRenderedHTML      = 64 << 20
	maxArchiveEntries    = 10000
)

// budget tracks the total decompressed bytes consumed so far and rejects
// archives that expand beyond maxDecompressedTotal.
type budget struct{ used int64 }

func (b *budget) take(n int64) error {
	if n > maxDecompressedTotal-b.used {
		return fmt.Errorf("EPUB 解压后内容超过 %d MiB 上限", maxDecompressedTotal>>20)
	}
	b.used += n
	return nil
}

type TocEntry struct {
	Label    string `json:"label"`
	Path     string `json:"path,omitempty"`
	Fragment string `json:"fragment,omitempty"`
	Offset   int64  `json:"offset,omitempty"`
	Depth    int    `json:"depth"`
}

type Asset struct {
	Data        []byte
	ContentType string
	Width       int
	Height      int
}

// Chapter 是一章清洗后的正文 HTML，客户端按章分段渲染（小合成层翻页）。
type Chapter struct {
	HTML string `json:"html"`
}

// Book is the parsed, immutable content model of one file.
type Book struct {
	Format   string     // "epub" | "txt"
	Title    string     // epub 书名；txt 为文件名
	Chapters []Chapter  // epub 逐章正文
	Text     string     // txt 全文（UTF-8）
	TOC      []TocEntry // epub 目录或 txt 章节（UTF-16 偏移）
	Cover    []byte
	CoverExt string
	Assets   []Asset // 按索引排列的 EPUB 内嵌图片
	size     int64
}

func (b *Book) bytes() int64 {
	if b.size > 0 {
		return b.size
	}
	n := int64(len(b.Text) + len(b.Cover))
	for _, chapter := range b.Chapters {
		n += int64(len(chapter.HTML))
	}
	for _, a := range b.Assets {
		n += int64(len(a.Data))
	}
	b.size = n
	return n
}

// Parse 按扩展名解析一个文件流。assetBase 是内嵌图片 URL 的前缀；
// assetVersion 附加为查询参数（如内容哈希），使资产 URL 与内容一一对应，
// 可安全地使用 immutable 长缓存。
func Parse(name string, rs io.ReadSeeker, size int64, assetBase, assetVersion string) (*Book, error) {
	switch strings.ToLower(path.Ext(name)) {
	case ".epub":
		if size > MaxEPUB {
			return nil, fmt.Errorf("EPUB 超过 %d MiB 限制", MaxEPUB>>20)
		}
		return parseEPUB(rs, size, assetBase, assetVersion)
	default:
		if size > MaxTXT {
			return nil, fmt.Errorf("文本文件超过 %d MiB 限制，请下载后离线阅读", MaxTXT>>20)
		}
		return parseTXT(rs)
	}
}

// readSeekerAt 让 io.ReadSeeker 满足 zip.Reader 需要的 ReaderAt。
type readSeekerAt struct {
	mu sync.Mutex
	rs io.ReadSeeker
}

func (a *readSeekerAt) ReadAt(p []byte, off int64) (int, error) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if _, err := a.rs.Seek(off, io.SeekStart); err != nil {
		return 0, err
	}
	n, err := io.ReadFull(a.rs, p)
	if err == io.ErrUnexpectedEOF {
		return n, io.EOF
	}
	return n, err
}

// ---- EPUB ----

type opfMeta struct {
	Name     string `xml:"name,attr"`
	Content  string `xml:"content,attr"`
	Property string `xml:"property,attr"`
	Text     string `xml:",chardata"`
}

type opfPackage struct {
	Metadata struct {
		Title   string    `xml:"title"`
		Creator string    `xml:"creator"`
		Meta    []opfMeta `xml:"meta"`
	} `xml:"metadata"`
	Manifest struct {
		Items []struct {
			ID         string `xml:"id,attr"`
			Href       string `xml:"href,attr"`
			MediaType  string `xml:"media-type,attr"`
			Properties string `xml:"properties,attr"`
		} `xml:"item"`
	} `xml:"manifest"`
	Spine struct {
		Toc      string `xml:"toc,attr"`
		Itemrefs []struct {
			IDRef string `xml:"idref,attr"`
		} `xml:"itemref"`
	} `xml:"spine"`
}

type manifestItem struct {
	path, mediaType, properties string
}

type renderBudget struct {
	used, max int64
	overflow  bool
}

type limitedBuilder struct {
	b      strings.Builder
	budget *renderBudget
}

func (b *limitedBuilder) Write(p []byte) (int, error) {
	if b.budget.overflow || int64(len(p)) > b.budget.max-b.budget.used {
		b.budget.overflow = true
		return 0, io.ErrShortWrite
	}
	n, err := b.b.Write(p)
	b.budget.used += int64(n)
	return n, err
}

func (b *limitedBuilder) WriteString(value string) (int, error) {
	return b.Write([]byte(value))
}

func (b *limitedBuilder) WriteByte(value byte) error {
	_, err := b.Write([]byte{value})
	return err
}

func (b *limitedBuilder) String() string { return b.b.String() }
func (b *limitedBuilder) Reset()         { b.b.Reset() }

type epubBuilder struct {
	zip          *zip.Reader
	manifest     map[string]manifestItem
	spine        []string
	assets       []Asset
	assetIndex   map[string]int
	out          limitedBuilder
	pending      []string
	chapter      string
	assetBase    string
	assetVersion string
	budget       *budget
}

func parseEPUB(rs io.ReadSeeker, size int64, assetBase, assetVersion string) (*Book, error) {
	zr, err := zip.NewReader(&readSeekerAt{rs: rs}, size)
	if err != nil {
		return nil, fmt.Errorf("EPUB 不是有效的 zip: %w", err)
	}
	if len(zr.File) > maxArchiveEntries {
		return nil, fmt.Errorf("EPUB 条目数量超过 %d 上限", maxArchiveEntries)
	}
	budget := &budget{}
	container, err := zipText(zr, "META-INF/container.xml", budget)
	if err != nil {
		return nil, fmt.Errorf("读取 EPUB 容器失败: %w", err)
	}
	opfPath, err := opfPathFromContainer(container)
	if err != nil {
		return nil, err
	}
	opfXML, err := zipText(zr, opfPath, budget)
	if err != nil {
		return nil, fmt.Errorf("读取 OPF 失败: %w", err)
	}
	var pkg opfPackage
	if err := xml.Unmarshal([]byte(opfXML), &pkg); err != nil {
		return nil, fmt.Errorf("解析 OPF 失败: %w", err)
	}

	rendered := &renderBudget{max: maxRenderedHTML}
	b := &epubBuilder{
		zip:          zr,
		manifest:     map[string]manifestItem{},
		assetIndex:   map[string]int{},
		assetBase:    strings.TrimRight(assetBase, "/"),
		assetVersion: assetVersion,
		budget:       budget,
		out:          limitedBuilder{budget: rendered},
	}
	if len(pkg.Manifest.Items) > maxArchiveEntries || len(pkg.Spine.Itemrefs) > maxArchiveEntries {
		return nil, fmt.Errorf("EPUB 清单或书脊条目过多")
	}
	for _, it := range pkg.Manifest.Items {
		b.manifest[it.ID] = manifestItem{path: resolvePath(opfPath, it.Href), mediaType: it.MediaType, properties: it.Properties}
	}
	for _, ref := range pkg.Spine.Itemrefs {
		b.spine = append(b.spine, ref.IDRef)
	}

	book := &Book{Format: "epub", Title: strings.TrimSpace(pkg.Metadata.Title)}
	if book.Title == "" {
		book.Title = "未命名书籍"
	}
	book.TOC = extractToc(zr, &pkg, b.manifest, opfPath, budget)
	book.Cover, book.CoverExt = extractCover(zr, &pkg, b.manifest, budget)

	for _, idref := range b.spine {
		item, ok := b.manifest[idref]
		if !ok || !isHTMLMedia(item.mediaType) {
			continue
		}
		chapter, err := zipText(zr, item.path, budget)
		if err != nil {
			continue
		}
		b.chapter = item.path
		b.pending = b.pending[:0]
		b.renderChapter([]byte(chapter))
		if rendered.overflow {
			return nil, fmt.Errorf("EPUB 渲染正文超过 %d MiB 上限", maxRenderedHTML>>20)
		}
		book.Chapters = append(book.Chapters, Chapter{HTML: b.out.String()})
		b.out.Reset()
	}
	if len(book.Chapters) == 0 {
		return nil, fmt.Errorf("EPUB 中没有可阅读内容")
	}
	book.Assets = b.assets
	return book, nil
}

func zipText(zr *zip.Reader, name string, b *budget) (string, error) {
	norm := normalizePath(name)
	for _, f := range zr.File {
		if normalizePath(f.Name) == norm {
			rc, err := f.Open()
			if err != nil {
				return "", err
			}
			defer rc.Close()
			data, err := readZipEntry(rc, b)
			if err != nil {
				return "", err
			}
			return string(data), nil
		}
	}
	return "", fmt.Errorf("EPUB 缺少文件：%s", name)
}

func zipBytes(zr *zip.Reader, name string, b *budget) ([]byte, error) {
	norm := normalizePath(name)
	for _, f := range zr.File {
		if normalizePath(f.Name) != norm {
			continue
		}
		rc, err := f.Open()
		if err != nil {
			return nil, err
		}
		defer rc.Close()
		return readZipEntry(rc, b)
	}
	return nil, fmt.Errorf("EPUB 缺少文件：%s", name)
}

// readZipEntry reads one zip entry, enforcing both the per-entry and the
// cumulative decompression budget. Exceeding either is an error rather than
// a silent truncation.
func readZipEntry(rc io.Reader, b *budget) ([]byte, error) {
	data, err := io.ReadAll(io.LimitReader(rc, maxDecompressedEntry+1))
	if err != nil {
		return nil, err
	}
	if int64(len(data)) > maxDecompressedEntry {
		return nil, fmt.Errorf("EPUB 条目超过 %d MiB 上限", maxDecompressedEntry>>20)
	}
	if err := b.take(int64(len(data))); err != nil {
		return nil, err
	}
	return data, nil
}

func opfPathFromContainer(xmlText string) (string, error) {
	var doc struct {
		Rootfiles []struct {
			FullPath string `xml:"full-path,attr"`
		} `xml:"rootfiles>rootfile"`
	}
	if err := xml.Unmarshal([]byte(xmlText), &doc); err != nil || len(doc.Rootfiles) == 0 || doc.Rootfiles[0].FullPath == "" {
		return "", fmt.Errorf("EPUB 缺少 container rootfile")
	}
	return normalizePath(doc.Rootfiles[0].FullPath), nil
}

func isHTMLMedia(mediaType string) bool {
	m := strings.ToLower(mediaType)
	return strings.Contains(m, "xhtml") || strings.Contains(m, "html") || strings.Contains(m, "xml")
}

// ---- 目录 ----

func extractToc(zr *zip.Reader, pkg *opfPackage, manifest map[string]manifestItem, opfPath string, b *budget) []TocEntry {
	// EPUB3 nav 文档
	for _, item := range manifest {
		if !strings.Contains(" "+item.properties+" ", " nav ") {
			continue
		}
		if navHTML, err := zipText(zr, item.path, b); err == nil {
			if entries := parseNav([]byte(navHTML), item.path); len(entries) > 0 {
				return entries
			}
		}
	}
	// 回退 NCX
	ncxItem, ok := manifest[pkg.Spine.Toc]
	if !ok {
		for _, item := range manifest {
			if strings.Contains(strings.ToLower(item.mediaType), "ncx") {
				ncxItem, ok = item, true
				break
			}
		}
	}
	if !ok {
		return []TocEntry{}
	}
	ncxXML, err := zipText(zr, ncxItem.path, b)
	if err != nil {
		return []TocEntry{}
	}
	return parseNCX([]byte(ncxXML), ncxItem.path)
}

func parseNav(data []byte, navPath string) []TocEntry {
	doc, err := html.Parse(bytes.NewReader(data))
	if err != nil {
		return nil
	}
	nav := findTocNav(doc)
	if nav == nil {
		return nil
	}
	var entries []TocEntry
	collectAnchors(nav, 0, navPath, &entries)
	return entries
}

func findTocNav(root *html.Node) *html.Node {
	var first, tocNav *html.Node
	var walk func(n *html.Node)
	walk = func(n *html.Node) {
		if tocNav != nil {
			return
		}
		if n.Type == html.ElementNode && n.Data == "nav" {
			if first == nil {
				first = n
			}
			for _, attr := range n.Attr {
				if attr.Val == "toc" {
					tocNav = n
					return
				}
			}
		}
		for c := n.FirstChild; c != nil; c = c.NextSibling {
			walk(c)
		}
	}
	walk(root)
	if tocNav != nil {
		return tocNav
	}
	return first
}

func collectAnchors(node *html.Node, listDepth int, navPath string, out *[]TocEntry) {
	if node.Type == html.ElementNode && (node.Data == "ol" || node.Data == "ul") {
		listDepth++
	}
	for child := node.FirstChild; child != nil; child = child.NextSibling {
		if child.Type == html.ElementNode && child.Data == "a" {
			href, label := "", ""
			for _, attr := range child.Attr {
				if attr.Key == "href" {
					href = attr.Val
				}
			}
			label = strings.TrimSpace(textContent(child))
			if label != "" {
				*out = append(*out, TocEntry{
					Label:    label,
					Path:     resolvePath(navPath, href),
					Fragment: fragmentOf(href),
					Depth:    max(0, listDepth-1),
				})
			}
			continue
		}
		collectAnchors(child, listDepth, navPath, out)
	}
}

type ncxNavPoint struct {
	Label   ncxLabel       `xml:"navLabel"`
	Content ncxContent     `xml:"content"`
	Points  []*ncxNavPoint `xml:"navPoint"`
}

type ncxLabel struct {
	Text string `xml:"text"`
}

type ncxContent struct {
	Src string `xml:"src,attr"`
}

func parseNCX(data []byte, ncxPath string) []TocEntry {
	var doc struct {
		Points []*ncxNavPoint `xml:"navMap>navPoint"`
	}
	if err := xml.Unmarshal(data, &doc); err != nil {
		return nil
	}
	var entries []TocEntry
	var walk func(points []*ncxNavPoint, depth int)
	walk = func(points []*ncxNavPoint, depth int) {
		for _, p := range points {
			label := strings.TrimSpace(p.Label.Text)
			if label == "" {
				label = "未命名章节"
			}
			entries = append(entries, TocEntry{Label: label, Path: resolvePath(ncxPath, p.Content.Src), Fragment: fragmentOf(p.Content.Src), Depth: depth})
			walk(p.Points, depth+1)
		}
	}
	walk(doc.Points, 0)
	return entries
}

// ---- 章节清洗渲染（白名单 emit，端口自 Rust 版 epub.rs） ----

func (b *epubBuilder) renderChapter(chapter []byte) {
	doc, err := html.Parse(bytes.NewReader(chapter))
	if err != nil {
		return
	}
	body := findElement(doc, "body")
	if body == nil {
		return
	}
	b.walk(body)
}

func findElement(node *html.Node, tag string) *html.Node {
	if node.Type == html.ElementNode && node.Data == tag {
		return node
	}
	for child := node.FirstChild; child != nil; child = child.NextSibling {
		if n := findElement(child, tag); n != nil {
			return n
		}
	}
	return nil
}

func textContent(node *html.Node) string {
	var sb strings.Builder
	var collect func(n *html.Node)
	collect = func(n *html.Node) {
		if n.Type == html.TextNode {
			sb.WriteString(n.Data)
		}
		for c := n.FirstChild; c != nil; c = c.NextSibling {
			collect(c)
		}
	}
	collect(node)
	return sb.String()
}

func isBlockTag(tag string) bool {
	switch tag {
	case "p", "h1", "h2", "h3", "h4", "h5", "h6", "li", "blockquote", "pre", "figure", "img", "svg", "table", "hr":
		return true
	}
	return false
}

func isDisallowedTag(tag string) bool {
	switch tag {
	case "script", "style", "iframe", "object", "embed", "form", "input", "button", "meta", "base", "link", "head", "title", "noscript":
		return true
	}
	return false
}

var voidTags = map[string]bool{
	"area": true, "base": true, "br": true, "col": true, "embed": true,
	"hr": true, "img": true, "input": true, "link": true, "meta": true,
	"source": true, "track": true, "wbr": true,
}

func (b *epubBuilder) hasBlockDescendant(node *html.Node) bool {
	found := false
	var scan func(n *html.Node)
	scan = func(n *html.Node) {
		if found {
			return
		}
		for c := n.FirstChild; c != nil; c = c.NextSibling {
			if c.Type == html.ElementNode && isBlockTag(c.Data) {
				found = true
				return
			}
			scan(c)
		}
	}
	scan(node)
	return found
}

func (b *epubBuilder) hasMedia(node *html.Node, includeImage bool) bool {
	found := false
	var scan func(n *html.Node)
	scan = func(n *html.Node) {
		if found {
			return
		}
		for c := n.FirstChild; c != nil; c = c.NextSibling {
			if c.Type != html.ElementNode {
				continue
			}
			tag := c.Data
			if tag == "img" || tag == "svg" || (includeImage && tag == "image") {
				found = true
				return
			}
			scan(c)
		}
	}
	scan(node)
	return found
}

func (b *epubBuilder) walk(parent *html.Node) {
	for child := parent.FirstChild; child != nil; child = child.NextSibling {
		switch child.Type {
		case html.TextNode:
			if strings.TrimSpace(child.Data) != "" {
				b.out.WriteString("<p")
				b.writeBlockExtra()
				b.out.WriteString(">")
				b.out.WriteString(stdhtml.EscapeString(child.Data))
				b.out.WriteString("</p>")
			}
		case html.ElementNode:
			tag := child.Data
			if isDisallowedTag(tag) {
				continue
			}
			childID := attrKey(child, "id")
			if isBlockTag(tag) {
				keep := tag == "hr" || tag == "img" || tag == "svg" || strings.TrimSpace(textContent(child)) != "" || b.hasMedia(child, true)
				if keep {
					b.emitElement(child, true)
				} else if childID != "" {
					b.pending = append(b.pending, childID)
				}
			} else if b.hasBlockDescendant(child) {
				if childID != "" {
					b.pending = append(b.pending, childID)
				}
				b.walk(child)
			} else if strings.TrimSpace(textContent(child)) != "" || b.hasMedia(child, false) {
				b.emitElement(child, true)
			} else if childID != "" {
				b.pending = append(b.pending, childID)
			}
		}
	}
}

// writeBlockExtra 注入 data-source-path 与顺延的锚点 id。
func (b *epubBuilder) writeBlockExtra() {
	fmt.Fprintf(&b.out, " data-source-path=%q", b.chapter)
	if len(b.pending) > 0 {
		fmt.Fprintf(&b.out, " data-frag-ids=%q", strings.Join(b.pending, " "))
		b.pending = b.pending[:0]
	}
}

func (b *epubBuilder) emitElement(node *html.Node, withExtra bool) {
	tag := node.Data
	if isDisallowedTag(tag) {
		return
	}
	if tag == "svg" {
		if b.emitSvgAsImage(node, withExtra) {
			return
		}
	}
	if tag == "img" {
		b.emitImg(node, withExtra)
		return
	}
	if tag == "image" {
		if href := attrKey(node, "href"); href != "" {
			b.writeImg(href, attrKey(node, "alt"), withExtra)
		}
		return
	}
	b.out.WriteByte('<')
	b.out.WriteString(tag)
	for _, attr := range node.Attr {
		key := attr.Key
		if key == "style" || key == "align" || strings.HasPrefix(key, "on") {
			continue
		}
		switch key {
		case "src", "srcset":
			continue
		case "href":
			if clean := sanitizeHref(attr.Val); clean != "" {
				b.out.WriteByte(' ')
				b.out.WriteString(key)
				b.out.WriteString(`="`)
				b.out.WriteString(stdhtml.EscapeString(clean))
				b.out.WriteByte('"')
			}
		default:
			b.out.WriteByte(' ')
			b.out.WriteString(key)
			b.out.WriteString(`="`)
			b.out.WriteString(stdhtml.EscapeString(attr.Val))
			b.out.WriteByte('"')
		}
	}
	if withExtra {
		b.writeBlockExtra()
	}
	if voidTags[tag] {
		b.out.WriteByte('>')
		return
	}
	b.out.WriteByte('>')
	for c := node.FirstChild; c != nil; c = c.NextSibling {
		switch c.Type {
		case html.ElementNode:
			b.emitElement(c, false)
		case html.TextNode:
			b.out.WriteString(stdhtml.EscapeString(c.Data))
		}
	}
	b.out.WriteString("</")
	b.out.WriteString(tag)
	b.out.WriteByte('>')
}

func (b *epubBuilder) emitImg(node *html.Node, withExtra bool) {
	src := attrKey(node, "src")
	if src == "" {
		return
	}
	b.writeImg(src, attrKey(node, "alt"), withExtra)
}

// writeImg 输出 <img>：内嵌图落盘到内存资产表并改写为 /assets 链接。
func (b *epubBuilder) writeImg(src, alt string, withExtra bool) {
	if isExternalURL(src) {
		return
	}
	zipPath := resolvePath(b.chapter, src)
	if zipPath == "" {
		return
	}
	ext := extOf(zipPath)
	contentType := AssetContentType(ext)
	if contentType == "application/octet-stream" || contentType == "image/svg+xml" {
		return
	}
	idx, ok := b.assetIndex[zipPath]
	if !ok {
		data, err := zipBytes(b.zip, zipPath, b.budget)
		if err != nil {
			return
		}
		idx = len(b.assets)
		w, h, _ := imageDims(data)
		b.assets = append(b.assets, Asset{Data: data, ContentType: contentType, Width: w, Height: h})
		b.assetIndex[zipPath] = idx
	}
	b.out.WriteString(`<img src="`)
	b.out.WriteString(b.assetBase)
	b.out.WriteByte('/')
	b.out.WriteString(strconv.Itoa(idx))
	if b.assetVersion != "" {
		b.out.WriteString("?v=")
		b.out.WriteString(url.QueryEscape(b.assetVersion))
	}
	b.out.WriteByte('"')
	if a := b.assets[idx]; a.Width > 0 && a.Height > 0 {
		fmt.Fprintf(&b.out, ` width="%d" height="%d"`, a.Width, a.Height)
	}
	if alt != "" {
		b.out.WriteString(` alt="`)
		b.out.WriteString(stdhtml.EscapeString(alt))
		b.out.WriteByte('"')
	}
	if withExtra {
		b.writeBlockExtra()
	}
	b.out.WriteByte('>')
}

// emitSvgAsImage：只裹一张 <image> 的 SVG（Calibre 整页封面常见）拆成 <img>。
func (b *epubBuilder) emitSvgAsImage(node *html.Node, withExtra bool) bool {
	var images []*html.Node
	var collect func(n *html.Node)
	collect = func(n *html.Node) {
		if n.Type == html.ElementNode && n.Data == "image" {
			images = append(images, n)
		}
		for c := n.FirstChild; c != nil; c = c.NextSibling {
			collect(c)
		}
	}
	collect(node)
	if len(images) != 1 {
		return false
	}
	img := images[0]
	href := attrKey(img, "href")
	if href == "" {
		return false
	}
	alt := attrKey(node, "aria-label")
	if alt == "" {
		alt = attrKey(img, "alt")
	}
	b.writeImg(href, alt, withExtra)
	return true
}

func attrKey(node *html.Node, key string) string {
	for _, attr := range node.Attr {
		if attr.Key == key {
			return attr.Val
		}
	}
	return ""
}

func sanitizeHref(value string) string {
	lower := strings.ToLower(strings.TrimLeft(value, " \t\r\n"))
	if strings.HasPrefix(lower, "javascript:") || strings.HasPrefix(lower, "vbscript:") || strings.HasPrefix(lower, "data:") {
		return ""
	}
	return value
}

func isExternalURL(value string) bool {
	v := strings.ToLower(strings.TrimLeft(value, " \t\r\n"))
	return strings.HasPrefix(v, "data:") || strings.HasPrefix(v, "blob:") ||
		strings.HasPrefix(v, "http:") || strings.HasPrefix(v, "https:")
}

// ---- 封面 ----

func extractCover(zr *zip.Reader, pkg *opfPackage, manifest map[string]manifestItem, b *budget) ([]byte, string) {
	cover := coverItem(pkg, manifest)
	if cover == nil {
		return nil, ""
	}
	data, err := zipBytes(zr, cover.path, b)
	if err != nil {
		return nil, ""
	}
	return data, extOf(cover.path)
}

func coverItem(pkg *opfPackage, manifest map[string]manifestItem) *manifestItem {
	// 1) <meta name="cover" content="<id>">
	for _, meta := range pkg.Metadata.Meta {
		if meta.Name == "cover" {
			if item, ok := manifest[meta.Content]; ok {
				return &item
			}
		}
	}
	// 2) properties 含 cover-image
	for _, item := range manifest {
		if strings.Contains(" "+item.properties+" ", " cover-image ") {
			it := item
			return &it
		}
	}
	// 3) 文件名启发式
	re := regexp.MustCompile(`(?i)(^|[/_.\-])cover([/_.\-]|$)`)
	for _, item := range manifest {
		if strings.HasPrefix(strings.ToLower(item.mediaType), "image/") && re.MatchString(item.path) {
			it := item
			return &it
		}
	}
	return nil
}

// ---- TXT ----

var txtChapterRe = regexp.MustCompile(`(?im)^[ \t　]{0,4}(第[零〇一二三四五六七八九十百千万两\d]+[章节卷部回篇][^\n]{0,40}|(?:chapter|part)[ \t]+[ivxlcdmIVXLCDM\d]+[^\n]{0,40})[ \t]*$`)

func parseTXT(rs io.ReadSeeker) (*Book, error) {
	data, err := io.ReadAll(io.LimitReader(rs, MaxTXT+1))
	if err != nil {
		return nil, err
	}
	if len(data) > MaxTXT {
		return nil, fmt.Errorf("文本文件超过 %d MiB 限制，请下载后离线阅读", MaxTXT>>20)
	}
	text := ""
	if utf8.Valid(data) {
		text = string(data)
	} else {
		decoded, err := simplifiedchinese.GBK.NewDecoder().Bytes(data)
		if err != nil {
			return nil, fmt.Errorf("文本编码无法识别")
		}
		text = string(decoded)
	}
	book := &Book{Format: "txt", Text: text, TOC: extractTxtToc(text)}
	return book, nil
}

// extractTxtToc 识别“第…章”式标题；偏移量按 UTF-16 码元计（与前端
// text.slice 语义一致），最多 500 条。
func extractTxtToc(text string) []TocEntry {
	marks := txtChapterRe.FindAllStringSubmatchIndex(text, 500)
	if len(marks) == 0 {
		return []TocEntry{}
	}
	type mark struct{ byteOff, labelOff, labelLen int }
	marksInOrder := make([]mark, 0, len(marks))
	for _, m := range marks {
		if len(m) < 4 {
			continue
		}
		marksInOrder = append(marksInOrder, mark{byteOff: m[0], labelOff: m[2], labelLen: m[3] - m[2]})
	}
	entries := make([]TocEntry, 0, len(marksInOrder))
	mi := 0
	utf16Offset := 0
	for bi, ch := range text {
		for mi < len(marksInOrder) && marksInOrder[mi].byteOff == bi {
			m := marksInOrder[mi]
			label := strings.TrimSpace(text[m.labelOff : m.labelOff+m.labelLen])
			entries = append(entries, TocEntry{Label: label, Offset: int64(utf16Offset)})
			mi++
		}
		utf16Offset += unicodeutf16.RuneLen(ch)
	}
	for mi < len(marksInOrder) {
		m := marksInOrder[mi]
		label := strings.TrimSpace(text[m.labelOff : m.labelOff+m.labelLen])
		entries = append(entries, TocEntry{Label: label, Offset: int64(utf16Offset)})
		mi++
	}
	return entries
}

// ---- 工具函数 ----

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

func resolvePath(baseFile, relative string) string {
	head := relative
	if i := strings.IndexAny(head, "#?"); i >= 0 {
		head = head[:i]
	}
	clean := decodePath(head)
	if clean == "" {
		return normalizePath(baseFile)
	}
	var parts []string
	if strings.HasPrefix(clean, "/") {
		parts = nil
	} else {
		base := normalizePath(baseFile)
		if base != "" {
			segs := strings.Split(base, "/")
			parts = append(parts, segs[:len(segs)-1]...)
		}
	}
	parts = append(parts, strings.Split(clean, "/")...)
	return normalizePath(strings.Join(parts, "/"))
}

func decodePath(p string) string {
	if decoded, err := url.PathUnescape(p); err == nil {
		return decoded
	}
	return p
}

func fragmentOf(href string) string {
	if i := strings.IndexByte(href, '#'); i >= 0 {
		return decodePath(href[i+1:])
	}
	return ""
}

func extOf(p string) string {
	ext := strings.ToLower(path.Ext(p))
	ext = strings.TrimPrefix(ext, ".")
	if ext != "" && len(ext) <= 5 {
		ok := true
		for _, r := range ext {
			if (r < 'a' || r > 'z') && (r < '0' || r > '9') {
				ok = false
				break
			}
		}
		if ok {
			return ext
		}
	}
	return "img"
}

func AssetContentType(ext string) string {
	switch ext {
	case "jpg", "jpeg":
		return "image/jpeg"
	case "png":
		return "image/png"
	case "gif":
		return "image/gif"
	case "webp":
		return "image/webp"
	case "svg":
		return "image/svg+xml"
	case "avif":
		return "image/avif"
	case "bmp":
		return "image/bmp"
	default:
		return "application/octet-stream"
	}
}

// imageDims 嗅探常见图片格式的宽高（无第三方依赖）。
func imageDims(data []byte) (int, int, bool) {
	switch {
	case bytes.HasPrefix(data, []byte("\x89PNG\r\n\x1a\n")) && len(data) >= 24:
		return int(binary.BigEndian.Uint32(data[16:20])), int(binary.BigEndian.Uint32(data[20:24])), true
	case len(data) >= 10 && (bytes.HasPrefix(data, []byte("GIF87a")) || bytes.HasPrefix(data, []byte("GIF89a"))):
		return int(binary.LittleEndian.Uint16(data[6:8])), int(binary.LittleEndian.Uint16(data[8:10])), true
	case len(data) >= 4 && data[0] == 0xFF && data[1] == 0xD8:
		for i := 2; i+9 < len(data); {
			if data[i] != 0xFF {
				i++
				continue
			}
			marker := data[i+1]
			if marker >= 0xC0 && marker <= 0xCF && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
				h := int(binary.BigEndian.Uint16(data[i+5 : i+7]))
				w := int(binary.BigEndian.Uint16(data[i+7 : i+9]))
				return w, h, true
			}
			segLen := int(binary.BigEndian.Uint16(data[i+2 : i+4]))
			i += 2 + segLen
		}
	case len(data) >= 30 && bytes.HasPrefix(data, []byte("RIFF")) && string(data[8:12]) == "WEBP":
		switch string(data[12:16]) {
		case "VP8X":
			w := int(data[24]) | int(data[25])<<8 | int(data[26])<<16
			h := int(data[27]) | int(data[28])<<8 | int(data[29])<<16
			return w + 1, h + 1, true
		case "VP8 ":
			w := int(binary.LittleEndian.Uint16(data[26:28]) & 0x3FFF)
			h := int(binary.LittleEndian.Uint16(data[28:30]) & 0x3FFF)
			return w, h, true
		case "VP8L":
			bits := binary.LittleEndian.Uint32(data[21:25])
			return int(bits&0x3FFF) + 1, int((bits>>14)&0x3FFF) + 1, true
		}
	}
	return 0, 0, false
}

// ---- 运行时缓存（keep-alive，按内容哈希键 LRU） ----

type Cache struct {
	mu       sync.Mutex
	entries  map[string]*list.Element
	order    *list.List
	maxBooks int
	maxBytes int64
}

var DefaultCache = NewCache(3, 96<<20)

func NewCache(maxBooks int, maxBytes int64) *Cache {
	return &Cache{entries: map[string]*list.Element{}, order: list.New(), maxBooks: maxBooks, maxBytes: maxBytes}
}

func (c *Cache) Get(key string) *Book {
	c.mu.Lock()
	defer c.mu.Unlock()
	if el, ok := c.entries[key]; ok {
		c.order.MoveToBack(el)
		return el.Value.(*Book)
	}
	return nil
}

func (c *Cache) Put(key string, b *Book) {
	if b == nil {
		return
	}
	if b.bytes() > c.maxBytes {
		return
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if el, ok := c.entries[key]; ok {
		el.Value = b
		c.order.MoveToBack(el)
	} else {
		c.entries[key] = c.order.PushBack(b)
	}
	var total int64
	for el := c.order.Back(); el != nil; el = el.Prev() {
		total += el.Value.(*Book).bytes()
	}
	for c.order.Len() > c.maxBooks || total > c.maxBytes {
		el := c.order.Front()
		book := el.Value.(*Book)
		total -= book.bytes()
		c.order.Remove(el)
		for k, e := range c.entries {
			if e == el {
				delete(c.entries, k)
				break
			}
		}
	}
}
