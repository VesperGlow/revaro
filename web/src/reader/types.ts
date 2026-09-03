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
  // 服务端 Stable NavAnchor：仅媒体目标（img/svg/video）输出，chunk HTML
  // 中存在 data-rv-anchor="<nav_anchor>" 的媒体元素，客户端按真实元素
  // rect 定栏。文本目标不再注入 DOM 标记。缺失时回退 text locator /
  // source_fragment / 块起点。
  nav_anchor?: string
  // 文本目标的导航 locator（服务端从清洗后 DOM 解析）：实际文本节点相对
  // 块元素的 childNodes 下标链 + 首个可见字符的 UTF-16 偏移。客户端加载
  // 目标 chunk 后解析真实 Text node，用 collapsed caret Range 的 rect
  // 计算栏（空 inline 标记在 column break 处会停在上一栏，已废弃）。
  text_path?: number[]
  text_offset?: number
  // 导航目标所在 chunk：目录跳转只需加载该 chunk（服务端已把目标块尽量
  // 切到 chunk 头部）。缺失时按 block 反查。
  chunk?: number
  // 原始 EPUB 目录 href（调试与回退）：locator 解析失败时用
  // source_fragment 做块内精确定位回退（服务端已 percent-decode 一次）。
  source_path?: string
  source_fragment?: string
}

export interface FlowManifest {
  version: number
  format: 'epub' | 'txt'
  total_chars: number
  // 书 blob 内容指纹（服务端注入）：客户端持久缓存用它隔离 chunk 键，
  // 同一文件 id 被替换为不同内容后指纹改变，旧缓存自然失效。
  book_key?: string
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
