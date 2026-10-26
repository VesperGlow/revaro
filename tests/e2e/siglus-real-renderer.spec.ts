import { test, expect } from '@playwright/test'
import { writeFile, mkdir } from 'node:fs/promises'
import { dirname } from 'node:path'

const gameUrl = process.env.SIGLUS_REAL_GAME_URL
const browser = process.env.E2E_BROWSER || 'chromium'
test.use({ launchOptions: {
  ...(process.env.PLAYWRIGHT_EXECUTABLE_PATH ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH } : {}),
  args: browser === 'chromium' ? ['--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--enable-unsafe-webgpu'] : [],
} })

const cases = [
  { name: 'reported desktop size', width: 2549, height: 1242, dpr: 1, backend: 'gl', limit: false },
  { name: 'double density', width: 1275, height: 621, dpr: 2, backend: 'gl', limit: false },
  { name: 'actual 2048 device limit', width: 2549, height: 1242, dpr: 1, backend: 'gl', limit: true },
  { name: 'WebGPU with unavailable WebGL2', width: 2549, height: 1242, dpr: 1, backend: 'webgpu', limit: false },
  { name: 'Canvas2D software fallback', width: 1280, height: 720, dpr: 1, backend: 'software', limit: false },
]

for (const scenario of cases) {
  test.describe(scenario.name, () => {
    test.use({ viewport: { width: scenario.width, height: scenario.height }, deviceScaleFactor: scenario.dpr })
    test('real game starts, resizes and keeps pointer coordinates aligned', async ({ page }, testInfo) => {
      test.skip(!gameUrl, 'Set SIGLUS_REAL_GAME_URL to a local game share')
      test.skip(scenario.backend === 'webgpu' && browser !== 'chromium', 'Software WebGPU harness uses Chromium')
      test.skip(scenario.backend === 'webgpu', 'WebGPU capability selection is covered by siglus-renderer.spec.ts; real gameplay uses WebGL2 when available')
      test.setTimeout(180_000)
      expect(['localhost', '127.0.0.1', '[::1]']).toContain(new URL(gameUrl!).hostname)
      await page.addInitScript(({ backend, limit }) => {
        ;(window as any).__siglusProbeUi = true
        const copy = AudioBuffer.prototype.copyToChannel
        AudioBuffer.prototype.copyToChannel = function (samples, channel, start = 0) {
          if (samples.some(sample => Math.abs(sample) > 0.001)) (window as any).__siglusNonSilent = true
          return copy.call(this, samples, channel, start)
        }
        if (backend === 'gl') Object.defineProperty(navigator, 'gpu', { configurable: true, value: undefined })
        else {
          const getContext = HTMLCanvasElement.prototype.getContext
          HTMLCanvasElement.prototype.getContext = function (type: string, ...args: any[]): any {
            return (type === 'webgl2' || (backend === 'software' && type === 'webgpu')) ? null : (getContext as any).call(this, type, ...args)
          }
          if (backend === 'software') Object.defineProperty(navigator, 'gpu', { configurable: true, value: undefined })
        }
        if (limit) {
          const getParameter = WebGL2RenderingContext.prototype.getParameter
          WebGL2RenderingContext.prototype.getParameter = function (name: number): any {
            if (name === this.MAX_TEXTURE_SIZE || name === this.MAX_RENDERBUFFER_SIZE) return 2048
            return getParameter.call(this, name)
          }
        }
      }, scenario)
      const errors: string[] = []
      page.on('pageerror', error => errors.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
      const state = () => page.evaluate(() => ({
        ui: JSON.parse((window as any).__siglusUi || '{}'),
        renderer: (window as any).siglusProbe?.renderer,
        phase: (window as any).siglusProbe?.phase,
        error: document.querySelector('#error')?.textContent,
        canvas: { width: (document.querySelector('canvas') as HTMLCanvasElement).width, height: (document.querySelector('canvas') as HTMLCanvasElement).height },
        screen: { width: innerWidth, height: innerHeight, dpr: devicePixelRatio },
      }))
      await page.goto(gameUrl!, { waitUntil: 'domcontentloaded' })
      await page.waitForFunction(() => (window as any).__siglusUi || (window as any).siglusProbe?.phase === 'failed', {}, { timeout: 60_000 })
      await expect(page.getByRole('alert')).toBeHidden()
      await page.locator('canvas').click()
      await page.waitForFunction(() => JSON.parse((window as any).__siglusUi || '{}').text?.length || (window as any).siglusProbe?.phase === 'failed', {}, { timeout: 60_000 })
      await expect(page.getByRole('alert')).toBeHidden()
      const first = await state()
      expect(first.renderer.backend).toBe(scenario.backend)
      expect(first.ui.text).toBeTruthy()
      if (scenario.backend === 'software') expect(first.canvas.width).toBe(first.screen.width * first.screen.dpr)
      else if (scenario.limit) expect(first.canvas.width).toBeLessThanOrEqual(2048)
      else expect(first.canvas.width).toBeGreaterThan(2048)
      await page.setViewportSize({ width: 2800, height: 1100 })
      await expect.poll(async () => (await state()).screen.width).toBe(2800)
      await expect.poll(async () => (await state()).ui.redraws).toBeGreaterThan(first.ui.redraws)
      await page.locator('canvas').click({ button: 'right' })
      await expect.poll(async () => (await state()).ui.hidden).toBe(true)
      await page.locator('canvas').click({ button: 'right' })
      await expect.poll(async () => (await state()).ui.hidden).toBe(false)
      const beforeClick = await state()
      for (let i = 0; i < 4; i++) {
        await page.locator('canvas').click()
        await page.waitForTimeout(500)
      }
      await expect.poll(async () => (await state()).ui.status).not.toBe(beforeClick.ui.status)
      await mkdir(dirname(testInfo.outputPath('game-desktop.png')), { recursive: true })
      await page.screenshot({ path: testInfo.outputPath('game-desktop.png') })
      await page.setViewportSize({ width: 390, height: 844 })
      await expect.poll(async () => (await state()).canvas.width).toBe(390 * scenario.dpr)
      await expect(page.getByRole('alert')).toBeHidden()
      const last = await state()
      expect(last.phase).toBe('running')
      expect(await page.evaluate(() => (window as any).__siglusNonSilent)).toBe(true)
      expect(errors).toEqual([])
      const report = { scenario, first, last, errors }
      const output = testInfo.outputPath('playback-state.json')
      await mkdir(dirname(output), { recursive: true })
      await writeFile(output, JSON.stringify(report, null, 2))
      await page.screenshot({ path: testInfo.outputPath('game-screen.png') })
      console.log(JSON.stringify({ scenario: scenario.name, renderer: first.renderer, initialCanvas: first.canvas, finalCanvas: last.canvas, errors }))
    })
  })
}
