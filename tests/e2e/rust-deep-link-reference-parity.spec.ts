import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const FILE_ID = 'deep-link-text'
const STAMP = '2026-01-01T00:00:00Z'

const root = {
  id: ROOT,
  parent_id: null,
  name: '我的文件',
  kind: 'directory',
  size: 0,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: '',
}

const book = {
  id: FILE_ID,
  parent_id: ROOT,
  name: '深链文本.txt',
  kind: 'file',
  size: 12,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'text/plain',
  etag: 'deep-link-etag',
}

const manifest = {
  version: 4,
  format: 'txt',
  book_key: 'deep-link-book',
  total_chars: 12,
  spines: [{ block_start: 0, block_count: 1 }],
  chunks: [{ index: 0, block_start: 0, block_count: 1, chars: 12 }],
  toc: [],
}

async function mockReader(page: Page, initiallySignedIn = true) {
  let signedIn = initiallySignedIn
  // Deep-link tests do not exercise background streams; leave them inert so
  // these long-lived connections cannot obscure route behavior or teardown.
  await page.addInitScript(() => {
    class QuietEventSource {
      static CONNECTING = 0
      static OPEN = 1
      static CLOSED = 2
      readonly url: string
      readyState = QuietEventSource.OPEN
      withCredentials = false

      constructor(url: string | URL) {
        this.url = new URL(url, location.href).href
      }

      addEventListener() {}
      removeEventListener() {}
      close() {
        this.readyState = QuietEventSource.CLOSED
      }
    }

    Object.defineProperty(window, 'EventSource', { configurable: true, value: QuietEventSource })
  })
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const request = route.request()
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') {
      if (signedIn) return json({ username: 'admin', has_avatar: false })
      return route.fulfill({
        status: 401,
        contentType: 'application/json',
        body: JSON.stringify({ error: { status: 401, message: 'not signed in' } }),
      })
    }
    if (path === '/api/auth/login') {
      signedIn = true
      return json({ username: 'admin', has_avatar: false })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 1, image: 0, video: 0, audio: 0, file: 0 } })
    }
    if (path === '/api/library/counts') return json({ book: 1, image: 0, video: 0, audio: 0, file: 0 })
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    if (path === `/api/files/${FILE_ID}`) return json({ file: book, breadcrumbs: [root] })
    if (path === `/api/files/${FILE_ID}/book`) return json({ format: 'txt', title: '深链文本', name: book.name, cover: false, toc: [] })
    if (path === `/api/files/${FILE_ID}/book/progress`) {
      if (request.method() === 'PUT') return route.fulfill({ status: 204, body: '' })
      return json({})
    }
    if (path === `/api/files/${FILE_ID}/book/flow`) return json(manifest)
    if (path === `/api/files/${FILE_ID}/book/flow/chunks/0`) {
      return route.fulfill({ contentType: 'text/html; charset=utf-8', body: '<p data-block="0">深链文本内容</p>' })
    }
    if (path.startsWith('/api/files/')) {
      return route.fulfill({
        status: 404,
        contentType: 'application/json',
        body: JSON.stringify({ error: { status: 404, message: 'file not found' } }),
      })
    }
    return json({ items: [] })
  })
}

async function openDeepLink(
  page: Page,
  baseUrl: string,
  id: string,
  expected: { reader: boolean; fetchTarget: boolean },
) {
  const targetResponse = expected.fetchTarget
    ? page.waitForResponse(response => new URL(response.url()).pathname === `/api/files/${id}`)
    : null
  await page.goto(`${baseUrl}/read/${id}?deep-link-reference=${crypto.randomUUID()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  if (targetResponse) await targetResponse
  if (expected.reader) {
    await expect(page.locator('#reader-view')).toBeVisible()
    await expect(page.locator('#reader-title')).toHaveText('深链文本')
    await expect.poll(() => new URL(page.url()).pathname).toBe(`/read/${id}`)
  } else {
    await expect(page.locator('#reader-view')).toHaveCount(0)
    await expect.poll(() => new URL(page.url()).pathname).toBe('/')
  }
  return captureOutcome(page)
}

async function captureOutcome(page: Page) {
  const reader = page.locator('#reader-view')
  const readerCount = await reader.count()
  if (readerCount) await expect(reader).toBeVisible()
  return {
    path: new URL(page.url()).pathname,
    reader: readerCount,
    title: await page.getByRole('heading', { name: '我的文件', exact: true }).innerText(),
    readerTitle: readerCount ? await page.locator('#reader-title').textContent() : null,
  }
}

async function submitLogin(page: Page, expected: { reader: boolean; fetchTarget: boolean }) {
  await expect(page.getByRole('heading', { name: '登录私人空间' })).toBeVisible()
  await page.getByLabel('用户名').fill('admin')
  await page.getByLabel('密码').fill('test-password')
  const targetResponse = expected.fetchTarget
    ? page.waitForResponse(response => new URL(response.url()).pathname === `/api/files/${FILE_ID}`)
    : null
  await page.getByRole('button', { name: '进入我的网盘' }).click()
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  if (targetResponse) await targetResponse
  if (expected.reader) {
    await expect(page.locator('#reader-view')).toBeVisible()
    await expect(page.locator('#reader-title')).toHaveText('深链文本')
    await expect.poll(() => new URL(page.url()).pathname).toBe(`/read/${FILE_ID}`)
  } else {
    await expect(page.locator('#reader-view')).toHaveCount(0)
    await expect.poll(() => new URL(page.url()).pathname).toBe('/')
  }
  return captureOutcome(page)
}

test('有效 /read/{id} 深链打开阅读器，并记录 reference 的路由缺陷例外', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockReader(oldPage), mockReader(newPage)])
    const [oldState, newState] = await Promise.all([
      openDeepLink(oldPage, oldUrl, FILE_ID, { reader: false, fetchTarget: false }),
      openDeepLink(newPage, newUrl, FILE_ID, { reader: true, fetchTarget: true }),
    ])
    expect(oldState, '旧版先回根目录，导致 openDeepLink 读不到原 pathname').toEqual({
      path: '/', reader: 0, title: '我的文件', readerTitle: null,
    })
    expect(newState, 'Rust 应按旧版源码声明的 /read/{id} 意图打开 TXT 阅读器').toEqual({
      path: `/read/${FILE_ID}`, reader: 1, title: '我的文件', readerTitle: '深链文本',
    })
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('未登录时打开阅读器深链，登录后仍回到目标文件', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockReader(oldPage, false), mockReader(newPage, false)])
    await Promise.all([
      oldPage.goto(`${oldUrl}/read/${FILE_ID}?login-deep-link=${crypto.randomUUID()}`),
      newPage.goto(`${newUrl}/read/${FILE_ID}?login-deep-link=${crypto.randomUUID()}`),
    ])
    const [oldState, newState] = await Promise.all([
      submitLogin(oldPage, { reader: false, fetchTarget: false }),
      submitLogin(newPage, { reader: true, fetchTarget: true }),
    ])
    expect(oldState, '旧版登录成功时直接打开根目录，丢失原深链目标').toEqual({
      path: '/', reader: 0, title: '我的文件', readerTitle: null,
    })
    expect(newState, 'Rust 应在认证后从原 pathname 恢复 Reader 深链').toEqual({
      path: `/read/${FILE_ID}`, reader: 1, title: '我的文件', readerTitle: '深链文本',
    })
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('无效 /read/{id} 深链安全回到根目录，不留下阅读器或错误页面', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockReader(oldPage), mockReader(newPage)])
    const [oldState, newState] = await Promise.all([
      openDeepLink(oldPage, oldUrl, 'missing-reader-target', { reader: false, fetchTarget: false }),
      openDeepLink(newPage, newUrl, 'missing-reader-target', { reader: false, fetchTarget: true }),
    ])
    const expected = { path: '/', reader: 0, title: '我的文件', readerTitle: null }
    expect(oldState).toEqual(expected)
    expect(newState, '无效目标应保持根目录，避免残留空 Reader').toEqual(expected)
    await expect(newPage.locator('.toast-error')).toHaveCount(0)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})
