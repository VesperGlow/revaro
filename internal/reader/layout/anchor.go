package layout

// Anchor 是跨 layout profile 稳定的阅读位置（readingAnchor）：
//   - Spine 是 Book.Chapters 里的章序号（spine 项序号）；
//   - Path 是从章内容根节点（.revaro-content）出发的子节点下标路径，
//     最后一个元素是目标节点在其父节点中的下标；
//   - Offset 是目标文本节点内的字符偏移；当锚点指向「某元素之前」的
//     元素边界位置时 Offset = -1。
//
// 锚点由分页器在同一棵清洗后 DOM 上确定性生成：同一位置在任何 profile
// 下都得到同一个 Anchor，因此它可以作为阅读进度，并用于旧 layout →
// 新 layout 的无缝切换。清洗后的 DOM 全书不变，锚点也就全书稳定。
type Anchor struct {
	Spine  int   `json:"spine"`
	Path   []int `json:"path"`
	Offset int   `json:"offset"` // -1 表示元素边界（位于 Path 末尾下标的子元素之前）
}

// Compare 提供全书稳定的全序（spine → path 逐元素 → offset），
// manifest 内的锚点按此顺序单调排列，可用于二分查找。
func (a Anchor) Compare(b Anchor) int {
	if a.Spine != b.Spine {
		if a.Spine < b.Spine {
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

// Valid 校验锚点的取值范围（防止恶意/损坏的进度数据）。
func (a Anchor) Valid() bool {
	if a.Spine < 0 || a.Spine >= 1<<20 {
		return false
	}
	if len(a.Path) > 64 {
		return false
	}
	for _, i := range a.Path {
		if i < 0 || i >= 1<<20 {
			return false
		}
	}
	return a.Offset >= -1 && a.Offset < 1<<24
}
