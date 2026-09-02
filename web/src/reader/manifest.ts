import type { LayoutManifest, ReadingAnchor } from './types'

// readingAnchor 的全书全序：与 Go 端 Anchor.Compare 完全一致
// （spine → path 逐元素 → offset；offset -1 表示元素边界）。
export function compareAnchor(a: ReadingAnchor, b: ReadingAnchor): number {
  if (a.spine !== b.spine) return a.spine < b.spine ? -1 : 1
  const n = Math.min(a.path.length, b.path.length)
  for (let i = 0; i < n; i++) {
    if (a.path[i] !== b.path[i]) return a.path[i] < b.path[i] ? -1 : 1
  }
  if (a.path.length !== b.path.length) return a.path.length < b.path.length ? -1 : 1
  if (a.offset !== b.offset) return a.offset < b.offset ? -1 : 1
  return 0
}

// pageForAnchor 返回锚点所在页：Start <= a < End 的页；恰在边界时归属下一页。
// 锚点跨 layout 稳定，因此可安全用于「旧 layout 进度 → 新 layout」的映射。
export function pageForAnchor(m: LayoutManifest, anchor: ReadingAnchor): number {
  if (!m.pages.length) return 0
  let lo = 0
  let hi = m.pages.length
  while (lo < hi) {
    const mid = (lo + hi) >> 1
    if (compareAnchor(m.pages[mid].start, anchor) <= 0) lo = mid + 1
    else hi = mid
  }
  if (lo === 0) return 0
  return Math.min(lo - 1, m.pages.length - 1)
}

export function anchorForPage(m: LayoutManifest, index: number): ReadingAnchor | null {
  if (index < 0 || index >= m.pages.length) return null
  return m.pages[index].start
}

export function clampPage(m: LayoutManifest, page: number): number {
  const last = Math.max(0, m.page_count - 1)
  return Math.min(Math.max(0, page), last)
}

// tocActiveIndex 返回当前页对应的目录高亮条目（最后一个 page <= 当前页的条目）。
export function tocActiveIndex(m: LayoutManifest, page: number): number {
  let active = -1
  for (let i = 0; i < m.toc.length; i++) {
    if (m.toc[i].page <= page) active = i
    else break
  }
  return active
}
