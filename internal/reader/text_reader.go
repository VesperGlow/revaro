// Package reader 把网盘里的 EPUB/TXT 文件解析成可供前端阅读器渲染的内容：
// EPUB 在服务端完成解包、目录提取、封面抽取与正文白名单清洗（等价于
// VesperGlow/reader 的 Rust 解析管线），TXT 做编码探测（UTF-8/GBK）与
// 章节标题识别。解析结果按文件内容哈希（清单键）缓存，天然不可变。
package reader

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"io"
	"net/url"
	"path"
	"regexp"
	"strings"
	unicodeutf16 "unicode/utf16"
	"unicode/utf8"

	"golang.org/x/text/encoding/simplifiedchinese"
)

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

// NormalizePath canonicalizes EPUB-internal paths without allowing them to
// escape the archive root. The flow builder uses the same canonical form when
// matching TOC entries to parsed chapter source paths.
func NormalizePath(p string) string {
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
		return NormalizePath(baseFile)
	}
	var parts []string
	if strings.HasPrefix(clean, "/") {
		parts = nil
	} else {
		base := NormalizePath(baseFile)
		if base != "" {
			segs := strings.Split(base, "/")
			parts = append(parts, segs[:len(segs)-1]...)
		}
	}
	parts = append(parts, strings.Split(clean, "/")...)
	return NormalizePath(strings.Join(parts, "/"))
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
