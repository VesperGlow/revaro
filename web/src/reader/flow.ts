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

// spineOriginChunk 返回 block 所属 spine 的稳定分页边界 chunk（spine 起始
// 块所在 chunk）。chunk 按体量切分、可能跨 spine，因此边界是「包含 spine
// 起始块的 chunk」；chunk 内容固定 ⇒ 排版原点固定。
export function spineOriginChunk(m: FlowManifest, block: number): number {
  const spine = spineForBlock(m, block)
  const start = m.spines[spine]?.block_start ?? 0
  const origin = chunkForBlock(m, start)
  return origin < 0 ? 0 : origin
}

// stableWindowRange 计算以 block（阅读位置所在块）为中心的稳定加载窗口：
// 窗口起点必须是 spine 稳定分页边界 chunk（保留排版前缀），终点是当前
// chunk + ahead 预取。CSS columns 以窗口首 chunk 为排版原点——不允许从
// 任意 chunk 起始，否则窗口增删会改变 page boundary（相位漂移）。
export function stableWindowRange(m: FlowManifest, block: number, ahead: number): [number, number] {
  const n = m.chunks.length
  if (n === 0) return [0, 0]
  const cb = chunkForBlock(m, block)
  if (cb < 0) return [0, Math.min(ahead, n - 1)]
  const lo = spineOriginChunk(m, block)
  const hi = Math.max(lo, Math.min(cb + ahead, n - 1))
  return [lo, hi]
}
