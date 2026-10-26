import { test, expect } from '@playwright/test'

test('actual WASM fast paths preserve affine sampling, blending and cache invalidation', async ({ page }) => {
  await page.route('**/siglus-software-check', route => route.fulfill({ contentType: 'text/html', body: '<!doctype html><title>Software compositor verification</title>' }))
  await page.goto('/siglus-software-check')
  const result = await page.evaluate(async () => {
    const engine = await import(/* @vite-ignore */ '/galgame/siglus/pkg/game_launcher.js')
    await engine.default()
    return JSON.parse(engine.siglus_wasm_software_verify())
  })
  expect(result.comparisons).toBe(162)
  expect(result.failures).toEqual([])
})
