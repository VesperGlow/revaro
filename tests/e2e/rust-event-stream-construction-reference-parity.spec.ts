import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

async function mockShell(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events') return route.fulfill({ contentType: 'text/event-stream', body: '' })
    if (path === '/api/system/status/stream') return route.fulfill({ contentType: 'text/event-stream', body: '' })
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({
        items: { book: [], image: [], video: [], audio: [] },
        counts: { book: 0, image: 0, video: 0, audio: 0, file: 0 },
      })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 0 })
    if (path === '/api/trash') return json({ items: [], total_bytes: 0, file_count: 0 })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: {
          id: ROOT,
          parent_id: null,
          name: '我的文件',
          kind: 'directory',
          size: 0,
          status: 'ready',
          created_at: STAMP,
          updated_at: STAMP,
          mime_type: '',
        },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    return json({ items: [] })
  })
}

async function failEventSources(page: Page) {
  await page.addInitScript(() => {
    const NativeEventSource = window.EventSource
    function CompatEventSource(this: unknown, url: string | URL, init?: EventSourceInit) {
      if (String(url).includes('/api/system/status/stream') || String(url).includes('/api/events')) {
        throw new Error('模拟 EventSource 构造失败')
      }
      return new NativeEventSource(url, init)
    }
    Object.setPrototypeOf(CompatEventSource, NativeEventSource)
    CompatEventSource.prototype = NativeEventSource.prototype
    Object.defineProperty(window, 'EventSource', {
      configurable: true,
      writable: true,
      value: CompatEventSource,
    })
  })
}

async function openStatus(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?event-source-constructor-reference=${crypto.randomUUID()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.locator('.system-status > summary').click()
  await expect(page.locator('.status-panel')).toBeVisible()
}

test('EventSource 构造失败保持 reference 的可见状态和通知语义', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  try {
    await Promise.all([
      mockShell(oldPage),
      mockShell(newPage),
      failEventSources(oldPage),
      failEventSources(newPage),
    ])
    await Promise.all([openStatus(oldPage, oldUrl), openStatus(newPage, newUrl)])

    const oldState = {
      error: await oldPage.locator('.status-error').count(),
      pending: await oldPage.locator('.status-empty').innerText(),
      toast: await oldPage.locator('.toast').count(),
    }
    const newState = {
      error: await newPage.locator('.status-error').count(),
      pending: await newPage.locator('.status-empty').innerText(),
      toast: await newPage.locator('.toast').count(),
    }
    expect(oldState).toEqual({ error: 0, pending: '正在获取状态…', toast: 0 })
    expect(newState, 'Rust 在 EventSource 构造失败时改变了 reference 的可见状态').toEqual(oldState)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})
