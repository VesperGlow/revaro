import { test, expect } from '@playwright/test'
import { mkdir, writeFile } from 'node:fs/promises'
import { dirname } from 'node:path'

const gameUrl = process.env.SIGLUS_REAL_GAME_URL

test('packaged CJK font supplies missing simplified glyphs in software playback', async ({ page }, testInfo) => {
  test.skip(!gameUrl, 'Set SIGLUS_REAL_GAME_URL to a local game share')
  test.setTimeout(120_000)
  expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(gameUrl!).hostname)
  await page.addInitScript(() => {
    ;(window as any).__siglusProbeUi = true
    Object.defineProperty(navigator, 'gpu', { configurable: true, value: undefined })
    const getContext = HTMLCanvasElement.prototype.getContext
    HTMLCanvasElement.prototype.getContext = function (type: string, ...args: any[]): any {
      return ['webgl2', 'webgpu'].includes(type) ? null : (getContext as any).call(this, type, ...args)
    }
  })
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await page.goto(gameUrl!, { waitUntil: 'domcontentloaded' })
  await page.waitForFunction(() => (window as any).__siglusUi || (window as any).siglusProbe?.phase === 'failed', {}, { timeout: 60_000 })
  await expect(page.getByRole('alert')).toBeHidden()
  const coverage = await page.evaluate(async () => {
    const engine = await import(/* @vite-ignore */ '/galgame/siglus/pkg/game_launcher.js')
    return JSON.parse(engine.siglus_wasm_font_probe('谢这让们够见灿烂辉欢纯简贯彻金あA「'))
  })
  const fallback = coverage.glyphs.filter((glyph: any) => glyph.primary_glyph === 0)
  expect(fallback.length).toBeGreaterThan(10)
  for (const glyph of fallback) {
    expect(glyph.resolved_glyph, glyph.character).toBeGreaterThan(0)
    expect(glyph.ink_pixels, glyph.character).toBeGreaterThan(0)
    expect(glyph.source, glyph.character).toMatch(/dat\/font03\.otf$/)
  }
  expect(new Set(fallback.map((glyph: any) => glyph.bitmap_hash)).size).toBe(fallback.length)
  for (const character of ['金', 'あ', 'A', '「']) {
    const glyph = coverage.glyphs.find((glyph: any) => glyph.character === character)
    expect(glyph.primary_glyph).toBeGreaterThan(0)
    expect(glyph.resolved_glyph).toBe(glyph.primary_glyph)
    expect(glyph.source).toBe(coverage.primary)
  }
  await page.locator('canvas').click()
  await page.waitForFunction(() => JSON.parse((window as any).__siglusUi || '{}').text?.length || (window as any).siglusProbe?.phase === 'failed', {}, { timeout: 60_000 })
  for (let i = 0; i < 12; i++) {
    const text = await page.evaluate(() => JSON.parse((window as any).__siglusUi || '{}').text || '')
    if (/[这谢]/.test(text)) break
    await page.keyboard.press('Space')
    await page.waitForTimeout(500)
  }
  const state = await page.evaluate(() => ({
    phase: (window as any).siglusProbe.phase,
    ui: JSON.parse((window as any).__siglusUi || '{}'),
    reads: (window as any).siglusProbe.reads.map((entry: any) => entry.path),
  }))
  expect(state.ui.text).toMatch(/[这谢]/)
  expect(state.reads).toContain('dat/font03.otf')
  expect(state.phase).toBe('running')
  await expect(page.getByRole('alert')).toBeHidden()
  expect(errors).toEqual([])
  const output = testInfo.outputPath('font-fallback.json')
  await mkdir(dirname(output), { recursive: true })
  await writeFile(output, JSON.stringify({ coverage, state, errors }, null, 2))
  await page.screenshot({ path: testInfo.outputPath('chinese-dialogue.png') })
  console.log(JSON.stringify({ fallbackCharacters: fallback.map((glyph: any) => glyph.character).join(''), text: state.ui.text, errors }))
})
