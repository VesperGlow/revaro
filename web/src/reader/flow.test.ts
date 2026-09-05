import { describe, expect, it } from 'vitest'
import type { FlowManifest, ReadingAnchor } from './types'
import { compareAnchor, spineForBlock, chunkForBlock, chunkPrefix, locateChar, spineOriginChunk, stableWindowRange, tocActiveIndex, totalBlocks } from './flow'

function anchor(spine: number, block: number, path: number[], offset: number): ReadingAnchor {
  return { spine, block, path, offset }
}

const manifest: FlowManifest = {
  version: 1,
  format: 'epub',
  total_chars: 1000,
  spines: [
    { block_start: 0, block_count: 10 },
    { block_start: 10, block_count: 5 },
    { block_start: 15, block_count: 2 },
  ],
  chunks: [
    { index: 0, block_start: 0, block_count: 7, chars: 300 },
    { index: 1, block_start: 7, block_count: 8, chars: 500 },
    { index: 2, block_start: 15, block_count: 2, chars: 200 },
  ],
  toc: [
    { label: '一', depth: 0, spine: 0, block: 0 },
    { label: '二', depth: 0, spine: 1, block: 10 },
    { label: '三', depth: 1, spine: 2, block: 15 },
  ],
}

describe('compareAnchor', () => {
  it('与 Go 端全序一致（spine → block → path → offset）', () => {
    const ordered: ReadingAnchor[] = [
      anchor(0, 0, [], -1),
      anchor(0, 1, [], -1),
      anchor(0, 1, [0], -1),
      anchor(0, 1, [0], 0),
      anchor(0, 1, [0], 7),
      anchor(0, 2, [], -1),
      anchor(1, 0, [], -1),
    ]
    for (let i = 0; i < ordered.length; i++) {
      expect(compareAnchor(ordered[i], ordered[i])).toBe(0)
      for (let j = i + 1; j < ordered.length; j++) {
        expect(compareAnchor(ordered[i], ordered[j])).toBeLessThan(0)
        expect(compareAnchor(ordered[j], ordered[i])).toBeGreaterThan(0)
      }
    }
  })
})

describe('flow manifest lookups', () => {
  it('spineForBlock / chunkForBlock / totalBlocks', () => {
    expect(spineForBlock(manifest, 0)).toBe(0)
    expect(spineForBlock(manifest, 9)).toBe(0)
    expect(spineForBlock(manifest, 10)).toBe(1)
    expect(spineForBlock(manifest, 16)).toBe(2)
    expect(chunkForBlock(manifest, 0)).toBe(0)
    expect(chunkForBlock(manifest, 6)).toBe(0)
    expect(chunkForBlock(manifest, 7)).toBe(1)
    expect(chunkForBlock(manifest, 14)).toBe(1)
    expect(chunkForBlock(manifest, 15)).toBe(2)
    expect(chunkForBlock(manifest, 99)).toBe(-1)
    expect(totalBlocks(manifest)).toBe(17)
  })

  it('chunkPrefix / locateChar', () => {
    expect(chunkPrefix(manifest, 0)).toBe(0)
    expect(chunkPrefix(manifest, 1)).toBe(300)
    expect(chunkPrefix(manifest, 2)).toBe(800)
    expect(locateChar(manifest, 0)).toEqual({ chunk: 0, offset: 0 })
    expect(locateChar(manifest, 299)).toEqual({ chunk: 0, offset: 299 })
    expect(locateChar(manifest, 300)).toEqual({ chunk: 1, offset: 0 })
    expect(locateChar(manifest, 799)).toEqual({ chunk: 1, offset: 499 })
    expect(locateChar(manifest, 800)).toEqual({ chunk: 2, offset: 0 })
    expect(locateChar(manifest, 99999).chunk).toBe(2)
    expect(locateChar(manifest, -5).chunk).toBe(0)
  })

  it('tocActiveIndex 高亮当前块所属目录条目', () => {
    expect(tocActiveIndex(manifest, 0)).toBe(0)
    expect(tocActiveIndex(manifest, 9)).toBe(0)
    expect(tocActiveIndex(manifest, 10)).toBe(1)
    expect(tocActiveIndex(manifest, 15)).toBe(2)
  })

})

describe('稳定分页边界（窗口虚拟化不允许从任意 chunk 起始）', () => {
  // chunk 可能跨 spine：上面的 manifest 里 chunk 1 覆盖块 7..14，
  // 而 spine 1 从块 10 开始 → spine 1 的稳定边界 chunk 是 1。
  it('spineOriginChunk 返回包含 spine 起始块的 chunk', () => {
    expect(spineOriginChunk(manifest, 0)).toBe(0)
    expect(spineOriginChunk(manifest, 9)).toBe(0)
    expect(spineOriginChunk(manifest, 10)).toBe(1) // spine 1 起始块在 chunk 1 内
    expect(spineOriginChunk(manifest, 14)).toBe(1)
    expect(spineOriginChunk(manifest, 15)).toBe(2)
  })

  it('stableWindowRange 保留 spine 排版前缀并预取 ahead', () => {
    // 阅读位置在 spine 1 中部（块 12 → chunk 1）：窗口 = [边界 1, 1+2 收敛到书尾]
    expect(stableWindowRange(manifest, 12, 2)).toEqual([1, 2])
    // 越过书尾收敛
    expect(stableWindowRange(manifest, 16, 5)).toEqual([2, 2])
    // spine 0：窗口起点始终是 chunk 0
    expect(stableWindowRange(manifest, 5, 2)).toEqual([0, 2])
    // 未知块回退到窗口 [0, ahead]
    expect(stableWindowRange(manifest, 99, 2)).toEqual([0, 2])
  })

  it('同一 spine 内推进阅读位置时窗口起点不回退（相位稳定前提）', () => {
    let prevLo = -1
    for (const block of [10, 11, 12, 13, 14]) {
      const [lo] = stableWindowRange(manifest, block, 3)
      expect(lo).toBe(1)
      expect(lo).toBeGreaterThanOrEqual(prevLo)
      prevLo = lo
    }
  })
})
