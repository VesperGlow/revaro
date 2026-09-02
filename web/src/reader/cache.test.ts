import { describe, expect, it } from 'vitest'
import { PageCache, InFlight } from './cache'
import { clamp, computeMargins, FONT_MIN, FONT_MAX } from './prefs'

describe('PageCache', () => {
  it('LRU 容量裁剪（chunk 序号键）', () => {
    const cache = new PageCache(3)
    cache.set(0, 'a')
    cache.set(1, 'b')
    cache.set(2, 'c')
    expect(cache.has(0)).toBe(true)
    cache.set(3, 'd') // 逐出最旧的 0
    expect(cache.has(0)).toBe(false)
    expect(cache.has(1)).toBe(true)
    cache.get(1) // 刷新
    cache.set(4, 'e') // 逐出 2
    expect(cache.has(2)).toBe(false)
    expect(cache.has(1)).toBe(true)
    expect(cache.get(1)).toBe('b')
  })

  it('重复 set 刷新最近使用', () => {
    const cache = new PageCache(2)
    cache.set(0, 'a')
    cache.set(1, 'b')
    cache.set(0, 'a2')
    cache.set(2, 'c')
    expect(cache.has(0)).toBe(true)
    expect(cache.has(1)).toBe(false)
  })
})

describe('InFlight', () => {
  it('并发去重：同一 chunk 只发一次请求', async () => {
    const inFlight = new InFlight()
    let calls = 0
    const load = () =>
      new Promise<string>(resolve => {
        calls++
        setTimeout(() => resolve('html'), 5)
      })
    const [a, b] = await Promise.all([inFlight.run(3, load), inFlight.run(3, load)])
    expect(a).toBe('html')
    expect(b).toBe('html')
    expect(calls).toBe(1)
  })
})

describe('computeMargins / clamp', () => {
  it('桌面宽屏限制栏宽（与旧客户端同源）', () => {
    const margins = computeMargins(1400, 900)
    expect(margins.top).toBe(60)
    expect(margins.bottom).toBe(24)
    expect(1400 - 2 * margins.side).toBe(720)
  })

  it('移动端边距按高度收紧', () => {
    const margins = computeMargins(390, 600)
    expect(margins.top).toBe(16)
    expect(margins.bottom).toBe(12)
  })

  it('clamp 与字号边界', () => {
    expect(clamp(5, FONT_MIN, FONT_MAX)).toBe(FONT_MIN)
    expect(clamp(99, FONT_MIN, FONT_MAX)).toBe(FONT_MAX)
    expect(clamp(20, FONT_MIN, FONT_MAX)).toBe(20)
  })
})
