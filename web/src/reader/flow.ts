import type { FlowManifest, ReadingAnchor } from './types'

// readingAnchor 全序：与 Go 端 flow.Anchor.Compare 完全一致
// （spine → block → path 逐元素 → offset）。
export function compareAnchor(a: ReadingAnchor, b: ReadingAnchor): number {
  if (a.spine !== b.spine) return a.spine < b.spine ? -1 : 1
  if (a.block !== b.block) return a.block < b.block ? -1 : 1
  const n = Math.min(a.path.length, b.path.length)
  for (let i = 0; i < n; i++) {
    if (a.path[i] !== b.path[i]) return a.path[i] < b.path[i] ? -1 : 1
  }
  if (a.path.length !== b.path.length) return a.path.length < b.path.length ? -1 : 1
  if (a.offset !== b.offset) return a.offset < b.offset ? -1 : 1
  return 0
}

// spineForBlock 返回块 b 所属的章序号（全书块编号 → spine）。
export function spineForBlock(m: FlowManifest, block: number): number {
  let best = 0
  for (let i = 0; i < m.spines.length; i++) {
    if (block >= m.spines[i].block_start) best = i
  }
  return best
}

// chunkForBlock 返回包含块 b 的 chunk 序号；找不到返回 -1。
export function chunkForBlock(m: FlowManifest, block: number): number {
  for (let i = 0; i < m.chunks.length; i++) {
    const c = m.chunks[i]
    if (block >= c.block_start && block < c.block_start + c.block_count) return i
  }
  return -1
}

// chunkPrefix 返回第 chunkIndex 个 chunk 之前的累计文本长度（UTF-16 码元）。
export function chunkPrefix(m: FlowManifest, chunkIndex: number): number {
  let acc = 0
  const upto = Math.min(chunkIndex, m.chunks.length)
  for (let i = 0; i < upto; i++) acc += m.chunks[i].chars
  return acc
}

// locateChar 把全局文本位置（UTF-16 码元）落到 (chunk, chunk 内偏移)。
// 每个 chunk 覆盖半开区间 [prefix, prefix+chars)，越界时收敛到首/末 chunk。
export function locateChar(m: FlowManifest, char: number): { chunk: number; offset: number } {
  if (!m.chunks.length) return { chunk: 0, offset: 0 }
  const target = Math.max(0, Math.min(Math.floor(char), m.total_chars))
  let lo = 0
  let hi = m.chunks.length
  while (lo < hi) {
    const mid = (lo + hi) >> 1
    if (chunkPrefix(m, mid) + m.chunks[mid].chars > target) hi = mid
    else lo = mid + 1
  }
  const chunk = Math.min(lo, m.chunks.length - 1)
  return { chunk, offset: target - chunkPrefix(m, chunk) }
}

// tocActiveIndex 返回当前块对应的目录高亮条目（最后一个 block <= 当前块的条目）。
export function tocActiveIndex(m: FlowManifest, block: number): number {
  let active = -1
  for (let i = 0; i < m.toc.length; i++) {
    if (m.toc[i].block <= block) active = i
    else break
  }
  return active
}

// totalBlocks 返回全书内容块总数（= 各章块数之和）。
export function totalBlocks(m: FlowManifest): number {
  let n = 0
  for (const sp of m.spines) n += sp.block_count
  return n
}

// anchorFromLegacy 把无 block 字段的旧锚点（path[0] = 章内块号）转成新格式。
// manifest 用于把章内块号换算为全书块号。
export function migrateAnchor(m: FlowManifest, legacy: { spine: number; path: number[]; offset: number }): ReadingAnchor {
  const spine = Math.min(Math.max(0, legacy.spine), Math.max(0, m.spines.length - 1))
  const base = m.spines[spine]?.block_start ?? 0
  const [first = 0, ...rest] = legacy.path
  return { spine, block: base + first, path: rest, offset: legacy.offset }
}
