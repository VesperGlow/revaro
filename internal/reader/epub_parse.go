// Package reader 把网盘里的 EPUB/TXT 文件解析成可供前端阅读器渲染的内容：
// EPUB 在服务端完成解包、目录提取、封面抽取与正文白名单清洗（等价于
// VesperGlow/reader 的 Rust 解析管线），TXT 做编码探测（UTF-8/GBK）与
// 章节标题识别。解析结果按文件内容哈希（清单键）缓存，天然不可变。
package reader

import (
	"archive/zip"
	"bytes"
	"encoding/xml"
	"fmt"
	"io"
	"strings"

	"golang.org/x/net/html"
)

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
	norm := NormalizePath(name)
	for _, f := range zr.File {
		if NormalizePath(f.Name) == norm {
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
	norm := NormalizePath(name)
	for _, f := range zr.File {
		if NormalizePath(f.Name) != norm {
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
	return NormalizePath(doc.Rootfiles[0].FullPath), nil
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
