import { test, expect } from '@playwright/test'

const gameUrl = process.env.SIGLUS_REAL_GAME_URL

test('real Siglus audio resumes on a continuous current timeline after main-thread stalls', async ({ page }, testInfo) => {
  test.skip(!gameUrl, 'Set SIGLUS_REAL_GAME_URL to a local game share')
  test.setTimeout(180_000)
  expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(gameUrl!).hostname)
  await page.addInitScript(() => {
    const w = window as any
    w.__siglusProbeUi = true
    w.__audioStarts = []
    w.__audioNonSilent = false
    // Exercise the portable renderer without requiring a GPU on the runner.
    Object.defineProperty(navigator, 'gpu', { configurable: true, value: undefined })
    const getContext = HTMLCanvasElement.prototype.getContext
    HTMLCanvasElement.prototype.getContext = function (kind: string, ...args: any[]): any {
      return ['webgl2', 'webgpu'].includes(kind) ? null : (getContext as any).call(this, kind, ...args)
    }
    const contexts = new WeakMap<AudioContext, number>()
    let nextContext = 0
    const create = AudioContext.prototype.createBufferSource
    AudioContext.prototype.createBufferSource = function () {
      if (!contexts.has(this)) contexts.set(this, ++nextContext)
      const node = create.call(this), context = this, start = node.start
      node.start = function (when = 0, ...args: [number?, number?]) {
        w.__audioStarts.push({
          context: contexts.get(context), at: performance.now(), when,
          now: context.currentTime, duration: node.buffer?.duration,
        })
        return start.call(this, when, ...args)
      }
      return node
    }
    const copy = AudioBuffer.prototype.copyToChannel
    AudioBuffer.prototype.copyToChannel = function (samples, channel, offset = 0) {
      if (samples.some(sample => Math.abs(sample) > .001)) w.__audioNonSilent = true
      return copy.call(this, samples, channel, offset)
    }
  })
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
  await page.goto(gameUrl!, { waitUntil: 'domcontentloaded' })
  await page.waitForFunction(() => (window as any).__siglusUi, {}, { timeout: 60_000 })
  await page.locator('canvas').click()
  await page.waitForFunction(() => JSON.parse((window as any).__siglusUi || '{}').text?.length, {}, { timeout: 60_000 })
  await page.waitForTimeout(600)
  const recoveries = []
  let voiceReads = 0
  for (const duration of [250, 1500, 2400]) {
    // Reach a new actual spoken line for each stall, exercising voice changes
    // through the real VM, OVK decoder and mixer as well as background music.
    let spoken = false
    for (let input = 0; input < 180; input++) {
      const state = await page.evaluate(() => ({
        voices: (window as any).siglusProbe.voiceReads.length,
        ui: JSON.parse((window as any).__siglusUi || '{}'),
      }))
      if (state.voices > voiceReads && state.ui.waiting && state.ui.text) {
        voiceReads = state.voices
        spoken = true
        break
      }
      await page.keyboard.press('Space')
      await page.waitForTimeout(160)
      await expect(page.getByRole('alert')).toBeHidden()
    }
    expect(spoken, 'reach a new voiced line before stalling playback').toBe(true)
    const resume = await page.evaluate(duration => {
      const w = window as any
      w.__audioStarts = []
      w.__audioNonSilent = false
      const started = performance.now()
      while (performance.now() - started < duration) { /* Deliberate long task, like resource decoding. */ }
      return performance.now()
    }, duration)
    await expect.poll(() => page.evaluate(() => (window as any).__audioStarts.length)).toBeGreaterThan(8)
    const result = await page.evaluate(() => ({ starts: (window as any).__audioStarts, pcm: (window as any).__audioNonSilent }))
    recoveries.push({ duration, resume, voiceReads, ...result })
    expect(result.pcm, 'non-silent playback must resume after each stall').toBe(true)
    for (const start of result.starts) {
      expect(start.when - start.now, 'never schedule a stale buffer in the past').toBeGreaterThanOrEqual(-.01)
    }
    const lastByContext = new Map<number, any>()
    for (const start of result.starts) {
      const previous = lastByContext.get(start.context)
      if (previous) expect(start.when, 'successive buffers must not overlap').toBeGreaterThanOrEqual(previous.when + previous.duration - .001)
      lastByContext.set(start.context, start)
    }
    // Playback must continue without requiring another gesture.
    expect(await page.evaluate(() => (window as any).siglusProbe.phase)).toBe('running')
    await expect(page.getByRole('alert')).toBeHidden()
  }
  expect(errors).toEqual([])
  await testInfo.attach('audio-stall-recoveries', { body: JSON.stringify({ recoveries, errors }, null, 2), contentType: 'application/json' })
})
