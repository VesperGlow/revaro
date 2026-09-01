// Package reader 把网盘里的 EPUB/TXT 文件解析成可供前端阅读器渲染的内容：
// EPUB 在服务端完成解包、目录提取、封面抽取与正文白名单清洗（等价于
// VesperGlow/reader 的 Rust 解析管线），TXT 做编码探测（UTF-8/GBK）与
// 章节标题识别。解析结果按文件内容哈希（清单键）缓存，天然不可变。
package reader

import (
	"archive/zip"
	"bytes"
	"fmt"
	stdhtml "html"
	"net/url"
	"regexp"
	"strconv"
	"strings"

	"golang.org/x/net/html"
)

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
