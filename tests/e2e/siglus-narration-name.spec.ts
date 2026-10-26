import { test, expect } from '@playwright/test'

const gameUrl = process.env.SIGLUS_REAL_GAME_URL

test('narration clears projected speaker glyphs and the next speaker returns', async ({ page }, testInfo) => {
  test.skip(!gameUrl, 'Set SIGLUS_REAL_GAME_URL to a local Kinkoi share')
  test.setTimeout(180_000)
  expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(gameUrl!).hostname)
  await page.addInitScript(() => {
    ;(window as any).__siglusProbeUi = true
    Object.defineProperty(navigator, 'gpu', { configurable: true, value: undefined })
    const getContext = HTMLCanvasElement.prototype.getContext
    HTMLCanvasElement.prototype.getContext = function (kind: string, ...args: any[]): any {
      return ['webgl2', 'webgpu'].includes(kind) ? null : (getContext as any).call(this, kind, ...args)
    }
  })
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await page.goto(gameUrl!, { waitUntil: 'domcontentloaded' })
  await page.waitForFunction(() => (window as any).__siglusUi, {}, { timeout: 60_000 })
  await page.locator('canvas').click()
  await page.waitForFunction(() => JSON.parse((window as any).__siglusUi || '{}').text?.length, {}, { timeout: 60_000 })
  const transitions: any[] = []
  let previous: any, cleared = 0, returned = 0
  for (let input = 0; input < 300; input++) {
    const current = await page.evaluate(() => JSON.parse((window as any).__siglusUi || '{}'))
    if (current.waiting && current.text) {
      expect(typeof current.name).toBe('string')
      expect(typeof current.name_glyphs).toBe('string')
      if (!current.name) {
        // A blank name string alone is insufficient: stale projected glyphs
        // can still be rasterized and shown above a narration line.
        expect(current.name_glyphs, `stale speaker during: ${current.text}`).toBe('')
      }
      if (previous && Boolean(previous.name) !== Boolean(current.name)) {
        transitions.push({ before: previous, after: current })
        if (previous.name) {
          cleared++
          if (cleared === 1) await testInfo.attach('narration-screen', { body: await page.screenshot(), contentType: 'image/png' })
        } else if (cleared) returned++
      }
      previous = current
      if (cleared >= 3 && returned >= 3) break
    }
    await page.keyboard.press('Space')
    await page.waitForTimeout(180)
    await expect(page.getByRole('alert')).toBeHidden()
  }
  expect(cleared, 'observe multiple actual named-dialogue to narration transitions').toBeGreaterThanOrEqual(3)
  expect(returned, 'subsequent named dialogue must retain its speaker').toBeGreaterThanOrEqual(3)
  expect(errors).toEqual([])
  await testInfo.attach('speaker-transitions', { body: JSON.stringify(transitions, null, 2), contentType: 'application/json' })
})
