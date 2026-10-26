import { test, expect, type Page } from '@playwright/test'

async function probe(page: Page, gpu: 'available' | 'missing' | 'rejected', gl: boolean) {
  const renderer = await page.request.get('/galgame/siglus/renderer.js')
  expect(renderer.status(), 'the renderer module must be served by the real server').toBe(200)
  expect(renderer.headers()['content-type']).toContain('javascript')
  await page.addInitScript(({ gpu, gl }) => {
    const calls: string[] = []
    ;(window as any).__rendererCalls = calls
    Object.defineProperty(navigator, 'gpu', { configurable: true, value: gpu === 'missing' ? undefined : {
      requestAdapter: async () => {
        if (gpu === 'rejected') throw Error('GPU adapter unavailable')
        return { limits: { maxTextureDimension2D: 8192 } }
      },
    } })
    HTMLCanvasElement.prototype.getContext = function (type: string): any {
      if (this.id === 'game') throw Error('The game canvas was bound during capability detection')
      calls.push(type)
      if (type === 'webgpu') return gpu === 'available' ? {} : null
      if (type !== 'webgl2' || !gl) return null
      return { MAX_TEXTURE_SIZE: 3379, getParameter: () => 16384,
        getExtension: () => ({ loseContext: () => calls.push('release-gl-probe') }) }
    }
  }, { gpu, gl })
  await page.route('**/siglus-renderer-check', route => route.fulfill({ contentType: 'text/html', body: `
    <canvas id="game"></canvas>
    <script type="module">
      import { selectRenderer } from '/galgame/siglus/renderer.js';
      try { window.result = await selectRenderer() } catch (error) { window.result = { error: error.message } }
    </script>
  ` }))
  await page.goto('/siglus-renderer-check')
  await expect.poll(() => page.evaluate(() => (window as any).result)).toBeTruthy()
  return page.evaluate(() => ({ result: (window as any).result, calls: (window as any).__rendererCalls }))
}

test('WebGPU is selected when WebGL2 is unavailable, without binding the game canvas', async ({ page }) => {
  expect(await probe(page, 'available', false)).toEqual({
    result: { backend: 'webgpu', maxTextureDimension2D: 8192 }, calls: ['webgl2', 'webgl2', 'webgpu'],
  })
})

for (const gpu of ['missing', 'rejected'] as const) {
  test(`WebGL2 is selected and its probe is released when WebGPU is ${gpu}`, async ({ page }) => {
    expect(await probe(page, gpu, true)).toEqual({
      result: { backend: 'gl', maxTextureDimension2D: 16384 }, calls: ['webgl2', 'release-gl-probe'],
    })
  })
}

test('unavailable graphics backends select the software renderer', async ({ page }) => {
  expect(await probe(page, 'missing', false)).toEqual({
    result: { backend: 'software', maxTextureDimension2D: 0, diagnostics: expect.any(String) },
    calls: ['webgl2', 'webgl2'],
  })
})
