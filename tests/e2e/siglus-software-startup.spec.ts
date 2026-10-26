import { test, expect } from '@playwright/test'
import { mkdir, writeFile } from 'node:fs/promises'
import { dirname } from 'node:path'

const gameUrl = process.env.SIGLUS_REAL_GAME_URL

for (const scenario of [
  { name: 'desktop', width: 1280, height: 720, dpr: 1 },
  { name: 'wide desktop', width: 2549, height: 1242, dpr: 1 },
  { name: 'double density', width: 1275, height: 621, dpr: 2 },
]) {
  test.describe(scenario.name, () => {
    test.use({ viewport: { width: scenario.width, height: scenario.height }, deviceScaleFactor: scenario.dpr })
    test('software startup draws the whole warning background before any input', async ({ page }, testInfo) => {
      test.skip(!gameUrl, 'Set SIGLUS_REAL_GAME_URL to a local game share')
      test.setTimeout(60_000)
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
      // The first colored startup frame must cover the framebuffer already.
      // Waiting for dialogue or sending a click would miss the reported bug.
      await page.waitForFunction(() => {
        const frame = (window as any).__siglusSoftwareFrame
        const size = (window as any).__siglusSoftwareSize
        if (!frame || !size) return false
        const i = (Math.floor(size[1] * 0.2) * size[0] + Math.floor(size[0] * 0.2)) * 4
        return frame[i] + frame[i + 1] + frame[i + 2] > 80
      }, {}, { timeout: 30_000 })
      const initial = await page.evaluate(() => {
        const frame = (window as any).__siglusSoftwareFrame
        const [width, height] = (window as any).__siglusSoftwareSize
        const canvas = document.querySelector('canvas') as HTMLCanvasElement
        return {
          status: JSON.parse((window as any).__siglusUi || '{}').status,
          backend: (window as any).siglusProbe.renderer.backend,
          frameSize: [width, height],
          canvasSize: [canvas.width, canvas.height],
          cssSize: [canvas.clientWidth, canvas.clientHeight],
          pixels: [0.1, 0.5, 0.9].flatMap(y => [0.1, 0.5, 0.9].map(x => {
            const i = (Math.floor(height * y) * width + Math.floor(width * x)) * 4
            return { x, y, rgba: Array.from(frame.slice(i, i + 4)) as number[] }
          })),
        }
      })
      expect(initial.backend).toBe('software')
      expect(initial.status).toContain('_start')
      for (const pixel of initial.pixels) {
        expect(pixel.rgba[3], `background opacity at ${pixel.x},${pixel.y}`).toBe(255)
        expect(Math.max(...pixel.rgba.slice(0, 3)), `background color at ${pixel.x},${pixel.y}`).toBeGreaterThan(80)
      }
      // Software presentation fills the entire CSS canvas, including at a
      // different aspect ratio and DPR. Invert precisely that displayed area.
      await expect.poll(() => page.evaluate(() => JSON.parse((window as any).__siglusUi || '{}').viewport))
        .toEqual([0, 0, ...initial.canvasSize])
      for (const [x, y] of [[.17, .33], [.83, .67]]) {
        await page.mouse.move(scenario.width * x, scenario.height * y)
        await expect.poll(async () => {
          const point = await page.evaluate(() => JSON.parse((window as any).__siglusUi || '{}').mouse)
          if (!point) return false
          return Math.abs(point[0] - initial.frameSize[0] * x) <= 2 && Math.abs(point[1] - initial.frameSize[1] * y) <= 2
        }).toBe(true)
      }
      await expect(page.getByRole('alert')).toBeHidden()
      expect(errors).toEqual([])
      const output = testInfo.outputPath('first-frame.json')
      await mkdir(dirname(output), { recursive: true })
      await writeFile(output, JSON.stringify({ scenario, initial, errors }, null, 2))
      await page.screenshot({ path: testInfo.outputPath('startup.png') })
      console.log(JSON.stringify({ scenario: scenario.name, initial }))
    })
  })
}
