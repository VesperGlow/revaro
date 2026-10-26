import { test, expect } from '@playwright/test'

const gameUrl = process.env.SIGLUS_REAL_GAME_URL

test('real save confirmation checkbox follows displayed pixels after resize and remembers its choice', async ({ page }, testInfo) => {
  test.skip(!gameUrl, 'Set SIGLUS_REAL_GAME_URL to a local Kinkoi share')
  test.setTimeout(180_000)
  expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(gameUrl!).hostname)
  await page.setViewportSize({ width: 1280, height: 720 })
  await page.addInitScript(() => {
    ;(window as any).__siglusProbeUi = true
    ;(window as any).__siglusProbeSprites = true
    Object.defineProperty(navigator, 'gpu', { configurable: true, value: undefined })
    const getContext = HTMLCanvasElement.prototype.getContext
    HTMLCanvasElement.prototype.getContext = function (kind: string, ...args: any[]): any {
      return ['webgl2', 'webgpu'].includes(kind) ? null : (getContext as any).call(this, kind, ...args)
    }
  })
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  const sprites = (fragment: string) => page.evaluate(fragment => JSON.parse((window as any).__siglusSprites || '[]')
    .filter((s: any) => s.visible && s.alpha > 0 && s.image.includes(fragment)), fragment)
  const checkbox = async () => (await sprites('_dialog_checkbox.g00'))[0]
  const checked = async () => {
    const sprite = await checkbox()
    if (!sprite) return undefined
    const frame = Number(sprite.image.match(/frame_index: Some\((\d+)\)/)?.[1])
    return frame >= 2
  }
  // Coordinates refer to the original artwork, and are mapped to the pixels
  // Canvas2D actually draws, independently of the engine's input transform.
  async function click(x: number, y: number) {
    const point = await page.locator('canvas').evaluate((canvas, point) => {
      const r = canvas.getBoundingClientRect()
      return { x: r.left + point.x * r.width / 1920, y: r.top + point.y * r.height / 1080 }
    }, { x, y })
    await page.mouse.move(point.x, point.y)
    await page.waitForTimeout(100)
    await page.mouse.click(point.x, point.y, { delay: 120 })
  }
  await page.goto(gameUrl!, { waitUntil: 'domcontentloaded' })
  await page.waitForFunction(() => (window as any).__siglusUi, {}, { timeout: 60_000 })
  await page.locator('canvas').click()
  await page.waitForFunction(() => JSON.parse((window as any).__siglusUi || '{}').text?.length, {}, { timeout: 60_000 })
  for (let i = 0; i < 180; i++) {
    if (await page.evaluate(() => (window as any).siglusProbe.voiceReads.length > 0
      && JSON.parse((window as any).__siglusUi || '{}').waiting)) break
    await page.keyboard.press('Space')
    await page.waitForTimeout(160)
  }
  expect(await page.evaluate(() => (window as any).siglusProbe.voiceReads.length)).toBeGreaterThan(0)
  await page.waitForTimeout(600)
  for (let attempt = 0; attempt < 3; attempt++) {
    await click(1080, 1055)
    await page.waitForTimeout(600)
    if ((await sprites('_storage_save_bg.g00')).length) break
  }
  await expect.poll(async () => (await sprites('_storage_save_bg.g00')).length).toBeGreaterThan(0)
  await click(285, 303)
  await expect.poll(() => page.evaluate(() => (window as any).siglusProbe.saveWrites
    .filter((w: any) => w.path === 'savedata/0000.sav').length)).toBeGreaterThan(0)
  await click(285, 303)
  await expect.poll(checked).toBe(false)
  const observations = []
  for (const viewport of [{ width: 2549, height: 1242 }, { width: 900, height: 1000 }, { width: 1280, height: 720 }]) {
    await page.setViewportSize(viewport)
    await expect.poll(() => page.locator('canvas').evaluate((c: HTMLCanvasElement) => c.width)).toBe(viewport.width)
    const sprite = await checkbox()
    const square = { x: sprite.position[0] - sprite.texture_center[0] + 8, y: sprite.position[1] }
    await click(square.x, square.y)
    await expect.poll(checked, 'click the displayed small square, including on a wide canvas').toBe(true)
    const ui = await page.evaluate(() => JSON.parse((window as any).__siglusUi))
    expect(ui.viewport).toEqual([0, 0, viewport.width, viewport.height])
    expect(Math.abs(ui.mouse[0] - square.x)).toBeLessThanOrEqual(2)
    expect(Math.abs(ui.mouse[1] - square.y)).toBeLessThanOrEqual(2)
    observations.push({ viewport, square, ui })
    await click(square.x + 70, square.y)
    await expect.poll(checked, 'the checkbox caption must toggle the same preference').toBe(false)
  }
  const sprite = await checkbox()
  await click(sprite.position[0] - sprite.texture_center[0] + 8, sprite.position[1])
  await expect.poll(checked).toBe(true)
  await click(895, 518)
  await expect.poll(async () => (await sprites('_dialog_checkbox.g00')).length).toBe(0)
  const writes = await page.evaluate(() => (window as any).siglusProbe.saveWrites.filter((w: any) => w.path === 'savedata/0000.sav').length)
  await click(285, 303)
  await expect.poll(() => page.evaluate(() => (window as any).siglusProbe.saveWrites
    .filter((w: any) => w.path === 'savedata/0000.sav').length)).toBeGreaterThan(writes)
  expect(await checkbox(), 'the next overwrite must skip the disabled confirmation').toBeUndefined()
  // LOAD has its own game-script flag. Verify the same visible target there
  // instead of assuming that disabling SAVE confirmation disabled both.
  await click(1545, 1055)
  await expect.poll(async () => (await sprites('_storage_load_bg.g00')).length).toBeGreaterThan(0)
  await click(285, 303)
  await expect.poll(checked, 'load confirmation remains enabled independently').toBe(false)
  await page.setViewportSize({ width: 2549, height: 1242 })
  await expect.poll(() => page.locator('canvas').evaluate((c: HTMLCanvasElement) => c.width)).toBe(2549)
  const loadCheckbox = await checkbox()
  await click(loadCheckbox.position[0] - loadCheckbox.texture_center[0] + 8, loadCheckbox.position[1])
  await expect.poll(checked).toBe(true)
  const reads = await page.evaluate(() => (window as any).siglusProbe.overlayReads.filter((p: string) => p === 'savedata/0000.sav').length)
  await click(895, 518)
  await expect.poll(async () => (await sprites('_dialog_checkbox.g00')).length).toBe(0)
  await expect.poll(() => page.evaluate(() => (window as any).siglusProbe.overlayReads
    .filter((p: string) => p === 'savedata/0000.sav').length)).toBeGreaterThan(reads)
  await expect.poll(async () => (await sprites('_storage_load_bg.g00')).length).toBe(0)
  expect(errors).toEqual([])
  await testInfo.attach('confirmation-resize-checks', { body: JSON.stringify(observations, null, 2), contentType: 'application/json' })
  await testInfo.attach('save-menu', { body: await page.screenshot(), contentType: 'image/png' })
})
