import { describe, expect, it } from 'vitest'
import { PageCache, InFlight } from './cache'
import { buildProfile, computeMargins, FONT_FAMILY } from './prefs'

describe('PageCache', () => {
  it('LRU 容量裁剪', () => {
    const cache = new PageCache(3)
    cache.set('u0', 'a')
    cache.set('u1', 'b')
    cache.set('u2', 'c')
    expect(cache.has('u0')).toBe(true)
    cache.set('u3', 'd') // 逐出最旧的 u0
    expect(cache.has('u0')).toBe(false)
    expect(cache.has('u1')).toBe(true)
    cache.get('u1') // 刷新 u1
    cache.set('u4', 'e') // 逐出 u2
    expect(cache.has('u2')).toBe(false)
    expect(cache.has('u1')).toBe(true)
    expect(cache.get('u1')).toBe('b')
  })

  it('重复 set 刷新最近使用', () => {
    const cache = new PageCache(2)
    cache.set('u0', 'a')
    cache.set('u1', 'b')
    cache.set('u0', 'a2')
    cache.set('u2', 'c')
    expect(cache.has('u0')).toBe(true)
    expect(cache.has('u1')).toBe(false)
  })
})

describe('InFlight', () => {
  it('并发去重：同一页只发一次请求', async () => {
    const inFlight = new InFlight()
    let calls = 0
    const load = () =>
      new Promise<string>(resolve => {
        calls++
        setTimeout(() => resolve('html'), 5)
      })
    const [a, b] = await Promise.all([inFlight.run('u3', load), inFlight.run('u3', load)])
    expect(a).toBe('html')
    expect(b).toBe('html')
    expect(calls).toBe(1)
  })
})

describe('buildProfile / computeMargins', () => {
  it('viewport 与偏好决定全部排版参数，主题不参与', () => {
    const profile = buildProfile(390, 844, { fontSize: 21, lineHeight: 1.4, theme: 'dark' })
    expect(profile.viewport_w).toBe(390)
    expect(profile.viewport_h).toBe(844)
    expect(profile.font_size).toBe(21)
    expect(profile.line_height).toBe(1.4)
    expect(profile.font_family).toBe(FONT_FAMILY)
    expect(profile.margin_side).toBeGreaterThan(0)
    // 字号不同 → 参数不同（新 profile）
    const bigger = buildProfile(390, 844, { fontSize: 22, lineHeight: 1.4, theme: 'dark' })
    expect(bigger.font_size).not.toBe(profile.font_size)
  })

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
})
