// Package flow 定义 Revaro 阅读器的连续 reading flow 模型与生成器。
//
// 阅读流程（reading flow）：把整本书（EPUB 各 spine 或 TXT 各章节）的清洗后
// 内容排成一个**连续的排版流**——文本与图片处于同一个排版上下文，不再以
// spine 为分页/翻页边界。服务端把 flow 按自然 DOM 边界切成若干 chunk（块组），
// chunk 不是页面，不与任何 viewport/字号绑定；客户端只加载当前位置附近的少量
// chunk，把它们装进统一 DOM，用浏览器原生 CSS columns 完成最终分页。
//
// 位置（locator / readingAnchor）在全书上稳定：Anchor 定位到某个 spine 内
// 某个顶层内容块（Block，全书统一编号）内部的具体文本位置，跨 chunk、跨
// 客户端分页结果、跨字号/横竖屏始终有效。
package flow

import "encoding/json"

// Anchor 是跨客户端分页稳定的阅读位置（readingAnchor）：
//   - Spine 是内容块所属章节（EPUB spine 或 TXT 章节）的序号；
//   - Block 是内容块在全书的统一编号（data-block 属性，客户端 DOM 可直接寻址）；
//   - Path 是从块元素出发的子节点下标路径（childNodes 下标，含文本节点），
//     最后一个元素指向承载 Offset 的节点；空路径表示块元素本身；
//   - Offset 是目标文本节点内的字符偏移（UTF-16 码元）；Offset = -1 表示
//     「Path 末尾元素（或空路径时的块元素）之前」的元素边界位置。
//
// 旧格式（{spine, path, offset}，path[0] 曾是章内顶层块序号）在反序列化时
// 自动迁移为 {spine, block: path[0], path: path[1:], offset}。
type Anchor struct {
	Spine  int   `json:"spine"`
	Block  int   `json:"block"`
	Path   []int `json:"path,omitempty"`
	Offset int   `json:"offset"`
}

// Compare 提供全书稳定的全序（spine → block → path 逐元素 → offset），
// 用于锚点排序/二分。
func (a Anchor) Compare(b Anchor) int {
	if a.Spine != b.Spine {
		if a.Spine < b.Spine {
			return -1
		}
		return 1
	}
	if a.Block != b.Block {
		if a.Block < b.Block {
			return -1
		}
		return 1
	}
	n := len(a.Path)
	if len(b.Path) < n {
		n = len(b.Path)
	}
	for i := 0; i < n; i++ {
		if a.Path[i] != b.Path[i] {
			if a.Path[i] < b.Path[i] {
				return -1
			}
			return 1
		}
	}
	if len(a.Path) != len(b.Path) {
		if len(a.Path) < len(b.Path) {
			return -1
		}
		return 1
	}
	if a.Offset != b.Offset {
		if a.Offset < b.Offset {
			return -1
		}
		return 1
	}
	return 0
}

// Valid 校验锚点取值范围（防止恶意/损坏的进度数据）。
func (a Anchor) Valid() bool {
	if a.Spine < 0 || a.Spine >= 1<<20 {
		return false
	}
	if a.Block < 0 || a.Block >= 1<<26 {
		return false
	}
	if len(a.Path) > 48 {
		return false
	}
	for _, i := range a.Path {
		if i < 0 || i >= 1<<20 {
			return false
		}
	}
	return a.Offset >= -1 && a.Offset < 1<<26
}

// rawAnchor 是 Anchor 的显式 JSON 形态，用于区分新旧格式。
type rawAnchor struct {
	Spine  int   `json:"spine"`
	Block  int   `json:"block"`
	Path   []int `json:"path"`
	Offset int   `json:"offset"`
}

// UnmarshalJSON 兼容旧格式进度：旧负载没有 block 字段，
// 其 path[0] 就是章内顶层块序号，迁移为全局块定位。
func (a *Anchor) UnmarshalJSON(data []byte) error {
	var probe map[string]json.RawMessage
	if err := json.Unmarshal(data, &probe); err != nil {
		return err
	}
	raw := rawAnchor{Offset: -1}
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	if _, hasBlock := probe["block"]; !hasBlock {
		// 旧格式：{spine, path, offset}
		if len(raw.Path) > 0 {
			raw.Block = raw.Path[0]
			raw.Path = raw.Path[1:]
		} else {
			// 空 path（章节根/书首边界）视为块 0 起点
			raw.Block = 0
		}
	}
	*a = Anchor{Spine: raw.Spine, Block: raw.Block, Path: raw.Path, Offset: raw.Offset}
	return nil
}
