import { test, expect } from '@playwright/test'

test('cold startup paints the loader and streams WASM while transport initializes', async ({ page, context }) => {
  let releaseWorker!: () => void
  const workerGate = new Promise<void>(resolve => { releaseWorker = resolve })
  let workerRequested!: () => void
  const workerRequest = new Promise<void>(resolve => { workerRequested = resolve })
  await context.route('**/transport-worker.js', async route => {
    workerRequested()
    await workerGate
    await route.continue()
  })
  await page.addInitScript(() => {
    const state = (window as any).startupProbe = { streamingStarted: false, streamingFinished: false, controlledAtMount: false }
    const original = WebAssembly.instantiateStreaming.bind(WebAssembly)
    WebAssembly.instantiateStreaming = async (...args) => {
      state.streamingStarted = true
      const result = await original(...args)
      state.streamingFinished = true
      return result
    }
    const observer = new MutationObserver(() => {
      if (document.querySelector('.login-page')) {
        state.controlledAtMount = Boolean(navigator.serviceWorker.controller)
        observer.disconnect()
      }
    })
    observer.observe(document, { childList: true, subtree: true })
  })
  try {
    await page.goto('/', { waitUntil: 'commit' })
    await expect(page.getByRole('status')).toHaveText('正在加载 Revaro…')
    await workerRequest
    await expect.poll(() => page.evaluate(() => (window as any).startupProbe.streamingFinished)).toBe(true)
    await expect(page.locator('.boot-splash')).toBeVisible()
    await expect(page.locator('.login-page')).toHaveCount(0)
    expect(await page.evaluate(() => Boolean(navigator.serviceWorker.controller))).toBe(false)
    releaseWorker()
    await expect(page.getByLabel('用户名')).toBeVisible()
    expect(await page.evaluate(() => (window as any).startupProbe.controlledAtMount)).toBe(true)
    await expect(page.locator('.boot-splash')).toHaveCount(0)
    expect(await page.evaluate(() => performance.getEntriesByType('resource').filter(e => new URL(e.name).pathname.endsWith('.wasm')).length)).toBe(1)
  } finally {
    releaseWorker()
  }
})

test('startup failure keeps an actionable loading screen', async ({ page }) => {
  await page.route('**/core.*.wasm', route => route.fulfill({ status: 503, body: 'unavailable' }))
  await page.goto('/', { waitUntil: 'domcontentloaded' })
  await expect(page.locator('.boot-splash')).toBeVisible()
  await expect(page.getByRole('button', { name: '重新加载', exact: true })).toBeVisible()
  await expect(page.locator('#boot-spinner')).toBeHidden()
  await expect(page.locator('.login-page')).toHaveCount(0)
})
