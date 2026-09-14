import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

const file = (value: Record<string, unknown>) => ({
  parent_id: ROOT,
  kind: 'file',
  size: 1024,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'application/octet-stream',
  etag: `etag-${String(value.id ?? 'item')}`,
  ...value,
})

const root = file({ id: ROOT, name: '我的文件', kind: 'directory', parent_id: null, size: 0, mime_type: '' })
const folder = file({ id: 'open-folder', name: '资料', kind: 'directory', size: 0, mime_type: '' })
const text = file({ id: 'open-text', name: '说明.txt', mime_type: 'text/plain', size: 32 })
const epub = file({ id: 'open-book', name: '兼容书籍.epub', mime_type: 'application/epub+zip', size: 2048 })
const image = file({ id: 'open-image', name: '预览.png', mime_type: 'image/png', size: 4096 })
const audio = file({ id: 'open-audio', name: '声音.mp3', mime_type: 'audio/mpeg', size: 4096 })
const video = file({ id: 'open-video', name: '影像.mp4', mime_type: 'video/mp4', size: 4096 })
const unknown = file({ id: 'open-unknown', name: '数据.bin', mime_type: 'application/octet-stream', size: 4096 })
const rootItems = [folder, text, epub, image, audio, video, unknown]

const manifest = {
  version: 4,
  format: 'epub',
  book_key: 'open-item-reference-book',
  total_chars: 20,
  spines: [{ block_start: 0, block_count: 1 }],
  chunks: [{ index: 0, block_start: 0, block_count: 1, chars: 20 }],
  toc: [],
}

const picture = '<svg xmlns="http://www.w3.org/2000/svg" width="900" height="600"><rect width="900" height="600" fill="#789"/></svg>'

async function mockOpening(page: Page) {
  await page.route('**/api/**', async route => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })

    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 1, image: 1, video: 1, audio: 1, file: rootItems.length } })
    }
    if (path === '/api/library/counts') return json({ book: 1, image: 1, video: 1, audio: 1, file: rootItems.length })
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: rootItems, total_bytes: 17408, file_count: rootItems.length - 1 })
    if (path === `/api/files/${folder.id}`) return json({ file: folder, breadcrumbs: [root, folder] })
    if (path === `/api/files/${folder.id}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    if (path === `/api/files/${text.id}/content`) return json({ content: '打开分流测试文本\n', etag: text.etag, updated_at: STAMP })
    if (path === `/api/files/${epub.id}/book`) return json({ format: 'epub', title: '兼容书籍', name: epub.name, cover: false, toc: [] })
    if (path === `/api/files/${epub.id}/book/progress`) {
      if (request.method() === 'PUT') return route.fulfill({ status: 204, body: '' })
      return json({})
    }
    if (path === `/api/files/${epub.id}/book/flow`) return json(manifest)
    if (path === `/api/files/${epub.id}/book/flow/chunks/0`) {
      return route.fulfill({ contentType: 'text/html; charset=utf-8', body: '<p data-block="0">兼容阅读内容</p>' })
    }
    if (path.endsWith('/audio') || path.endsWith('/video')) return json({ subtitles: [], chapters: [] })
    if (path.endsWith('/media/progress')) {
      if (request.method() === 'PUT') return route.fulfill({ status: 204, body: '' })
      return json({ position: 0 })
    }
    if (path.endsWith('/thumbnail') || path.endsWith('/preview')) {
      return route.fulfill({ contentType: 'image/svg+xml', body: picture })
    }
    return json({ items: [] })
  })
}

async function openRoot(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?open-item-reference=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await expect(page.locator('.file-card')).toHaveCount(rootItems.length)
}

async function overlaySnapshot(page: Page) {
  return page.evaluate(() => ({
    path: location.pathname,
    title: document.title,
    heading: document.querySelector('.content-head h1')?.textContent?.trim() ?? null,
    preview: document.querySelector('.preview-modal')?.className ?? null,
    reader: document.querySelector('#reader-view')?.className ?? null,
    editor: document.querySelector('.document-editor')?.className ?? null,
  }))
}

async function returnToRoot(page: Page) {
  await page.goBack()
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await expect(page.locator('.preview-modal, #reader-view, .document-editor')).toHaveCount(0)
}

async function exerciseOpening(page: Page, baseUrl: string) {
  await openRoot(page, baseUrl)
  const states: Record<string, unknown> = {}

  await page.locator('.file-card').filter({ hasText: folder.name }).click()
  await expect(page.getByRole('heading', { name: folder.name, exact: true })).toBeVisible()
  states.folder = await overlaySnapshot(page)
  await page.goBack()
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await expect(page.locator('.file-card')).toHaveCount(rootItems.length)

  await page.locator('.file-card').filter({ hasText: text.name }).click()
  await expect(page.locator('.document-editor')).toBeVisible()
  await expect(page.locator('.document-editor textarea')).toHaveValue('打开分流测试文本\n')
  states.text = await overlaySnapshot(page)
  await returnToRoot(page)

  await page.locator('.file-card').filter({ hasText: epub.name }).click()
  await expect(page.locator('#reader-view')).toBeVisible()
  await expect(page.locator('#reader-title')).toHaveText('兼容书籍')
  states.epub = await overlaySnapshot(page)
  await returnToRoot(page)

  for (const item of [image, audio, video]) {
    await page.locator('.file-card').filter({ hasText: item.name }).click()
    await expect(page.locator('.preview-modal')).toBeVisible()
    states[item.id] = await overlaySnapshot(page)
    await returnToRoot(page)
  }

  await page.locator('.file-card').filter({ hasText: unknown.name }).click()
  await expect(page.locator('.preview-modal, #reader-view, .document-editor')).toHaveCount(0)
  states.unknown = await overlaySnapshot(page)
  return states
}

test('旧版与 Rust 版普通文件打开分流及浏览器后退行为一致', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockOpening(oldPage), mockOpening(newPage)])
    const [oldState, newState] = await Promise.all([
      exerciseOpening(oldPage, oldUrl),
      exerciseOpening(newPage, newUrl),
    ])
    expect(newState, 'Rust 文件打开分流或浏览器后退行为与 reference 不一致').toEqual(oldState)
    expect(oldState.folder).toEqual({ path: '/f/open-folder', title: 'revaro · 私人网盘', heading: '资料', preview: null, reader: null, editor: null })
    expect(oldState.text).toMatchObject({ path: '/', heading: '我的文件', editor: 'document-editor' })
    expect(oldState.epub).toMatchObject({ path: '/read/open-book', reader: 'reader-shell' })
    expect(oldState.unknown).toMatchObject({ path: '/', preview: null, reader: null, editor: null })
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})
