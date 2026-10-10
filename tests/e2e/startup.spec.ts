import { test, expect } from '@playwright/test'

test('cold startup paints the loader and streams WASM while transport initializes', async ({ page }) => {
  await page.addInitScript(() => {
    const state = (window as any).startupProbe = { streamingStarted: false, streamingFinished: false, controlledAtMount: false, workerRequested: false, releaseWorker: () => {} }
    const register = navigator.serviceWorker.register.bind(navigator.serviceWorker)
    navigator.serviceWorker.register = async (...args) => {
      state.workerRequested = true
      await new Promise<void>(resolve => { state.releaseWorker = resolve })
      return register(...args)
    }
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
    await expect.poll(() => page.evaluate(() => (window as any).startupProbe.workerRequested)).toBe(true)
    await expect.poll(() => page.evaluate(() => (window as any).startupProbe.streamingFinished)).toBe(true)
    await expect(page.locator('.boot-splash')).toBeVisible()
    await expect(page.locator('.login-page')).toHaveCount(0)
    expect(await page.evaluate(() => Boolean(navigator.serviceWorker.controller))).toBe(false)
    await page.evaluate(() => (window as any).startupProbe.releaseWorker())
    await expect(page.getByLabel('用户名')).toBeVisible()
    expect(await page.evaluate(() => (window as any).startupProbe.controlledAtMount)).toBe(true)
    await expect(page.locator('.boot-splash')).toHaveCount(0)
    expect(await page.evaluate(() => performance.getEntriesByType('resource').filter(e => new URL(e.name).pathname.endsWith('.wasm')).length)).toBe(1)
  } finally {
    await page.evaluate(() => (window as any).startupProbe?.releaseWorker()).catch(() => {})
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

test('an unavailable worker cannot block startup or mutable API requests', async ({ page }) => {
  await page.addInitScript(() => {
    navigator.serviceWorker.register = () => new Promise(() => {})
  })
  await page.goto('/', { waitUntil: 'commit' })
  await expect(page.getByLabel('用户名')).toBeVisible({ timeout: 20_000 })
  expect(await page.evaluate(() => Boolean(navigator.serviceWorker.controller))).toBe(false)
  const result = await page.evaluate(async () => {
    const response = await fetch('/api/auth/me')
    return response.status
  })
  expect(result).toBe(401)
})
