package layout

import (
	"crypto/sha256"
	_ "embed"
	"encoding/hex"
	"fmt"
)

// 共享 WebFont：服务端分页（Chromium）与客户端页面渲染使用**同一字体文件**，
// 保证分页结果在两端逐像素一致。字体经子集化（CJK 统一表意区 + 常用标点 +
// ASCII/Latin-1），缺字时两端同样回退到各自的系统字体（罕见字场景会有
// 细微差异，属可接受范围）。
//
// 生成方式（一次性）：
//
//	curl -L -o NotoSerifSC-Regular.otf \
//	  https://github.com/notofonts/noto-cjk/raw/main/Serif/SubsetOTF/SC/NotoSerifSC-Regular.otf
//	# 字符集：0x20-0x7E, 0xA0-0xFF, 0x2000-0x206F, 0x3000-0x303F,
//	#         0x4E00-0x9FFF, 0xFE10-0xFE1F, 0xFE30-0xFE4F, 0xFF00-0xFFEF
//	npx subset-font NotoSerifSC-Regular.otf charset.txt \
//	  --flavor woff2 --target-format woff2 -o revaro-serif.woff2
//
// 字体来源为 OFL-1.1 许可（Noto Serif SC）。
//
//go:embed fonts/revaro-serif.woff2
var serifFont []byte

// FontFamilySerif 是共享样式表里 @font-face 声明的族名；profile 默认用它。
const FontFamilySerif = "Revaro Serif"

// FontFile 返回内嵌字体文件名（不含路径）。
const FontFile = "revaro-serif.woff2"

// FontBytes 返回内嵌字体字节。
func FontBytes() []byte { return serifFont }

// FontVersion 是字体内容的短哈希，用作不可变缓存的版本参数。
func FontVersion() string {
	sum := sha256.Sum256(serifFont)
	return hex.EncodeToString(sum[:])[:12]
}

// FontFaceCSS 返回引用字体的 @font-face 规则。fontURL 是字体文件的
// 完整 URL：客户端场景用 /api/reader/fonts/...；分页器内部用拦截源的
// 绝对 URL。两处最终都解析到同一份 FontBytes。
func FontFaceCSS(fontURL string) string {
	return fmt.Sprintf(`@font-face{font-family:%q;src:url(%q) format("woff2");font-display:swap;}`, FontFamilySerif, fontURL)
}
