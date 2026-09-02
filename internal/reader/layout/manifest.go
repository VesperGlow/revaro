package layout

import "sort"

// PageMeta 描述一个固定页：Spine 是来源章，Start/End 是该页覆盖的
// 内容范围（End 等于下一页的 Start，全书无缝拼接），URL 指向页面产物。
type PageMeta struct {
	Index int    `json:"index"`
	Spine int    `json:"spine"`
	Start Anchor `json:"start"`
	End   Anchor `json:"end"`
	URL   string `json:"url,omitempty"`
	Bytes int64  `json:"bytes,omitempty"`
}

// TOCMeta 是目录条目在某个 layout 里的落点（页码），随 manifest 持久化，
// 客户端目录抽屉直接用它跳页，不再做任何内容定位。
type TOCMeta struct {
	Label    string `json:"label"`
	Page     int    `json:"page"`
	Depth    int    `json:"depth"`
	Spine    int    `json:"spine,omitempty"`
	Fragment string `json:"fragment,omitempty"`
}

// Manifest 是一次分页的完整产物清单。Pages 按阅读顺序排列，
// 锚点单调递增（单调性是分页器的硬约束，切页验证会断言）。
type Manifest struct {
	Version   int        `json:"version"`
	BookHash  string     `json:"book_hash"`
	ProfileID string     `json:"profile_id"`
	Profile   Profile    `json:"profile"`
	PageCount int        `json:"page_count"`
	Pages     []PageMeta `json:"pages"`
	TOC       []TOCMeta  `json:"toc"`
	// Complete 为 false 时是渐进式分页的中间快照：Pages/TOC 只含已生成
	// 的章（按 spine 顺序排列），后续发布的新快照会替换本对象。客户端
	// 在任何快照下都能阅读：锚点单调性对已生成前缀保持成立。
	Complete bool `json:"complete"`
	// GeneratedAt 是 RFC3339 时间戳，仅用于调试与老化回收。
	GeneratedAt string `json:"generated_at,omitempty"`
}

// PageForAnchor 返回锚点所在的页序号：满足 Start <= a < End 的页；
// a 恰好落在页边界时归属下一页（与「当前页 start anchor 保存进度」语义一致）。
// 找不到时返回 -1（例如空书）。
func (m *Manifest) PageForAnchor(a Anchor) int {
	if m == nil || len(m.Pages) == 0 {
		return -1
	}
	idx := sort.Search(len(m.Pages), func(i int) bool {
		return m.Pages[i].Start.Compare(a) > 0
	})
	if idx == 0 {
		// a 在全书开头之前：属于第一页。
		if a.Compare(m.Pages[0].Start) < 0 {
			return 0
		}
		return -1
	}
	return idx - 1
}

// AnchorForPage 返回第 index 页的 start anchor（进度保存用）。
func (m *Manifest) AnchorForPage(index int) (Anchor, bool) {
	if m == nil || index < 0 || index >= len(m.Pages) {
		return Anchor{}, false
	}
	return m.Pages[index].Start, true
}
