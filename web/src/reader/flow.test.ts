import { describe, expect, it } from 'vitest'
import type { FlowManifest, ReadingAnchor } from './types'
import { compareAnchor, spineForBlock, chunkForBlock, chunkPrefix, locateChar, tocActiveIndex, totalBlocks, migrateAnchor } from './flow'

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

  it('migrateAnchor：旧锚点 path[0] = 章内块号 → 全书块号', () => {
    const legacy = migrateAnchor(manifest, { spine: 1, path: [2, 0], offset: 7 })
    expect(legacy).toEqual({ spine: 1, block: 12, path: [0], offset: 7 })
    const first = migrateAnchor(manifest, { spine: 2, path: [], offset: -1 })
    expect(first.block).toBe(15)
  })
})
