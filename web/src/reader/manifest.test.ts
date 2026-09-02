import { describe, expect, it } from 'vitest'
import type { LayoutManifest, ReadingAnchor } from './types'
import { compareAnchor, pageForAnchor, anchorForPage, clampPage, tocActiveIndex } from './manifest'

function anchor(spine: number, path: number[], offset: number): ReadingAnchor {
  return { spine, path, offset }
}

function page(start: ReadingAnchor): { index: number; spine: number; start: ReadingAnchor; end: ReadingAnchor } {
  return { index: 0, spine: start.spine, start, end: start }
}

describe('compareAnchor', () => {
  it('总序与 Go 端一致', () => {
    const ordered: ReadingAnchor[] = [
      anchor(0, [0], -1),
      anchor(0, [0], 0),
      anchor(0, [0], 5),
      anchor(0, [0, 0], 0),
      anchor(0, [0, 1], 0),
      anchor(0, [1], -1),
      anchor(1, [], -1),
      anchor(1, [0], 0),
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

describe('pageForAnchor', () => {
  const m: LayoutManifest = {
    version: 1,
    book_hash: 'h',
    profile_id: 'p',
    profile: {
      viewport_w: 400, viewport_h: 500, font_size: 16,
      font_family: 'Revaro Serif', line_height: 1.6,
      margin_top: 10, margin_bottom: 10, margin_side: 10,
    },
    page_count: 3,
    pages: [
      { ...page(anchor(0, [0], 0)), index: 0, end: anchor(0, [1], 0) },
      { ...page(anchor(0, [1], 0)), index: 1, end: anchor(1, [0], 0) },
      { ...page(anchor(1, [0], 0)), index: 2, end: anchor(2, [0], 0) },
    ],
    toc: [],
  }

  it('锚点落在 [start, end) 区间内', () => {
    expect(pageForAnchor(m, anchor(0, [0], 0))).toBe(0)
    expect(pageForAnchor(m, anchor(0, [0], 99))).toBe(0)
    expect(pageForAnchor(m, anchor(0, [1], 0))).toBe(1) // 页边界归属下一页
    expect(pageForAnchor(m, anchor(0, [2], 0))).toBe(1)
    expect(pageForAnchor(m, anchor(1, [0], 0))).toBe(2)
    expect(pageForAnchor(m, anchor(3, [0], 0))).toBe(2) // 书末之后 → 最后一页
  })

  it('空 manifest 与书首锚点', () => {
    const empty: LayoutManifest = { ...m, pages: [], page_count: 0 }
    expect(pageForAnchor(empty, anchor(0, [0], 0))).toBe(0)
  })

  it('anchorForPage 与 clampPage', () => {
    expect(anchorForPage(m, 1)).toEqual(anchor(0, [1], 0))
    expect(anchorForPage(m, 9)).toBeNull()
    expect(clampPage(m, -5)).toBe(0)
    expect(clampPage(m, 99)).toBe(2)
  })
})

describe('tocActiveIndex', () => {
  const m: LayoutManifest = {
    version: 1,
    book_hash: 'h',
    profile_id: 'p',
    profile: { viewport_w: 1, viewport_h: 1, font_size: 16, font_family: 'f', line_height: 1.6, margin_top: 0, margin_bottom: 0, margin_side: 0 },
    page_count: 10,
    pages: [],
    toc: [
      { label: '一', page: 0, depth: 0 },
      { label: '二', page: 3, depth: 0 },
      { label: '三', page: 7, depth: 1 },
    ],
  }
  it('返回最后一个 page <= 当前页的条目', () => {
    expect(tocActiveIndex(m, 0)).toBe(0)
    expect(tocActiveIndex(m, 2)).toBe(0)
    expect(tocActiveIndex(m, 3)).toBe(1)
    expect(tocActiveIndex(m, 9)).toBe(2)
  })
})
