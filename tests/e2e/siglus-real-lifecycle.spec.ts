import { test, expect } from '@playwright/test'

// Opt-in integration check against a local production player with real game
// resources. No game data or key is required by the normal CI suite.
const gameUrl = process.env.SIGLUS_REAL_GAME_URL

test('real Siglus playback survives retired audio callbacks in a strict browser', async ({ page }, testInfo) => {
  test.skip(!gameUrl, 'Set SIGLUS_REAL_GAME_URL to a local game share')
  test.setTimeout(120_000)
  expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(gameUrl!).hostname)
  await page.addInitScript(() => {
    ;(window as any).__siglusProbeUi = true
    const stats = { retired: 0, closedAccesses: 0, sources: 0, nonSilent: false }
    ;(window as any).__audioLifecycleStats = stats
    const proto = AudioContext.prototype
    const close = proto.close
    proto.close = function () {
      stats.retired++
      return close.call(this)
    }
    const create = proto.createBufferSource
    proto.createBufferSource = function () {
      if (this.state === 'closed') {
        stats.closedAccesses++
        throw new DOMException('AudioContext is closed.', 'InvalidStateError')
      }
      stats.sources++
      return create.call(this)
    }
    const copy = AudioBuffer.prototype.copyToChannel
    AudioBuffer.prototype.copyToChannel = function (samples, channel, start = 0) {
      if (samples.some(sample => Math.abs(sample) > 0.001)) stats.nonSilent = true
      return copy.call(this, samples, channel, start)
    }
  })
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
  const ui = () => page.evaluate(() => JSON.parse((window as any).__siglusUi || '{}'))
  await page.goto(gameUrl!, { waitUntil: 'domcontentloaded' })
  await page.waitForFunction(() => (window as any).__siglusUi || (window as any).siglusProbe?.phase === 'failed', {}, { timeout: 60_000 })
  await expect(page.getByRole('alert')).toBeHidden()
  await page.locator('canvas').click()
  await page.waitForFunction(() => JSON.parse((window as any).__siglusUi || '{}').text?.length || (window as any).siglusProbe?.phase === 'failed', {}, { timeout: 60_000 })
  await expect(page.getByRole('alert')).toBeHidden()
  const first = await ui()
  expect(first.text).toBeTruthy()
  for (let i = 0; i < 30; i++) {
    await page.keyboard.press('Space')
    await page.waitForTimeout(350)
    await expect(page.getByRole('alert')).toBeHidden()
  }
  const last = await ui()
  expect(last.status).not.toBe(first.status)
  expect(last.redraws).toBeGreaterThan(first.redraws)
  const stats = await page.evaluate(() => (window as any).__audioLifecycleStats)
  expect(stats.retired).toBeGreaterThan(0)
  expect(stats.sources).toBeGreaterThan(2)
  expect(stats.nonSilent).toBe(true)
  expect(stats.closedAccesses).toBe(0)
  expect(errors).toEqual([])
  expect(await page.evaluate(() => (window as any).siglusProbe.phase)).toBe('running')
  await testInfo.attach('playback-state', { body: JSON.stringify({ first, last, stats, errors }, null, 2), contentType: 'application/json' })
  await testInfo.attach('game-screen', { body: await page.screenshot(), contentType: 'image/png' })
})
