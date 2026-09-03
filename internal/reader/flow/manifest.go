package flow

// Manifest 是一本书的连续 reading flow 的产物清单：
//   - 全书按内容哈希与 flow 版本缓存（见 server 层）；
//   - Chunks 按阅读顺序覆盖全部内容块（全书统一 data-block 编号），
//     chunk 只按自然 DOM 边界与体量切分，与任何 viewport/字号无关；
//   - Spines 记录每章（EPUB spine / TXT 章节）的块区间，用于
//     旧进度迁移、目录分组与块→章换算；
//   - TOC 每条目标解析为 Stable NavAnchor（绑定实际可见目标节点 +
//     所在 chunk），Spine/Block 保留用于高亮、进度换算与回退。
type Manifest struct {
	Version    int         `json:"version"`     // flow 生成器格式版本
	Format     string      `json:"format"`      // "epub" | "txt"
	TotalChars int64       `json:"total_chars"` // 全文 UTF-16 码元数（文本进度基准）
	Spines     []SpineMeta `json:"spines"`
	Chunks     []ChunkMeta `json:"chunks"`
	TOC        []TOCTarget `json:"toc"`
	// GeneratedAt 仅用于调试。
	GeneratedAt string `json:"generated_at,omitempty"`
}

// SpineMeta 描述一章的内容块区间。块编号全书连续，
// 第 s 章的块区间为 [BlockStart, BlockStart+BlockCount)。
type SpineMeta struct {
	BlockStart int `json:"block_start"`
	BlockCount int `json:"block_count"`
}

// ChunkMeta 描述一个 flow chunk。Chunk 内容 = 从 BlockStart 起连续
// BlockCount 个顶层内容块（data-block=BlockStart…）。Chars 是该 chunk
// 内文本的 UTF-16 码元数（滑动进度用）；Bytes 是 HTML 片段估算值。
type ChunkMeta struct {
	Index      int    `json:"index"`
	BlockStart int    `json:"block_start"`
	BlockCount int    `json:"block_count"`
	Chars      int64  `json:"chars"`
	Bytes      int    `json:"bytes,omitempty"`
	URL        string `json:"url,omitempty"`
}

// TOCTarget 是目录条目的导航目标（Stable NavAnchor 体系）：
//   - Spine/Block 供目录高亮、进度换算与回退定位；
//   - NavAnchor 是清洗期绑定到实际可见目标节点的稳定 synthetic id，
//     chunk HTML 中存在 data-rv-anchor="<id>" 的绑定节点（文本目标 =
//     首个有效文本位置前的内联标记；图片/SVG 目标 = 媒体元素自身；
//     无 fragment = 目标 spine 首个真实可见内容）。客户端目录跳转只
//     加载 Chunk，按 data-rv-anchor 找节点并按其实际布局定栏；
//   - SourcePath/SourceFragment 保留 EPUB 目录原始 href（调试与回退）。
type TOCTarget struct {
	Label     string `json:"label"`
	Depth     int    `json:"depth"`
	Spine     int    `json:"spine"`
	Block     int    `json:"block"`
	NavAnchor string `json:"nav_anchor,omitempty"`
	// Chunk 是 NavAnchor 绑定节点所在的 chunk；客户端目录跳转只需加载
	// 它。始终输出（chunk 0 也是合法值）。
	Chunk int `json:"chunk"`
	// SourcePath/SourceFragment 保留原始目录 href 的路径与 fragment
	// （解析层已 percent-decode 一次），用于调试与客户端回退。
	SourcePath     string `json:"source_path,omitempty"`
	SourceFragment string `json:"source_fragment,omitempty"`
}

// SpineForBlock 返回块 b 所属的章（全书块编号 → spine 序号）。
// b 越界或书为空时返回 0。
func (m *Manifest) SpineForBlock(b int) int {
	if m == nil {
		return 0
	}
	best := -1
	for i, sp := range m.Spines {
		if b >= sp.BlockStart {
			best = i
		}
	}
	if best < 0 {
		return 0
	}
	if b >= m.Spines[best].BlockStart+m.Spines[best].BlockCount {
		return best
	}
	return best
}

// ChunkForBlock 返回包含块 b 的 chunk 序号；找不到时返回 -1。
func (m *Manifest) ChunkForBlock(b int) int {
	if m == nil {
		return -1
	}
	for i := range m.Chunks {
		c := &m.Chunks[i]
		if b >= c.BlockStart && b < c.BlockStart+c.BlockCount {
			return i
		}
	}
	return -1
}

// BlockStartChars 返回第 blockStart 个块之前（全书）的 UTF-16 文本长度，
// 用于把滑动进度（按字符）落到块/章。需要调用方保持 Chunks 有序。
func (m *Manifest) CharsBeforeChunk(chunkIndex int) int64 {
	if m == nil {
		return 0
	}
	var acc int64
	for i := 0; i < chunkIndex && i < len(m.Chunks); i++ {
		acc += m.Chunks[i].Chars
	}
	return acc
}

// TotalBlocks 返回全书内容块总数（= 各章块数之和）。
func (m *Manifest) TotalBlocks() int {
	if m == nil {
		return 0
	}
	n := 0
	for _, sp := range m.Spines {
		n += sp.BlockCount
	}
	return n
}
