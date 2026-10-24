import { test, expect } from '@playwright/test'

// Requires a real, legally supplied game already shared by Revaro.
// No synthetic XP3 can establish compatibility. The sample-scene clicks below
// belong to this opt-in verification fixture, never the player implementation.
const gameUrl = process.env.E2E_GALGAME_URL
test.skip(!gameUrl, 'Set E2E_GALGAME_URL to run a real-game compatibility check')
if (gameUrl && (!process.env.E2E_BROWSER || process.env.E2E_BROWSER === 'chromium')) {
  test.use({ launchOptions: {
    executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH,
    args: ['--use-angle=swiftshader', '--enable-unsafe-swiftshader'],
  } })
}

test('native game input, modal geometry, log scrolling and save reload', async ({ browserName, browser }) => {
  test.skip(browserName !== 'chromium' || !gameUrl, 'Set E2E_GALGAME_URL to the local real-game share')
  test.setTimeout(180_000)
  const context = await browser.newContext({ viewport: { width: 1280, height: 720 } })
  const page = await context.newPage()
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  try {
    await page.goto(gameUrl!)
    await page.waitForFunction(() => (window as any).RevaroPlayer?.snapshot()?.width > 0)
    await page.waitForTimeout(9000)
    // Enter chapter 1 using the real game's title and reader preview.
    await page.mouse.click(1080, 202)
    await page.waitForTimeout(5000)
    await page.mouse.click(640, 270)
    await page.waitForTimeout(10_000)
    await page.mouse.click(640, 400)
    await page.waitForTimeout(6000)
    await page.screenshot({ path: test.info().outputPath('story.png') })
    // Native wheel opens the log; native right returns from it.
    await page.mouse.move(640, 400)
    await page.mouse.wheel(0, -80)
    await expect.poll(() => page.evaluate(() => (window as any).RevaroPlayer.snapshot().wheel)).toBeGreaterThan(0)
    await page.waitForTimeout(500)
    await page.screenshot({ path: test.info().outputPath('log.png') })
    await page.mouse.click(640, 400, { button: 'right' })
    await page.waitForTimeout(500)
    // Save menu and a genuine native modal.
    await page.mouse.click(770, 572)
    await page.waitForTimeout(700)
    await page.mouse.click(180, 170)
    const modal = page.locator('#revaro-modal')
    await expect(modal).toBeVisible()
    const panel = page.locator('#revaro-modal-panel')
    const box = (await panel.boundingBox())!
    expect(Math.abs(box.x + box.width / 2 - 640)).toBeLessThan(1)
    expect(Math.abs(box.y + box.height / 2 - 360)).toBeLessThan(1)
    expect(box.width).toBeGreaterThan(573)
    const logical = await page.evaluate(() => (window as any).RevaroPlayer.snapshot())
    expect(box.width / box.height).toBeCloseTo(logical.width / logical.height, 2)
    await page.screenshot({ path: test.info().outputPath('save-confirm.png') })
    await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2, { button: 'right' })
    await expect(modal).toBeHidden()
    expect(await page.evaluate(() => (window as any).RevaroPlayer.snapshot().cancel)).toBe(1)
    // Confirm using the remapped original buttons, then validate browser saves survive reload.
    await page.mouse.click(180, 170)
    await expect(modal).toBeVisible()
    const confirm = (await panel.boundingBox())!
    await page.mouse.click(confirm.x + confirm.width * 0.345, confirm.y + confirm.height * 0.675)
    await expect(modal).toBeHidden()
    await page.waitForTimeout(700)
    const space = await page.evaluate(() => (window as any).Module._saveSpaceId)
    const saved = await page.evaluate(async () => (await (window as any).idbLoadAll()).map((f: any) => f.path))
    expect(saved.some((p: string) => p.endsWith('.bmp'))).toBeTruthy()
    await page.reload()
    await page.waitForFunction(() => (window as any).RevaroPlayer?.snapshot()?.width > 0)
    await page.waitForTimeout(9000)
    expect(await page.evaluate(() => (window as any).Module._saveSpaceId)).toBe(space)
    await page.mouse.click(1080, 255)
    await page.waitForTimeout(700)
    await page.mouse.click(180, 170)
    await expect(modal).toBeVisible()
    // Verify native VK_RETURN delivery. The game decides whether it confirms a modal.
    await page.keyboard.down('Enter')
    await expect.poll(() => page.evaluate(() => (window as any).HEAPU8[5857536 + 13] & 1)).toBe(1)
    await page.keyboard.up('Enter')
    await expect.poll(() => page.evaluate(() => (window as any).HEAPU8[5857536 + 13] & 1)).toBe(0)
    if (await modal.isVisible()) {
      const load = (await panel.boundingBox())!
      await page.mouse.click(load.x + load.width * 0.345, load.y + load.height * 0.675)
    }
    await expect(modal).toBeHidden()
    await page.waitForTimeout(1500)
    await page.screenshot({ path: test.info().outputPath('restored-story.png') })
    // Repeated Escape cannot close two UI levels on one physical press.
    await page.mouse.click(770, 572)
    await page.waitForTimeout(700)
    await page.mouse.click(180, 170)
    await expect(modal).toBeVisible()
    const accepted = await page.evaluate(() => (window as any).RevaroPlayer.snapshot().keyDown)
    await page.keyboard.down('Escape')
    await page.keyboard.down('Escape')
    await page.keyboard.up('Escape')
    await expect(modal).toBeHidden()
    expect(await page.evaluate(() => (window as any).RevaroPlayer.snapshot().keyDown)).toBe(accepted + 1)
    await page.keyboard.down('Control')
    await expect.poll(() => page.evaluate(() => (window as any).HEAPU8[5857536 + 17] & 1)).toBe(1)
    await page.evaluate(() => window.dispatchEvent(new Event('blur')))
    await expect.poll(() => page.evaluate(() => (window as any).HEAPU8[5857536 + 17] & 1)).toBe(0)
    expect(await page.evaluate(() => (window as any).RevaroPlayer.snapshot().heldKeys)).toBe(0)
    expect(errors).toEqual([])
  } finally { await context.close() }
})

test('real-game touch taps, modal hit testing and long-press cancel at DPR 2', async ({ browserName, browser }) => {
  test.skip(browserName !== 'chromium' || !gameUrl, 'Requires the real-game Chromium fixture')
  test.setTimeout(100_000)
  const context = await browser.newContext({ viewport: { width: 844, height: 390 }, deviceScaleFactor: 2, hasTouch: true, isMobile: true })
  const page = await context.newPage()
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  async function tap(x: number, y: number) {
    // Coordinates describe only this opt-in sample's 1280x720 artwork.
    const point = await page.evaluate(({ x, y }) => {
      const r = document.getElementById('canvas')!.getBoundingClientRect()
      const scale = Math.min(r.width / 1280, r.height / 720)
      return { x: r.left + (r.width - 1280 * scale) / 2 + x * scale, y: r.top + (r.height - 720 * scale) / 2 + y * scale }
    }, { x, y })
    await page.touchscreen.tap(point.x, point.y)
  }
  try {
    await page.goto(gameUrl!)
    await page.waitForFunction(() => (window as any).RevaroPlayer?.snapshot()?.width > 0)
    await page.waitForTimeout(9000)
    await tap(1080, 202)
    await page.waitForTimeout(8000)
    await tap(770, 572)
    await page.waitForTimeout(800)
    await tap(180, 170)
    const modal = page.locator('#revaro-modal')
    await expect(modal).toBeVisible()
    const panel = page.locator('#revaro-modal-panel')
    const box = (await panel.boundingBox())!
    expect(Math.abs(box.x + box.width / 2 - 422)).toBeLessThan(1)
    expect(Math.abs(box.y + box.height / 2 - 195)).toBeLessThan(1)
    expect(box.x).toBeGreaterThanOrEqual(0)
    expect(box.x + box.width).toBeLessThanOrEqual(844)
    expect(await page.evaluate(() => (document.getElementById('canvas') as HTMLCanvasElement).width)).toBe(1688)
    await page.screenshot({ path: test.info().outputPath('touch-confirm.png') })
    await page.touchscreen.tap(box.x + box.width * 0.66, box.y + box.height * 0.675)
    await expect(modal).toBeHidden()
    await tap(180, 170)
    await expect(modal).toBeVisible()
    const again = (await panel.boundingBox())!
    const cdp = await context.newCDPSession(page)
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: again.x + again.width / 2, y: again.y + again.height / 2 }] })
    await page.waitForTimeout(600)
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await expect(modal).toBeHidden()
    expect(await page.evaluate(() => (window as any).RevaroPlayer.snapshot().cancel)).toBe(1)
    await page.getByRole('button', { name: '返回', exact: true }).click()
    await page.waitForTimeout(600)
    await page.getByRole('button', { name: '日志', exact: true }).click()
    await expect.poll(() => page.evaluate(() => (window as any).RevaroPlayer.snapshot().wheel)).toBeGreaterThan(0)
    await page.screenshot({ path: test.info().outputPath('touch-log.png') })
    expect(errors).toEqual([])
  } finally { await context.close() }
})
