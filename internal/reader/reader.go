// Package reader 把网盘里的 EPUB/TXT 文件解析成可供前端阅读器渲染的内容：
// EPUB 在服务端完成解包、目录提取、封面抽取与正文白名单清洗（等价于
// VesperGlow/reader 的 Rust 解析管线），TXT 做编码探测（UTF-8/GBK）与
// 章节标题识别。解析结果按文件内容哈希（清单键）缓存，天然不可变。
package reader

import (
	"fmt"
	"io"
	"path"
	"strings"
	"sync"
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
