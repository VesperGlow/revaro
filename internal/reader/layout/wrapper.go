package layout

import (
	"encoding/json"
	"fmt"
	"html"
	"strings"
	"unicode/utf8"
)

// 分页器在 Chromium 里加载一个自包含 wrapper 页面：文档本身、字体、内嵌
// 图片全部由拦截源（pagerOrigin）经 Fetch 拦截满足，Chromium 不发起任何
// 真实网络请求。整本 spine 装进 #revaro-book，每章一个 .spine 分栏段，
// 排版参数（viewport、字号、行距、边距）以 inline style 锁定。

// pagerOrigin 是 wrapper 页面的伪源，仅用于把相对资源 URL 变成可拦截的
// 绝对 URL；DNS 永远不被解析。
const pagerOrigin = "http://revaro.local"
const pagerMainPath = "/book"

// PagerMainURL 是 wrapper 文档的绝对 URL。
func PagerMainURL() string { return pagerOrigin + pagerMainPath }

// PagerFontURL 是共享字体在拦截源下的绝对 URL。
func PagerFontURL() string { return pagerOrigin + "/fonts/" + FontFile }

// ChapterInput 是分页器的一章输入。
type ChapterInput struct {
	Spine int
	HTML  string
}

// jsProfile 是注入 wrapper 页面的 profile JSON（camelCase，供 pager.js 使用）。
type jsProfile struct {
	ViewportW    int         `json:"viewportW"`
	ViewportH    int         `json:"viewportH"`
	FontSize     int         `json:"fontSize"`
	FontFamily   string      `json:"fontFamily"`
	LineHeight   float64     `json:"lineHeight"`
	MarginTop    int         `json:"marginTop"`
	MarginBottom int         `json:"marginBottom"`
	MarginSide   int         `json:"marginSide"`
	TXT          bool        `json:"txt"`
	TOC          []TOCTarget `json:"toc"`
}

// BuildWrapper 生成 wrapper 页面的完整 HTML。chapters 是清洗后的章正文；
// profile 决定排版参数；targets 是要映射页码的目录目标；pagerJS 与
// sharedCSS 来自内嵌文件。
func BuildWrapper(chapters []ChapterInput, profile Profile, txt bool, sharedCSS, pagerJS string, targets []TOCTarget) string {
	profile = profile.normalized()
	var b strings.Builder
	b.WriteString("<!doctype html><html><head><meta charset=\"utf-8\">")
	b.WriteString("<base href=\"" + pagerOrigin + "/\">")
	b.WriteString("<style>")
	b.WriteString(FontFaceCSS(PagerFontURL()))
	b.WriteString("\n")
	b.WriteString(sharedCSS)
	b.WriteString("</style></head><body style=\"margin:0;")
	fmt.Fprintf(&b, "--revaro-font-family:%s;", cssFontFamily(profile.FontFamily))
	fmt.Fprintf(&b, "--revaro-font-size:%dpx;", profile.FontSize)
	fmt.Fprintf(&b, "--revaro-line-height:%s;", fmt.Sprintf("%.3f", profile.LineHeight))
	b.WriteString("\"><main id=\"revaro-book\">")
	for _, ch := range chapters {
		innerH := profile.InnerHeight()
		b.WriteString("<section class=\"spine\" data-spine=\"" + fmt.Sprint(ch.Spine) + "\" style=\"")
		fmt.Fprintf(&b, "width:%dpx;height:%dpx;", profile.ViewportW, profile.ViewportH)
		fmt.Fprintf(&b, "padding:%dpx %dpx %dpx;", profile.MarginTop, profile.MarginSide, profile.MarginBottom)
		fmt.Fprintf(&b, "column-width:%dpx;column-gap:%dpx;", profile.InnerWidth(), 2*profile.MarginSide)
		fmt.Fprintf(&b, "--revaro-col-height:%dpx;", innerH)
		b.WriteString("\">")
		class := "revaro-content"
		if txt {
			class += " txt"
		}
		b.WriteString("<div class=\"" + class + "\">")
		b.WriteString(ch.HTML)
		b.WriteString("</div></section>")
	}
	b.WriteString("</main><script>window.__revaroProfile = ")
	if targets == nil {
		targets = []TOCTarget{}
	}
	profileJSON, _ := json.Marshal(jsProfile{
		ViewportW:    profile.ViewportW,
		ViewportH:    profile.ViewportH,
		FontSize:     profile.FontSize,
		FontFamily:   profile.FontFamily,
		LineHeight:   profile.LineHeight,
		MarginTop:    profile.MarginTop,
		MarginBottom: profile.MarginBottom,
		MarginSide:   profile.MarginSide,
		TXT:          txt,
		TOC:          targets,
	})
	b.Write(profileJSON)
	b.WriteString(";</script><script>")
	b.WriteString(pagerJS)
	b.WriteString("</script></body></html>")
	return b.String()
}

// cssFontFamily 把 profile 的字体族名安全地嵌入 CSS（Validate 已保证字符集）。
func cssFontFamily(family string) string {
	// 族名可能含逗号分隔的回退列表；每个裸名字加引号由共享样式表处理，
	// 这里原样输出（仅放行过安全字符）。
	return family
}

// TXTChaptersWithTOC 与 TXTChapters 相同，但额外返回每条目录
// （按 tocOffsets 下标）所在的章块序号：渐进式分页需要知道 toc-anchor
// 目标落在哪个 chunk。
func TXTChaptersWithTOC(text string, tocOffsets []int64) (chapters []string, tocChunk []int) {
	type mark struct {
		offset int64 // UTF-16 码元偏移
		index  int
	}
	marks := make([]mark, 0, len(tocOffsets))
	units := utf16UnitCount(text)
	prev := int64(-1)
	for i, off := range tocOffsets {
		if off > 0 && off < units && off > prev {
			marks = append(marks, mark{offset: off, index: i})
			prev = off
		}
	}
	cuts := []int64{0}
	for _, m := range marks {
		cuts = append(cuts, utf16OffsetToByte(text, m.offset))
	}
	cuts = append(cuts, int64(len(text)))
	if len(marks) == 0 && units > 60000 {
		cuts = []int64{0}
		unitsSeen := int64(0)
		for bytePos := range text {
			if unitsSeen > 0 && unitsSeen%20000 == 0 {
				cuts = append(cuts, int64(bytePos))
			}
			unitsSeen += utf16UnitsOfRune(runeAt(text, bytePos))
		}
		cuts = append(cuts, int64(len(text)))
	}
	tocChunk = make([]int, len(tocOffsets))
	for i := range tocChunk {
		tocChunk[i] = -1
	}
	for chunkIdx := 0; chunkIdx < len(cuts)-1; chunkIdx++ {
		start, end := cuts[chunkIdx], cuts[chunkIdx+1]
		if end <= start {
			continue
		}
		var b strings.Builder
		if start > 0 {
			for _, m := range marks {
				if utf16OffsetToByte(text, m.offset) == start {
					fmt.Fprintf(&b, "<span class=\"toc-anchor\" data-toc=\"%d\"></span>", m.index)
					tocChunk[m.index] = chunkIdx
				}
			}
		}
		b.WriteString(html.EscapeString(text[start:end]))
		chapters = append(chapters, b.String())
	}
	if len(chapters) == 0 {
		chapters = []string{""}
	}
	return chapters, tocChunk
}

// TXTChapters 把 TXT 全文切成章块 HTML：
// 目录偏移（UTF-16 码元）切章，无目录的长文按 20000 码元分段；锚点 span
// 带 data-toc。切割点一律落在码元边界，字节偏移由内部换算。
func TXTChapters(text string, tocOffsets []int64) []string {
	type mark struct {
		offset int64 // UTF-16 码元偏移
		index  int
	}
	marks := make([]mark, 0, len(tocOffsets))
	units := utf16UnitCount(text)
	prev := int64(-1)
	for i, off := range tocOffsets {
		if off > 0 && off < units && off > prev {
			marks = append(marks, mark{offset: off, index: i})
			prev = off
		}
	}
	cuts := []int64{0}
	for _, m := range marks {
		cuts = append(cuts, utf16OffsetToByte(text, m.offset))
	}
	cuts = append(cuts, int64(len(text)))
	if len(marks) == 0 && units > 60000 {
		cuts = []int64{0}
		unitsSeen := int64(0)
		for bytePos := range text {
			if unitsSeen > 0 && unitsSeen%20000 == 0 {
				cuts = append(cuts, int64(bytePos))
			}
			unitsSeen += utf16UnitsOfRune(runeAt(text, bytePos))
		}
		cuts = append(cuts, int64(len(text)))
	}
	var chapters []string
	for i := 0; i < len(cuts)-1; i++ {
		start, end := cuts[i], cuts[i+1]
		if end <= start {
			continue
		}
		var b strings.Builder
		if start > 0 {
			for _, m := range marks {
				if utf16OffsetToByte(text, m.offset) == start {
					fmt.Fprintf(&b, "<span class=\"toc-anchor\" data-toc=\"%d\"></span>", m.index)
				}
			}
		}
		b.WriteString(html.EscapeString(text[start:end]))
		chapters = append(chapters, b.String())
	}
	if len(chapters) == 0 {
		chapters = []string{""}
	}
	return chapters
}

// utf16UnitCount 返回字符串的 UTF-16 码元数（JS string.length 语义）。
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
