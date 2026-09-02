// 服务端固定分页阅读器（v2）的类型契约，与 internal/reader/layout 的 JSON 一一对应。
// 位置统一叫 readingAnchor（不叫 CFI）。

export interface LayoutProfile {
  viewport_w: number
  viewport_h: number
  font_size: number
  font_family: string
  line_height: number
  margin_top: number
  margin_bottom: number
  margin_side: number
}

export interface ReadingAnchor {
  spine: number
  path: number[]
  offset: number // -1 表示元素边界位置
}

export interface PageMeta {
  index: number
  spine: number
  start: ReadingAnchor
  end: ReadingAnchor
  url?: string
  bytes?: number
}

export interface TocMeta {
  label: string
  page: number
  depth: number
  spine?: number
  fragment?: string
}

export interface LayoutManifest {
  version: number
  book_hash: string
  profile_id: string
  profile: LayoutProfile
  page_count: number
  pages: PageMeta[]
  toc: TocMeta[]
  // complete=false 是渐进式分页的中间快照：pages/toc 只含已生成的章，
  // 后续新快照会替换本对象；全局页码随快照更新（锚点位置不变）。
  complete?: boolean
  generated_at?: string
}

export interface LayoutStatus {
  profile_id: string
  status: 'queued' | 'running' | 'done' | 'error'
  phase?: 'window' | 'background'
  error?: string
  pages?: number
  page_count?: number
  complete?: boolean
  spines_done?: number
  spines_total?: number
  manifest?: string
}

export interface BookProgress {
  anchor?: ReadingAnchor | null
  profile?: string | null
}

export interface ReaderPrefs {
  fontSize: number
  lineHeight: number
  theme: 'light' | 'dark'
}
