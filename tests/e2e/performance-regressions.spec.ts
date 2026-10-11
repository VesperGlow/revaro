import { test, expect } from '@playwright/test'
import { login, navigate, holdApplicationResponse } from './helpers'

test('library navigation cancels obsolete reads and filters reuse collections', async ({ page }) => {
  await login(page)
  await page.waitForTimeout(500)
  await page.evaluate(() => {
    const original = window.fetch.bind(window)
    const probe = (window as any).libraryProbe = { collections: 0, held: false, aborted: false }
    window.fetch = (input, init) => {
      const url = new URL(typeof input === 'string' ? input : input instanceof Request ? input.url : input.toString(), location.href)
      if (url.pathname === '/api/library/collections') probe.collections++
      if (url.pathname === '/api/library/items' && url.searchParams.get('kind') === 'image') {
        probe.held = true
        const signal = init?.signal || (input instanceof Request ? input.signal : null)
        return new Promise<Response>((_resolve, reject) => {
          const abort = () => { probe.aborted = true; reject(new DOMException('Aborted', 'AbortError')) }
          if (signal?.aborted) abort()
          else signal?.addEventListener('abort', abort, { once: true })
        })
      }
      return original(input, init)
    }
  })
  await navigate(page, '图片')
  await expect.poll(() => page.evaluate(() => (window as any).libraryProbe.held)).toBe(true)
  await navigate(page, '音乐')
  await expect.poll(() => page.evaluate(() => (window as any).libraryProbe.aborted)).toBe(true)
  await expect(page.locator('.library-loading')).toHaveCount(0)
  await page.waitForTimeout(300)
  await page.evaluate(() => { (window as any).libraryProbe.collections = 0 })
  for (let i = 0; i < 10; i++) await page.getByRole('button', { name: '我的收藏', exact: true }).click()
  await page.waitForTimeout(300)
  expect(await page.evaluate(() => (window as any).libraryProbe.collections)).toBe(0)
})

test('same-library refreshes finish the active read and coalesce a follow-up', async ({ page }) => {
  await login(page)
  await navigate(page, '音乐')
  await page.waitForTimeout(500)
  await page.evaluate(() => {
    const original = window.fetch.bind(window)
    const probe = (window as any).refreshProbe = { reads: 0, held: false, aborted: false, release: () => {} }
    window.fetch = async (input, init) => {
      const url = new URL(typeof input === 'string' ? input : input instanceof Request ? input.url : input.toString(), location.href)
      if (url.pathname === '/api/library/items' && url.searchParams.get('kind') === 'audio') {
        probe.reads++
        if (!probe.held) {
          probe.held = true
          const signal = init?.signal || (input instanceof Request ? input.signal : null)
          signal?.addEventListener('abort', () => { probe.aborted = true }, { once: true })
          await new Promise<void>(resolve => { probe.release = resolve })
        }
      }
      return original(input, init)
    }
  })
  await navigate(page, '音乐')
  await expect.poll(() => page.evaluate(() => (window as any).refreshProbe.held)).toBe(true)
  await navigate(page, '音乐')
  await navigate(page, '音乐')
  expect(await page.evaluate(() => (window as any).refreshProbe.reads)).toBe(1)
  expect(await page.evaluate(() => (window as any).refreshProbe.aborted)).toBe(false)
  await page.evaluate(() => (window as any).refreshProbe.release())
  await expect.poll(() => page.evaluate(() => (window as any).refreshProbe.reads)).toBe(2)
  await page.waitForTimeout(300)
  expect(await page.evaluate(() => (window as any).refreshProbe.reads)).toBe(2)
})

test('navigation revalidates collections created outside the current document', async ({ page }) => {
  await login(page)
  const name = `external-collection-${Date.now()}`
  const headers = { origin: new URL(page.url()).origin }
  const response = await page.request.post('/api/library/collections', { headers, data: { name, kind: 'audio' } })
  expect(response.status()).toBe(200)
  const collection = await response.json()
  try {
    await navigate(page, '音乐')
    await page.getByLabel('选择集合', { exact: true }).click()
    await expect(page.getByRole('button', { name: `${name} · 0`, exact: true })).toBeVisible()
  } finally {
    await page.request.delete(`/api/library/collections/${collection.id}`, { headers })
  }
})

test('returning to a page revalidates collections after a delayed response', async ({ page }) => {
  await login(page)
  await page.waitForTimeout(500)
  await page.evaluate(() => {
    const original = window.fetch.bind(window)
    ;(window as any).collectionReads = 0
    window.fetch = (input, init) => {
      const url = new URL(typeof input === 'string' ? input : input instanceof Request ? input.url : input.toString(), location.href)
      if (url.pathname === '/api/library/collections') (window as any).collectionReads++
      return original(input, init)
    }
  })
  const held = await holdApplicationResponse(page, '^/api/library/collections$', 'GET')
  const name = `return-collection-${Date.now()}`
  const headers = { origin: new URL(page.url()).origin }
  let id: string | undefined
  try {
    await navigate(page, '图片')
    await expect.poll(held.captured).toBe(true)
    const response = await page.request.post('/api/library/collections', { headers, data: { name, kind: 'image' } })
    expect(response.status()).toBe(200)
    id = (await response.json()).id
    await navigate(page, '视频')
    await navigate(page, '图片')
    expect(await page.evaluate(() => (window as any).collectionReads)).toBe(1)
    await held.release()
    await expect.poll(() => page.evaluate(() => (window as any).collectionReads)).toBe(2)
    await page.getByLabel('选择集合', { exact: true }).click()
    await expect(page.getByRole('button', { name: `${name} · 0`, exact: true })).toBeVisible()
  } finally {
    await held.release()
    if (id) await page.request.delete(`/api/library/collections/${id}`, { headers })
  }
})
