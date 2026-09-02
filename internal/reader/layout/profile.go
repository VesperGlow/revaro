// Package layout 是「服务端固定分页」阅读架构的排版层：
//   - Profile 描述一次不可变的分页参数（viewport、字号、字体、行距、边距），
//     与书内容哈希一起派生出稳定的 profileID；
//   - Anchor（readingAnchor）是跨 layout 稳定的阅读位置：spine 章序号 +
//     清洗后 DOM 的节点路径 + 文本偏移；
//   - Manifest 记录每个页面的 start/end anchor；
//   - Pager 用 headless Chromium 做真实排版，把整本 spine 切成固定 Page HTML。
//
// 术语约定：位置统一叫 locator / readingAnchor，不叫 CFI。
package layout

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"strconv"
	"strings"
)

// formatVersion 参与 profileID 计算：profile 语义（哪些字段、如何规范化）
// 变化时必须递增，避免旧 manifest 被误当成新语义的产物复用。
const formatVersion = "v1"

// Profile 是一次固定分页的全部排版参数。零值字段在使用前由 normalized()
// 补齐默认值；序列化与哈希只看规范化后的字段，因此结构顺序不影响 ID。
type Profile struct {
	// ViewportW/H 是阅读视口的 CSS 像素尺寸（不含浏览器 chrome）。
	ViewportW int `json:"viewport_w"`
	ViewportH int `json:"viewport_h"`
	// FontSize 是正文基准字号（px）。
	FontSize int `json:"font_size"`
	// FontFamily 是正文字体族名，与共享样式表里的 @font-face 对应。
	FontFamily string `json:"font_family"`
	// LineHeight 是行距倍数。
	LineHeight float64 `json:"line_height"`
	// MarginTop/Bottom/Side 是页面内边距（px）。
	MarginTop    int `json:"margin_top"`
	MarginBottom int `json:"margin_bottom"`
	MarginSide   int `json:"margin_side"`
}

func defaultProfile() Profile {
	return Profile{
		ViewportW:    800,
		ViewportH:    600,
		FontSize:     19,
		FontFamily:   FontFamilySerif,
		LineHeight:   1.7,
		MarginTop:    60,
		MarginBottom: 24,
		MarginSide:   44,
	}
}

// normalized 返回补全默认值后的 Profile；非法值（负数等）由 Validate 负责。
func (p Profile) normalized() Profile {
	d := defaultProfile()
	if p.ViewportW > 0 {
		d.ViewportW = p.ViewportW
	}
	if p.ViewportH > 0 {
		d.ViewportH = p.ViewportH
	}
	if p.FontSize > 0 {
		d.FontSize = p.FontSize
	}
	if p.FontFamily != "" {
		d.FontFamily = p.FontFamily
	}
	if p.LineHeight > 0 {
		d.LineHeight = p.LineHeight
	}
	if p.MarginTop > 0 {
		d.MarginTop = p.MarginTop
	}
	if p.MarginBottom > 0 {
		d.MarginBottom = p.MarginBottom
	}
	if p.MarginSide > 0 {
		d.MarginSide = p.MarginSide
	}
	return d
}

// Validate 检查取值范围；所有值都必须能被 Chromium 安全地用作 CSS 数值。
func (p Profile) Validate() error {
	if p.ViewportW < 100 || p.ViewportW > 10000 {
		return fmt.Errorf("viewport_w must be between 100 and 10000")
	}
	if p.ViewportH < 100 || p.ViewportH > 10000 {
		return fmt.Errorf("viewport_h must be between 100 and 10000")
	}
	if p.FontSize < 8 || p.FontSize > 96 {
		return fmt.Errorf("font_size must be between 8 and 96")
	}
	if !validFontFamily(p.FontFamily) {
		return fmt.Errorf("font_family contains unsupported characters")
	}
	if p.LineHeight < 1.0 || p.LineHeight > 3.0 {
		return fmt.Errorf("line_height must be between 1.0 and 3.0")
	}
	if p.MarginTop < 0 || p.MarginTop > 400 || p.MarginBottom < 0 || p.MarginBottom > 400 {
		return fmt.Errorf("margins must be between 0 and 400")
	}
	if p.MarginSide < 0 || p.MarginSide > 400 {
		return fmt.Errorf("margin_side must be between 0 and 400")
	}
	inner := p.InnerWidth()
	if inner < 60 || p.InnerHeight() < 60 {
		return fmt.Errorf("margins leave no readable inner area")
	}
	return nil
}

// validFontFamily 只放行 CSS 字体族名里安全可嵌入的字符，防止 profile 注入 CSS。
func validFontFamily(name string) bool {
	if name == "" || len(name) > 200 {
		return false
	}
	for _, r := range name {
		switch {
		case r >= 'a' && r <= 'z', r >= 'A' && r <= 'Z', r >= '0' && r <= '9':
		case r == ' ', r == ',', r == '-', r == '_', r == '\'', r == '"':
		default:
			return false
		}
	}
	return true
}

// Normalized 返回补全默认值后的 Profile（零值字段用默认值填充）。
func (p Profile) Normalized() Profile { return p.normalized() }

// Canonical 是 profile 的规范化文本表示，用于哈希。
func (p Profile) Canonical() string {
	p = p.normalized()
	return strings.Join([]string{
		"viewport_w=" + strconv.Itoa(p.ViewportW),
		"viewport_h=" + strconv.Itoa(p.ViewportH),
		"font_size=" + strconv.Itoa(p.FontSize),
		"font_family=" + p.FontFamily,
		"line_height=" + strconv.FormatFloat(p.LineHeight, 'f', 3, 64),
		"margin_top=" + strconv.Itoa(p.MarginTop),
		"margin_bottom=" + strconv.Itoa(p.MarginBottom),
		"margin_side=" + strconv.Itoa(p.MarginSide),
	}, "\n")
}

// ID 返回 profile 与书内容哈希共同决定的稳定标识。
func (p Profile) ID(bookHash string) string {
	sum := sha256.Sum256([]byte(formatVersion + "\x00" + bookHash + "\x00" + p.Canonical()))
	return formatVersion + "-" + hex.EncodeToString(sum[:])
}

// InnerWidth 是栏宽（= 视口宽 - 两侧边距）。
func (p Profile) InnerWidth() int {
	p = p.normalized()
	return p.ViewportW - 2*p.MarginSide
}

// InnerHeight 是栏高（= 视口高 - 上下边距）。
func (p Profile) InnerHeight() int {
	p = p.normalized()
	return p.ViewportH - p.MarginTop - p.MarginBottom
}
