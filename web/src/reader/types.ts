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
  // EPUB 目录原始 fragment（服务端已 percent-decode 一次）：块内精确定位用。
  // 缺失或块内找不到时回退块起点；TXT 目标没有该字段。
  fragment?: string
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
