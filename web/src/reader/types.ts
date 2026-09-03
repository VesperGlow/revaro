// 连续 reading flow 阅读器的类型契约，与 internal/reader/flow 的 JSON 对应。
// 位置统一叫 readingAnchor（locator，不叫 CFI）。

export interface ReadingAnchor {
  spine: number
  block: number
  // path/offset：块内 childNodes 下标链 + 文本节点 UTF-16 偏移；
  // offset = -1 表示「块（或 Path 末尾元素）之前」的元素边界。
  path: number[]
  offset: number
}

export interface FlowSpineMeta {
  block_start: number
  block_count: number
}

export interface FlowChunkMeta {
  index: number
  block_start: number
  block_count: number
  chars: number
  bytes?: number
  url?: string
}

export interface FlowTocEntry {
  label: string
  depth: number
  spine: number
  block: number
  // 服务端 Stable NavAnchor：清洗期已把 data-rv-anchor="<nav_anchor>" 绑定
  // 到实际可见目标节点（文本目标 = 首个有效文本位置前的内联标记；图片/SVG
  // 目标 = 媒体元素自身；无 fragment = 目标 spine 首个真实可见内容）。
  // 字段名与 Go JSON tag 一致。缺失（TXT 或极端回退）时回退块起点。
  nav_anchor?: string
  // NavAnchor 绑定节点所在 chunk：目录跳转只需加载该 chunk（服务端已把
  // 绑定块尽量切到 chunk 头部）。缺失时按 block 反查。
  chunk?: number
  // 原始 EPUB 目录 href（调试与回退）：绑定节点缺失时用 source_fragment
  // 做块内精确定位回退（服务端已 percent-decode 一次）。
  source_path?: string
  source_fragment?: string
}

export interface FlowManifest {
  version: number
  format: 'epub' | 'txt'
  total_chars: number
  spines: FlowSpineMeta[]
  chunks: FlowChunkMeta[]
  toc: FlowTocEntry[]
  generated_at?: string
}

export interface BookProgress {
  anchor?: ReadingAnchor | null
}

export interface ReaderPrefs {
  fontSize: number
  lineHeight: number
  theme: 'light' | 'dark'
}
