import { test, expect } from '@playwright/test'
import { mkdir, writeFile } from 'node:fs/promises'
import { dirname } from 'node:path'

const gameUrl = process.env.SIGLUS_REAL_GAME_URL

test('composed portraits use their original height and occupy the upper scene', async ({ page }, testInfo) => {
  test.skip(!gameUrl, 'Set SIGLUS_REAL_GAME_URL to a local Kinkoi share')
  test.setTimeout(180_000)
  expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(gameUrl!).hostname)
  await page.setViewportSize({ width: 1280, height: 720 })
  await page.addInitScript(() => {
    ;(window as any).__siglusProbeUi = true
    ;(window as any).__siglusProbeSprites = true
    Object.defineProperty(navigator, 'gpu', { configurable: true, value: undefined })
    const getContext = HTMLCanvasElement.prototype.getContext
    HTMLCanvasElement.prototype.getContext = function (type: string, ...args: any[]): any {
      return ['webgl2', 'webgpu'].includes(type) ? null : (getContext as any).call(this, type, ...args)
    }
  })
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await page.goto(gameUrl!, { waitUntil: 'domcontentloaded' })
  await page.waitForFunction(() => (window as any).__siglusUi, {}, { timeout: 60_000 })
  await page.locator('canvas').click()
  await page.waitForFunction(() => JSON.parse((window as any).__siglusUi || '{}').text?.length, {}, { timeout: 60_000 })
  let found = false
  for (let i = 0; i < 180; i++) {
    const state = await page.evaluate(() => {
      const w = window as any
      const sprites = JSON.parse(w.__siglusSprites || '[]')
      const portrait = sprites.find((s: any) => s.visible && s.alpha === 255 && /bs\d_.*_base/.test(s.image))
      const ui = JSON.parse(w.__siglusUi || '{}')
      if (!portrait && ui.waiting && ui.text && sprites.some((s: any) => s.image.includes('bg10a.g00'))) {
        w.__portraitBackground = w.__siglusSoftwareFrame.slice()
      }
      return { portrait, ui }
    })
    if (state.portrait && state.ui.waiting && state.ui.text) { found = true; break }
    await page.keyboard.press('Space')
    await page.waitForTimeout(160)
  }
  expect(found, 'reach the first standing portrait in the real prologue').toBe(true)
  await page.waitForTimeout(500)
  const result = await page.evaluate(() => {
    const w = window as any
    const sprites = JSON.parse(w.__siglusSprites || '[]')
    const portraits = sprites.filter((s: any) => s.visible && /bs\d_.*_base/.test(s.image))
    const [width, height] = w.__siglusSoftwareSize
    const frame = w.__siglusSoftwareFrame, background = w.__portraitBackground
    let changed = 0, count = 0
    if (background) for (let y = Math.floor(height * .15); y < height * .6; y += 4) {
      for (let x = Math.floor(width * .25); x < width * .75; x += 4) {
        const i = (y * width + x) * 4
        if (Math.abs(frame[i] - background[i]) + Math.abs(frame[i + 1] - background[i + 1]) + Math.abs(frame[i + 2] - background[i + 2]) > 40) changed++
        count++
      }
    }
    return { portraits, upperSceneChanged: count ? changed / count : 0, status: JSON.parse(w.__siglusUi).status }
  })
  const output = testInfo.outputPath('portrait-layout.json')
  await mkdir(dirname(output), { recursive: true })
  await writeFile(output, JSON.stringify(result, null, 2))
  await page.screenshot({ path: testInfo.outputPath('standing-portrait.png') })
  expect(result.portraits.length).toBeGreaterThan(0)
  for (const portrait of result.portraits) expect(portrait.position[1], 'script height lookup must not return zero').toBeLessThan(0)
  expect(result.upperSceneChanged, 'the actual portrait pixels must cover the upper scene').toBeGreaterThan(.15)
  // A different portrait range has a different original height. Keep checking
  // after the first shot so a fixed offset cannot make this regression pass.
  let medium: any
  for (let i = 0; i < 100; i++) {
    const state = await page.evaluate(() => ({
      portrait: JSON.parse((window as any).__siglusSprites || '[]').find((s: any) => s.visible && /bs2_.*_base/.test(s.image)),
      ui: JSON.parse((window as any).__siglusUi || '{}'),
    }))
    if (state.portrait && state.ui.waiting && state.ui.text) { medium = state.portrait; break }
    await page.keyboard.press('Space')
    await page.waitForTimeout(160)
  }
  expect(medium, 'reach the next portrait range').toBeTruthy()
  // Wipe/position events can still be animating while the previous line is
  // waiting. Check the settled geometry rather than an intermediate frame.
  await expect.poll(() => page.evaluate(() => JSON.parse((window as any).__siglusSprites || '[]')
    .find((s: any) => s.visible && /bs2_.*_base/.test(s.image))?.position[1])).toBe(-225)
  medium = await page.evaluate(() => JSON.parse((window as any).__siglusSprites || '[]')
    .find((s: any) => s.visible && /bs2_.*_base/.test(s.image)))
  expect(medium.position[1]).toBe(-225)
  expect(medium.position[1]).not.toBe(result.portraits[0].position[1])
  await page.setViewportSize({ width: 2549, height: 1242 })
  await expect.poll(() => page.locator('canvas').evaluate((c: HTMLCanvasElement) => c.width)).toBe(2549)
  await page.waitForTimeout(500)
  await page.screenshot({ path: testInfo.outputPath('medium-portrait-resized.png') })
  await writeFile(testInfo.outputPath('medium-portrait.json'), JSON.stringify(medium, null, 2))
  await expect(page.getByRole('alert')).toBeHidden()
  expect(errors).toEqual([])
})
