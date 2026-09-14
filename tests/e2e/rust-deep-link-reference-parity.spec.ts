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

async function mockReader(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const request = route.request()
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
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
    return json({ items: [] })
  })
}

async function openDeepLink(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/read/${FILE_ID}?deep-link-reference=${crypto.randomUUID()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  // The pinned reference calls openFolder(ROOT) first. That replaces the
  // `/read/{id}` pathname before openDeepLink reads it, so its existing
  // runtime behaviour is to return to the root without opening Reader.
  await expect(page.locator('#reader-view')).toHaveCount(0)
  await expect.poll(() => new URL(page.url()).pathname).toBe('/')
  return {
    path: new URL(page.url()).pathname,
    reader: await page.locator('#reader-view').count(),
    title: await page.getByRole('heading', { name: '我的文件', exact: true }).innerText(),
  }
}

test('直接打开 /read/{id} 保留 reference 的现有回根行为', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockReader(oldPage), mockReader(newPage)])
    const [oldState, newState] = await Promise.all([
      openDeepLink(oldPage, oldUrl),
      openDeepLink(newPage, newUrl),
    ])
    expect(oldState).toEqual({ path: '/', reader: 0, title: '我的文件' })
    expect(newState, 'Rust 直接阅读器深链与 reference 的现有行为不一致').toEqual(oldState)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})
