package reader

import (
	"archive/zip"
	"bytes"
	"encoding/binary"
	"io"
	"strings"
	"testing"
	unicodeutf16 "unicode/utf16"

	"golang.org/x/text/encoding/simplifiedchinese"
)

type repeatingByteReader byte

func (r repeatingByteReader) Read(p []byte) (int, error) {
	for i := range p {
		p[i] = byte(r)
	}
	return len(p), nil
}

func fakePNG(w, h int) []byte {
	data := make([]byte, 33)
	copy(data, []byte("\x89PNG\r\n\x1a\n"))
	binary.BigEndian.PutUint32(data[8:12], 13)
	copy(data[12:16], "IHDR")
	binary.BigEndian.PutUint32(data[16:20], uint32(w))
	binary.BigEndian.PutUint32(data[20:24], uint32(h))
	return data
}

// buildTestEPUB 构造一个最小但完整的 EPUB3：容器、OPF、nav 目录、一章正文
// （含脚本/危险链接/内嵌图片）与封面。
func buildTestEPUB(t *testing.T) []byte {
	t.Helper()
	buf := &bytes.Buffer{}
	zw := zip.NewWriter(buf)
	write := func(name, content string) {
		w, err := zw.Create(name)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := w.Write([]byte(content)); err != nil {
			t.Fatal(err)
		}
	}
	write("mimetype", "application/epub+zip")
	write("META-INF/container.xml", `<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>`)
	write("OEBPS/content.opf", `<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>测试书</dc:title><dc:creator>作者甲</dc:creator><meta name="cover" content="cover-img"/></metadata><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/><item id="cover-img" href="img/cover.png" media-type="image/png"/><item id="fig" href="img/fig.png" media-type="image/png"/></manifest><spine><itemref idref="ch1"/></spine></package>`)
	write("OEBPS/nav.xhtml", `<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol><li><a href="ch1.xhtml#sec1">第一章</a></li></ol></nav></body></html>`)
	write("OEBPS/ch1.xhtml", `<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="sec1">第一章 开始</h1><p>你好世界</p><script>alert(1)</script><p><a href="javascript:alert(2)">坏链接</a> <img src="img/fig.png" alt="插图"/></p></body></html>`)
	write("OEBPS/img/cover.png", string(fakePNG(300, 400)))
	write("OEBPS/img/fig.png", string(fakePNG(10, 20)))
	if err := zw.Close(); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

func TestParseEPUBPipeline(t *testing.T) {
	fixture := buildTestEPUB(t)
	book, err := Parse("book.epub", bytes.NewReader(fixture), int64(len(fixture)), "/api/files/f1/book/assets", "test-etag")
	if err != nil {
		t.Fatal(err)
	}
	if book.Format != "epub" || book.Title != "测试书" {
		t.Fatalf("format=%q title=%q", book.Format, book.Title)
	}
	if len(book.TOC) != 1 {
		t.Fatalf("toc len=%d: %+v", len(book.TOC), book.TOC)
	}
	entry := book.TOC[0]
	if entry.Label != "第一章" || entry.Path != "OEBPS/ch1.xhtml" || entry.Fragment != "sec1" || entry.Depth != 0 {
		t.Fatalf("toc entry=%+v", entry)
	}
	if len(book.Chapters) != 1 || !strings.Contains(book.Chapters[0].HTML, "你好世界") {
		t.Fatalf("chapters=%+v", book.Chapters)
	}
	htmlOut := book.Chapters[0].HTML
	for _, want := range []string{"你好世界", "第一章 开始", `data-source-path="OEBPS/ch1.xhtml"`, `alt="插图"`} {
		if !strings.Contains(htmlOut, want) {
			t.Fatalf("html missing %q:\n%s", want, htmlOut)
		}
	}
	for _, forbidden := range []string{"<script", "javascript:", "alert"} {
		if strings.Contains(htmlOut, forbidden) {
			t.Fatalf("html must not contain %q:\n%s", forbidden, htmlOut)
		}
	}
	if !strings.Contains(htmlOut, `/api/files/f1/book/assets/0?v=test-etag" width="10" height="20"`) {
		t.Fatalf("img not rewritten with dims and version:\n%s", htmlOut)
	}
	if len(book.Assets) != 1 || book.Assets[0].Width != 10 || book.Assets[0].Height != 20 {
		t.Fatalf("assets=%+v", book.Assets)
	}
	if len(book.Cover) == 0 || book.CoverExt != "png" {
		t.Fatalf("cover ext=%q bytes=%d", book.CoverExt, len(book.Cover))
	}
}

func TestParseTXTTocAndOffsets(t *testing.T) {
	text := "第一章 开始\n正文一行\n第二章 继续\n"
	book, err := Parse("book.txt", bytes.NewReader([]byte(text)), int64(len(text)), "", "")
	if err != nil {
		t.Fatal(err)
	}
	if book.Format != "txt" || book.Text != text {
		t.Fatalf("format=%q text=%q", book.Format, book.Text)
	}
	if len(book.TOC) != 2 {
		t.Fatalf("toc len=%d: %+v", len(book.TOC), book.TOC)
	}
	if book.TOC[0].Label != "第一章 开始" || book.TOC[0].Offset != 0 {
		t.Fatalf("first=%+v", book.TOC[0])
	}
	// UTF-16 偏移 = 前两行的 UTF-16 码元数。
	want := utf16Len("第一章 开始\n正文一行\n")
	if book.TOC[1].Offset != want {
		t.Fatalf("second offset=%d want %d", book.TOC[1].Offset, want)
	}
}

func utf16Len(s string) int64 {
	total := 0
	for _, r := range s {
		total += unicodeutf16.RuneLen(r)
	}
	return int64(total)
}

func TestParseTXTGBKDecode(t *testing.T) {
	text := "第一章 测试\n内容行"
	encoded, err := simplifiedchinese.GBK.NewEncoder().Bytes([]byte(text))
	if err != nil {
		t.Fatal(err)
	}
	book, err := Parse("book.txt", bytes.NewReader(encoded), int64(len(encoded)), "", "")
	if err != nil {
		t.Fatal(err)
	}
	if book.Text != text {
		t.Fatalf("decoded text=%q", book.Text)
	}
	if len(book.TOC) != 1 || book.TOC[0].Label != "第一章 测试" {
		t.Fatalf("toc=%+v", book.TOC)
	}
}

func TestBudgetEnforcesTotalLimit(t *testing.T) {
	b := &budget{}
	if err := b.take(maxDecompressedTotal); err != nil {
		t.Fatalf("exact-limit take rejected: %v", err)
	}
	if err := b.take(1); err == nil {
		t.Fatal("over-limit take must be rejected")
	}
	if err := b.take(0); err != nil {
		t.Fatalf("zero take rejected: %v", err)
	}
}

func TestEPUBRejectsDecompressionBomb(t *testing.T) {
	// 单个高压缩比条目（重复字节）解压后超过总预算：解析必须报错，
	// 而不是静默截断或耗尽内存。
	var buf bytes.Buffer
	zw := zip.NewWriter(&buf)
	write := func(name, content string) {
		w, err := zw.Create(name)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := w.Write([]byte(content)); err != nil {
			t.Fatal(err)
		}
	}
	write("mimetype", "application/epub+zip")
	write("META-INF/container.xml", `<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>`)
	write("OEBPS/content.opf", `<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>炸弹</dc:title></metadata><manifest><item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="ch1"/></spine></package>`)
	bomb, err := zw.Create("OEBPS/ch1.xhtml")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := io.CopyN(bomb, repeatingByteReader('A'), maxDecompressedTotal+1); err != nil {
		t.Fatal(err)
	}
	if err := zw.Close(); err != nil {
		t.Fatal(err)
	}
	if _, err := Parse("book.epub", bytes.NewReader(buf.Bytes()), int64(buf.Len()), "/api/files/f1/book/assets", "etag"); err == nil {
		t.Fatal("decompression bomb must be rejected")
	}
}

func TestCacheEviction(t *testing.T) {
	cache := NewCache(2, 1<<20)
	mk := func(label string) *Book {
		b := &Book{Format: "txt", Text: strings.Repeat(label, 100)}
		b.bytes()
		return b
	}
	cache.Put("a", mk("a"))
	cache.Put("b", mk("b"))
	if cache.Get("a") == nil {
		t.Fatal("recent entry missing")
	}
	cache.Put("c", mk("c")) // 超出 maxBooks，应驱逐最久未用的 b
	if cache.Get("b") != nil {
		t.Fatal("LRU entry should be evicted")
	}
	if cache.Get("a") == nil || cache.Get("c") == nil {
		t.Fatal("recent entries should survive")
	}
}

func TestCacheDoesNotRetainOversizedBook(t *testing.T) {
	cache := NewCache(2, 100)
	cache.Put("large", &Book{Format: "txt", Text: strings.Repeat("x", 101)})
	if cache.Get("large") != nil {
		t.Fatal("oversized book must not be cached")
	}
}

func TestResolvePath(t *testing.T) {
	cases := []struct{ base, rel, want string }{
		{"OEBPS/content.opf", "ch1.xhtml", "OEBPS/ch1.xhtml"},
		{"OEBPS/content.opf", "../img/a.png", "img/a.png"},
		{"OEBPS/ch1.xhtml", "img/fig.png", "OEBPS/img/fig.png"},
		{"OEBPS/ch1.xhtml", "ch1.xhtml#frag", "OEBPS/ch1.xhtml"},
		{"OEBPS/nav.xhtml", "ch1.xhtml?q=1#f", "OEBPS/ch1.xhtml"},
		{"a/b/c.xhtml", "../../d.png", "d.png"},
		{"ch1.xhtml", "/absolute.png", "absolute.png"},
	}
	for _, c := range cases {
		if got := resolvePath(c.base, c.rel); got != c.want {
			t.Fatalf("resolvePath(%q, %q) = %q, want %q", c.base, c.rel, got, c.want)
		}
	}
	if got := fragmentOf("ch1.xhtml#sec%201"); got != "sec 1" {
		t.Fatalf("fragment=%q", got)
	}
}

func TestImageDims(t *testing.T) {
	if w, h, ok := imageDims(fakePNG(300, 400)); !ok || w != 300 || h != 400 {
		t.Fatalf("png dims=%d,%d,%v", w, h, ok)
	}
	gif := append([]byte("GIF89a"), make([]byte, 6)...)
	binary.LittleEndian.PutUint16(gif[6:8], 64)
	binary.LittleEndian.PutUint16(gif[8:10], 32)
	if w, h, ok := imageDims(gif); !ok || w != 64 || h != 32 {
		t.Fatalf("gif dims=%d,%d,%v", w, h, ok)
	}
	webp := make([]byte, 30)
	copy(webp, "RIFF")
	copy(webp[8:12], "WEBP")
	copy(webp[12:16], "VP8X")
	webp[24], webp[25], webp[26] = 99, 0, 0
	webp[27], webp[28], webp[29] = 49, 0, 0
	if w, h, ok := imageDims(webp); !ok || w != 100 || h != 50 {
		t.Fatalf("webp dims=%d,%d,%v", w, h, ok)
	}
	if _, _, ok := imageDims([]byte("not an image")); ok {
		t.Fatal("garbage detected as image")
	}
}

func TestSanitizeHref(t *testing.T) {
	if sanitizeHref("javascript:alert(1)") != "" || sanitizeHref("data:text/html,x") != "" || sanitizeHref("vbscript:x") != "" {
		t.Fatal("dangerous hrefs must be stripped")
	}
	if sanitizeHref("https://example.com") != "https://example.com" {
		t.Fatal("safe href stripped")
	}
}
