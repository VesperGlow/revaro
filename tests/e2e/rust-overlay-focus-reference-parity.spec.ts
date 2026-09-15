import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
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

const images = [
  { id: 'focus-image-1', parent_id: ROOT, name: '焦点图片.png', kind: 'file', size: 100, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: 'image/png' },
  { id: 'focus-image-2', parent_id: ROOT, name: '另一张图片.png', kind: 'file', size: 100, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: 'image/png' },
]

const book = {
  id: 'focus-book',
  parent_id: ROOT,
  name: '焦点书籍.epub',
  kind: 'file',
  size: 1000,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'application/epub+zip',
}

function focusName(page: Page) {
  return page.evaluate(() => {
    const element = document.activeElement as HTMLElement | null
    if (!element) return 'none'
    if (element.matches('.file-card')) return 'file-card'
    if (element.matches('.preview-modal')) return 'preview-modal'
    if (element.matches('.preview-commandbar summary')) return 'preview-menu-summary'
    if (element.matches('.preview-close')) return 'preview-close'
    if (element.matches('.preview-nav.preview-prev')) return 'preview-prev'
    if (element.matches('.preview-nav.preview-next')) return 'preview-next'
    if (element.matches('#reader-view')) return 'reader-view'
    if (element.id) return `#${element.id}`
    return `${element.tagName.toLowerCase()}.${element.className}`
  })
}

async function installFocusProbe(page: Page) {
  await page.addInitScript(() => {
    const original = HTMLElement.prototype.focus
    ;(window as any).__focusRestoreCalls = []
    ;(window as any).__readerFocusCalls = []
    HTMLElement.prototype.focus = function (...args: any[]) {
      if (this.matches('.file-card')) {
        ;(window as any).__focusRestoreCalls.push({
          connected: this.isConnected,
          options: args[0] ? { preventScroll: args[0].preventScroll === true } : null,
        })
      }
      if (this.id === 'toc-close') {
        ;(window as any).__readerFocusCalls.push({
          options: args[0] ? { preventScroll: args[0].preventScroll === true } : null,
        })
      }
      return original.apply(this, args)
    }
  })
}

function focusRestoreCalls(page: Page) {
  return page.evaluate(() => (window as any).__focusRestoreCalls ?? [])
}

async function clearFocusProbe(page: Page) {
  await page.evaluate(() => {
    ;(window as any).__focusRestoreCalls = []
    ;(window as any).__readerFocusCalls = []
  })
}

function readerFocusCalls(page: Page) {
  return page.evaluate(() => (window as any).__readerFocusCalls ?? [])
}

async function mockMedia(page: Page) {
  const image = '<svg xmlns="http://www.w3.org/2000/svg" width="900" height="600"><rect width="900" height="600" fill="#aac"/></svg>'
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 2, video: 0, audio: 0, file: 0 } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 2, video: 0, audio: 0, file: 0 })
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: images, total_bytes: 200, file_count: 2 })
    if (path.endsWith('/thumbnail') || path.endsWith('/preview')) {
      return route.fulfill({ contentType: 'image/svg+xml', body: image })
    }
    return json({ items: [] })
  })
}

function chunkHtml(index: number) {
  return Array.from({ length: 20 }, (_, block) => `<p data-block="${index * 20 + block}">焦点阅读器第 ${index * 20 + block} 段内容，用于验证阅读器的键盘焦点边界。</p>`).join('')
}

async function mockReader(page: Page) {
  const manifest = {
    version: 4,
    format: 'epub',
    book_key: 'focus-book-key',
    total_chars: 1600,
    spines: [{ block_start: 0, block_count: 40 }],
    chunks: [
      { index: 0, block_start: 0, block_count: 20, chars: 800 },
      { index: 1, block_start: 20, block_count: 20, chars: 800 },
    ],
    toc: [{ label: '开头', depth: 0, spine: 0, block: 0, chunk: 0, text_path: [0], text_offset: 0 }],
  }
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
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 1, image: 0, video: 0, audio: 0, file: 0 } })
    }
    if (path === '/api/library/counts') return json({ book: 1, image: 0, video: 0, audio: 0, file: 0 })
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: [book], total_bytes: book.size, file_count: 1 })
    if (path === `/api/files/${book.id}`) return json({ file: book, breadcrumbs: [] })
    if (path === `/api/files/${book.id}/thumbnail`) return route.fulfill({ status: 404, body: '' })
    if (path === `/api/files/${book.id}/book`) return json({ format: 'epub', title: '焦点书籍', name: book.name, cover: false, toc: [] })
    if (path === `/api/files/${book.id}/book/progress`) {
      if (request.method() === 'PUT') return route.fulfill({ status: 204, body: '' })
      return json({})
    }
    if (path === `/api/files/${book.id}/book/flow`) return json(manifest)
    const chunk = path.match(new RegExp(`^/api/files/${book.id}/book/flow/chunks/(\\d+)$`))
    if (chunk) return route.fulfill({ contentType: 'text/html; charset=utf-8', body: chunkHtml(Number(chunk[1])) })
    return json({ items: [] })
  })
}

async function exerciseMedia(page: Page, baseUrl: string) {
  await mockMedia(page)
  await installFocusProbe(page)
  await page.goto(`${baseUrl}/?overlay-focus-media=${Date.now()}`)
  const card = page.locator('.file-card').filter({ hasText: '焦点图片.png' })
  await card.click()
  await expect(page.locator('.preview-modal')).toBeVisible()
  await expect(page.locator('.preview-image')).toBeVisible()
  await expect(page.locator('.preview-modal')).toBeFocused()
  const initial = await focusName(page)

  await page.keyboard.press('Tab')
  const first = await focusName(page)
  await page.keyboard.press('Shift+Tab')
  const last = await focusName(page)

  await page.locator('.preview-commandbar summary').click()
  await page.keyboard.press('Escape')
  const menuEscape = {
    open: await page.locator('.preview-commandbar details[open]').count(),
    focus: await focusName(page),
  }
  await page.keyboard.press('Escape')
  await expect(page.locator('.preview-modal')).toHaveCount(0)
  const connectedRestore = await focusRestoreCalls(page)
  const restored = await focusName(page)

  await card.click()
  await expect(page.locator('.preview-modal')).toBeVisible()
  await clearFocusProbe(page)
  await page.evaluate(() => document.querySelector('.file-card')?.remove())
  await page.keyboard.press('Escape')
  await expect(page.locator('.preview-modal')).toHaveCount(0)
  const disconnectedRestore = await focusRestoreCalls(page)

  return { initial, first, last, menuEscape, restored, overflow: await page.evaluate(() => document.body.style.overflow), connectedRestore, disconnectedRestore }
}

async function exerciseReader(page: Page, baseUrl: string) {
  await mockReader(page)
  await installFocusProbe(page)
  await page.goto(`${baseUrl}/?overlay-focus-reader=${Date.now()}`)
  await page.locator('.file-card').filter({ hasText: book.name }).click()
  await expect(page.locator('#reader-view')).toBeVisible()
  await expect(page.locator('#loading')).toBeHidden({ timeout: 20_000 })
  await expect(page.locator('#reader-view')).toBeFocused()
  const initial = await focusName(page)

  await page.keyboard.press('Tab')
  const first = await focusName(page)
  await page.keyboard.press('Shift+Tab')
  const last = await focusName(page)

  await clearFocusProbe(page)
  await page.locator('#toc-button').click()
  await expect(page.locator('#toc-close')).toBeFocused()
  const tocFocus = await readerFocusCalls(page)
  await page.keyboard.press('Shift+Tab')
  const drawerShiftTab = await page.evaluate(() => Boolean(document.activeElement?.closest('#toc-drawer')))
  await page.keyboard.press('Escape')
  const tocEscape = { open: await page.locator('#toc-drawer.open').count(), focus: await focusName(page) }
  await page.keyboard.press('Escape')
  await expect(page.locator('#reader-view')).toHaveCount(0)
  const connectedRestore = await focusRestoreCalls(page)
  const restored = await focusName(page)

  await page.locator('.file-card').filter({ hasText: book.name }).click()
  await expect(page.locator('#reader-view')).toBeVisible()
  await clearFocusProbe(page)
  await page.evaluate(() => document.querySelector('.file-card')?.remove())
  await page.keyboard.press('Escape')
  await expect(page.locator('#reader-view')).toHaveCount(0)
  const disconnectedRestore = await focusRestoreCalls(page)

  return { initial, first, last, tocFocus, drawerShiftTab, tocEscape, restored, overflow: await page.evaluate(() => document.body.style.overflow), connectedRestore, disconnectedRestore }
}

for (const [name, exercise] of [['媒体预览', exerciseMedia], ['阅读器', exerciseReader] ] as const) {
  test(`old/new ${name} 的 Tab 陷阱、Escape 层级与关闭后焦点保持一致`, async ({ browser }) => {
    const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
    const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
    const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
    const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
    const oldPage = await oldContext.newPage()
    const newPage = await newContext.newPage()
    try {
      const [oldState, newState] = await Promise.all([
        exercise(oldPage, oldUrl),
        exercise(newPage, newUrl),
      ])
      expect(newState, `Rust ${name} 焦点与 Escape 行为和 reference 不一致`).toEqual(oldState)
      expect(oldState.restored).toContain(name === '媒体预览' ? 'file-card' : 'file-card')
      expect(oldState.overflow).toBe('')
    } finally {
      await oldContext.close()
      await newContext.close()
    }
  })
}
