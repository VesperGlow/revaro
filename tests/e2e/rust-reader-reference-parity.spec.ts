import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const BOOK_ID = 'reader-reference-book'
const STAMP = '2026-01-01T00:00:00Z'

const book = {
  id: BOOK_ID,
  parent_id: ROOT,
  name: '旧版阅读器对照.epub',
  kind: 'file',
  size: 1000,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'application/epub+zip',
}

type TocEntry = {
  label: string
  depth: number
  spine: number
  block: number
  chunk: number
  text_path: number[]
  text_offset: number
}

type ReaderFixture = {
  toc?: TocEntry[]
  flowToc?: TocEntry[] | null
  rawFlowToc?: unknown
  flowSpines?: unknown
  flowMetadata?: Record<string, unknown>
  flowChunkMetadata?: Record<string, unknown>
  flowFailure?: boolean
  bookMetadata?: Record<string, unknown>
}

type ReaderPrefsFixture = {
  fontSize: number
  lineHeight: number
  theme: 'light' | 'dark'
}

const defaultToc: TocEntry[] = [
  { label: '第一章', depth: 0, spine: 0, block: 0, chunk: 0, text_path: [0], text_offset: 0 },
  { label: '第二章', depth: 2, spine: 1, block: 20, chunk: 1, text_path: [0], text_offset: 0 },
]

function chunkHtml(index: number) {
  return Array.from({ length: 20 }, (_, block) => {
    const number = index * 20 + block
    const spineStart = number > 0 && number % 20 === 0 ? ' data-spine-start' : ''
    return `<p data-block="${number}"${spineStart}>这是阅读器 parity 的第 ${number} 段文字。它足够长，能够在移动端形成多个分页，用于验证设置重排和横向翻页行为。</p>`
  }).join('')
}

async function mockReader(page: Page, fixture: ReaderFixture = {}) {
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
  const manifest = {
    version: 4,
    format: 'epub',
    book_key: 'reader-reference-book-key',
    total_chars: 3200,
    spines: fixture.flowSpines ?? [
      { block_start: 0, block_count: 20 },
      { block_start: 20, block_count: 20 },
      { block_start: 40, block_count: 20 },
      { block_start: 60, block_count: 20 },
    ],
    chunks: Array.from({ length: 4 }, (_, index) => ({
      index,
      block_start: index * 20,
      block_count: 20,
      chars: 800,
      ...(index === 0 ? fixture.flowChunkMetadata : {}),
    })),
    ...fixture.flowMetadata,
    toc: 'rawFlowToc' in fixture
      ? fixture.rawFlowToc
      : 'flowToc' in fixture
        ? fixture.flowToc
        : fixture.toc ?? defaultToc,
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
    if (path === '/api/library/all') return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 1, image: 0, video: 0, audio: 0, file: 0 } })
    if (path === '/api/library/counts') return json({ book: 1, image: 0, video: 0, audio: 0, file: 0 })
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: [book], total_bytes: book.size, file_count: 1 })
    if (path === `/api/files/${BOOK_ID}`) return json({ file: book, breadcrumbs: [root] })
    if (path === `/api/files/${BOOK_ID}/thumbnail`) return route.fulfill({ status: 404, body: '' })
    if (path === `/api/files/${BOOK_ID}/book`) {
      return json(fixture.bookMetadata ?? { format: 'epub', title: '旧版阅读器对照', name: book.name, cover: false, toc: [] })
    }
    if (path === `/api/files/${BOOK_ID}/book/progress`) {
      if (request.method() === 'PUT') return route.fulfill({ status: 204, body: '' })
      return json({})
    }
    if (path === `/api/files/${BOOK_ID}/book/flow`) {
      if (fixture.flowFailure) return route.fulfill({ status: 503, json: { error: { status: 503, message: '阅读流暂时不可用' } } })
      return json(manifest)
    }
    const chunk = path.match(new RegExp(`^/api/files/${BOOK_ID}/book/flow/chunks/(\\d+)$`))
    if (chunk) return route.fulfill({ contentType: 'text/html; charset=utf-8', body: chunkHtml(Number(chunk[1])) })
    return json({ items: [] })
  })
}

async function prepare(page: Page, baseUrl: string, fixture: ReaderFixture = {}, prefs: ReaderPrefsFixture | null = { fontSize: 21, lineHeight: 1.4, theme: 'light' }) {
  await page.addInitScript(value => {
    if (value === null) localStorage.removeItem('revaro-reader-prefs')
    else localStorage.setItem('revaro-reader-prefs', JSON.stringify(value))
  }, prefs)
  await mockReader(page, fixture)
  await page.goto(`${baseUrl}/?reader-reference=${Date.now()}`)
  if (fixture.flowFailure) {
    await page.evaluate(bookId => new Promise<void>(resolve => {
      localStorage.removeItem(`revaro-reader-manifest:${bookId}`)
      const clear = () => {
        const request = indexedDB.deleteDatabase('revaro-reader-cache')
        request.onsuccess = () => resolve()
        request.onerror = () => resolve()
        request.onblocked = () => resolve()
      }
      if ('caches' in window) void caches.delete('revaro-reader-flow-v1').then(clear)
      else clear()
    }), BOOK_ID)
  }
  await page.locator('.file-card').filter({ hasText: book.name }).click()
  await expect(page.locator('#reader-view')).toBeVisible()
  await expect(page.locator('#loading')).toBeHidden({ timeout: 20_000 })
  if (!fixture.flowFailure) {
    await expect(page.locator('#flow .rf-chunk').first()).toBeVisible({ timeout: 20_000 })
  }
}

test('old/new 阅读器书籍成功响应缺少未使用元数据字段时仍使用 name/title', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext()
  const newContext = await browser.newContext()
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const fixture = { bookMetadata: { name: '服务端书名' } }

  try {
    await Promise.all([
      prepare(oldPage, oldUrl, fixture),
      prepare(newPage, newUrl, fixture),
    ])
    await Promise.all([
      expect(oldPage.locator('#reader-title')).toHaveText('服务端书名'),
      expect(newPage.locator('#reader-title')).toHaveText('服务端书名'),
    ])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('old/new 阅读器书籍成功响应的未使用元数据为 null 时仍使用 name/title', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext()
  const newContext = await browser.newContext()
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const fixture = {
    bookMetadata: { format: null, title: null, name: '服务端书名', cover: null, toc: null },
  }

  try {
    await Promise.all([
      prepare(oldPage, oldUrl, fixture),
      prepare(newPage, newUrl, fixture),
    ])
    await Promise.all([
      expect(oldPage.locator('#reader-title')).toHaveText('服务端书名'),
      expect(newPage.locator('#reader-title')).toHaveText('服务端书名'),
    ])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('old/new 阅读器书籍 metadata 的 TOC 条目字段为 null 时仍可加载 flow', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext()
  const newContext = await browser.newContext()
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const fixture = {
    bookMetadata: {
      name: '服务端书名',
      toc: [{ label: null, path: null, fragment: null, offset: null, depth: null }],
    },
  }

  try {
    await Promise.all([
      prepare(oldPage, oldUrl, fixture),
      prepare(newPage, newUrl, fixture),
    ])
    await Promise.all([
      expect(oldPage.locator('#flow .rf-chunk').first()).toBeVisible(),
      expect(newPage.locator('#flow .rf-chunk').first()).toBeVisible(),
      expect(oldPage.locator('#reader-title')).toHaveText('服务端书名'),
      expect(newPage.locator('#reader-title')).toHaveText('服务端书名'),
    ])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('old/new 阅读器 flow 的 toc 为 null 时仍显示空目录', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext()
  const newContext = await browser.newContext()
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const fixture = { flowToc: null }

  try {
    await Promise.all([
      prepare(oldPage, oldUrl, fixture),
      prepare(newPage, newUrl, fixture),
    ])
    await Promise.all([
      oldPage.locator('#toc-button').click(),
      newPage.locator('#toc-button').click(),
    ])
    await Promise.all([
      expect(oldPage.locator('.toc-empty')).toHaveText('这本书没有可用目录。'),
      expect(newPage.locator('.toc-empty')).toHaveText('这本书没有可用目录。'),
    ])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('old/new 阅读器 flow 的 TOC 可选字段为 null 时仍保留目录条目', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext()
  const newContext = await browser.newContext()
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const fixture = {
    rawFlowToc: [{
      label: '空值字段章节',
      depth: null,
      spine: 0,
      block: 0,
      chunk: null,
      nav_anchor: null,
      text_path: null,
      text_offset: null,
      source_path: null,
      source_fragment: null,
    }],
  }

  try {
    await Promise.all([
      prepare(oldPage, oldUrl, fixture),
      prepare(newPage, newUrl, fixture),
    ])
    await Promise.all([
      oldPage.locator('#toc-button').click(),
      newPage.locator('#toc-button').click(),
    ])
    await Promise.all([
      expect(oldPage.locator('.toc-item')).toHaveText('空值字段章节'),
      expect(newPage.locator('.toc-item')).toHaveText('空值字段章节'),
    ])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('old/new 阅读器 flow 的 spine 范围字段为 null 时仍可加载正文', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext()
  const newContext = await browser.newContext()
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const fixture = {
    flowSpines: [
      { block_start: null, block_count: 20 },
      { block_start: 20, block_count: 20 },
      { block_start: 40, block_count: 20 },
      { block_start: 60, block_count: 20 },
      { block_start: 80, block_count: null },
    ],
  }

  try {
    await Promise.all([
      prepare(oldPage, oldUrl, fixture),
      prepare(newPage, newUrl, fixture),
    ])
    await Promise.all([
      expect(oldPage.locator('#flow .rf-chunk').first()).toBeVisible(),
      expect(newPage.locator('#flow .rf-chunk').first()).toBeVisible(),
    ])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('old/new 阅读器 flow 的可选 book_key/generated_at 为 null 时仍可加载', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18184'
  const oldContext = await browser.newContext()
  const newContext = await browser.newContext()
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const fixture = { flowMetadata: { book_key: null, generated_at: null } }

  try {
    await Promise.all([
      prepare(oldPage, oldUrl, fixture),
      prepare(newPage, newUrl, fixture),
    ])
    await Promise.all([
      expect(oldPage.locator('#flow .rf-chunk').first()).toBeVisible(),
      expect(newPage.locator('#flow .rf-chunk').first()).toBeVisible(),
    ])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('old/new 阅读器 flow chunk 的可选 bytes/url 为 null 时仍可加载', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18180'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18184'
  const oldContext = await browser.newContext()
  const newContext = await browser.newContext()
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const fixture = { flowChunkMetadata: { bytes: null, url: null } }

  try {
    await Promise.all([
      prepare(oldPage, oldUrl, fixture),
      prepare(newPage, newUrl, fixture),
    ])
    await Promise.all([
      expect(oldPage.locator('#flow .rf-chunk').first()).toBeVisible(),
      expect(newPage.locator('#flow .rf-chunk').first()).toBeVisible(),
    ])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('old/new 阅读器 flow chunk 的索引和字符数为 null 时仍可加载', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18184'
  const oldContext = await browser.newContext()
  const newContext = await browser.newContext()
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const fixture = {
    flowMetadata: { total_chars: 2400 },
    flowChunkMetadata: { index: null, block_start: null, chars: null },
  }

  try {
    await Promise.all([
      prepare(oldPage, oldUrl, fixture),
      prepare(newPage, newUrl, fixture),
    ])
    await Promise.all([
      expect(oldPage.locator('#flow .rf-chunk').first()).toBeVisible(),
      expect(newPage.locator('#flow .rf-chunk').first()).toBeVisible(),
    ])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

async function readerSnapshot(page: Page) {
  return page.evaluate(() => {
    const reader = document.querySelector('#reader-view') as HTMLElement
    const font = document.querySelector('#font-popover') as HTMLElement
    const drawer = document.querySelector('#toc-drawer') as HTMLElement
    const slider = document.querySelector('#font-slider') as HTMLInputElement
    const lineHeight = Array.from(document.querySelectorAll('.v2-lineheight .font-step'))
      .find(button => button.classList.contains('v2-active'))?.textContent?.trim() ?? null
    const style = (selector: string, property: string) => {
      const element = document.querySelector(selector)
      return element ? getComputedStyle(element).getPropertyValue(property) : null
    }
    return {
      dark: reader.classList.contains('dark'),
      toolsHidden: reader.classList.contains('tools-hidden'),
      fontOpen: getComputedStyle(font).display !== 'none',
      tocOpen: drawer.classList.contains('open'),
      tocHidden: drawer.getAttribute('aria-hidden'),
      fontExpanded: document.querySelector('#font-button')?.getAttribute('aria-expanded'),
      tocExpanded: document.querySelector('#toc-button')?.getAttribute('aria-expanded'),
      fontValue: slider.value,
      lineHeight,
      emptyText: document.querySelector('.toc-empty')?.textContent?.trim() ?? null,
      flowFontSize: style('#flow', 'font-size'),
      flowLineHeight: style('#flow', 'line-height'),
      paper: style('#reader-view .reader-viewport', 'background-color'),
      pageLabel: document.querySelector('#page-label')?.textContent?.trim() ?? null,
    }
  })
}

async function prefsSnapshot(page: Page) {
  return page.evaluate(() => JSON.parse(localStorage.getItem('revaro-reader-prefs') ?? 'null'))
}

test('old/new 阅读器的排版、明暗、工具显隐和空目录行为一致', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 390, height: 844 } })
  const newContext = await browser.newContext({ viewport: { width: 390, height: 844 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await prepare(page, baseUrl, { toc: [] })
    const initial = await readerSnapshot(page)

    await page.locator('#font-button').click()
    await expect(page.locator('#font-popover')).toBeVisible()
    const fontOpen = await readerSnapshot(page)
    await page.locator('#font-larger').click()
    await page.locator('.v2-lineheight .font-step').nth(2).click()
    await page.locator('#theme-button').click()
    await page.waitForTimeout(700)
    const changed = await readerSnapshot(page)
    const changedPrefs = await prefsSnapshot(page)

    await page.locator('#center-zone').click()
    const hidden = await readerSnapshot(page)
    await page.locator('#center-zone').click()
    const shown = await readerSnapshot(page)

    await page.locator('#toc-button').click()
    await expect(page.locator('.toc-empty')).toHaveText('这本书没有可用目录。')
    const emptyOpen = await readerSnapshot(page)
    await page.mouse.click(5, 400)
    const emptyClosed = await readerSnapshot(page)
    return { initial, fontOpen, changed, changedPrefs, hidden, shown, emptyOpen, emptyClosed }
  }

  try {
    const [oldState, newState] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(newState, 'Rust 阅读器排版/主题/工具与空目录行为和 reference 不一致').toEqual(oldState)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

async function touchSwipe(page: Page, start: { x: number; y: number }, end: { x: number; y: number }) {
  const client = await page.context().newCDPSession(page)
  await client.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ id: 1, x: start.x, y: start.y }] })
  await client.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ id: 1, x: end.x, y: end.y }] })
  await client.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
  await client.detach()
}

async function touchCancel(page: Page, start: { x: number; y: number }, end: { x: number; y: number }) {
  const client = await page.context().newCDPSession(page)
  await client.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ id: 1, x: start.x, y: start.y }] })
  await client.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ id: 1, x: end.x, y: end.y }] })
  await client.send('Input.dispatchTouchEvent', { type: 'touchCancel', touchPoints: [] })
  await client.detach()
}

async function pageState(page: Page) {
  return page.evaluate(() => ({
    label: document.querySelector('#page-label')?.textContent?.trim() ?? null,
    transform: (document.querySelector('#flow') as HTMLElement | null)?.style.transform ?? null,
    transition: (document.querySelector('#flow') as HTMLElement | null)?.style.transition ?? null,
  }))
}

test('old/new 阅读器 390px 横向触摸翻页，纵向手势不改变阅读页', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true })
  const newContext = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await prepare(page, baseUrl)
    const before = await pageState(page)
    await touchSwipe(page, { x: 300, y: 400 }, { x: 80, y: 400 })
    await page.waitForTimeout(450)
    const horizontal = await pageState(page)
    await touchSwipe(page, { x: 180, y: 400 }, { x: 190, y: 700 })
    await page.waitForTimeout(120)
    const vertical = await pageState(page)
    return { before, horizontal, vertical }
  }

  try {
    const [oldState, newState] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(newState, 'Rust 阅读器触摸翻页和 reference 不一致').toEqual(oldState)
    expect(newState.horizontal.transform).not.toBe(newState.before.transform)
    expect(newState.vertical.label).toBe(newState.horizontal.label)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 阅读器触摸取消恢复当前栏，不误翻页', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true })
  const newContext = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await prepare(page, baseUrl)
    const before = await pageState(page)
    await touchCancel(page, { x: 300, y: 400 }, { x: 80, y: 400 })
    await page.waitForTimeout(120)
    return { before, after: await pageState(page) }
  }

  try {
    const [oldState, newState] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(newState, 'Rust 阅读器 touch-cancel 恢复和 reference 不一致').toEqual(oldState)
    expect(newState.after.label).toBe(newState.before.label)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 阅读器目录层级、活动项和错误关闭入口一致', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldErrorContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newErrorContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const oldErrorPage = await oldErrorContext.newPage()
  const newErrorPage = await newErrorContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await prepare(page, baseUrl)
    await page.locator('#toc-button').click()
    await expect(page.locator('#toc-drawer')).toHaveClass(/open/)
    const entries = await page.locator('.toc-item').evaluateAll(items => items.map(item => ({
      text: item.textContent?.trim(),
      indent: getComputedStyle(item).paddingLeft,
      active: item.classList.contains('active'),
    })))
    await page.locator('.toc-item').nth(1).click()
    await page.waitForTimeout(350)
    const selected = await page.locator('.toc-item').evaluateAll(items => items.map(item => item.classList.contains('active')))
    return { entries, selected }
  }

  async function exerciseError(page: Page, baseUrl: string) {
    await prepare(page, baseUrl, { flowFailure: true })
    const error = await page.locator('.reader-loading').last().evaluate(element => ({
      text: element.textContent?.replace(/\\s+/g, ' ').trim(),
      buttons: Array.from(element.querySelectorAll('button')).map(button => button.textContent?.trim()),
    }))
    await page.locator('.reader-loading button').click()
    return { error, closed: await page.locator('#reader-view').count() === 0 }
  }

  try {
    const [oldState, newState] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(newState, 'Rust 阅读器目录层级/活动项和 reference 不一致').toEqual(oldState)

    const [oldError, newError] = await Promise.all([
      exerciseError(oldErrorPage, oldUrl),
      exerciseError(newErrorPage, newUrl),
    ])
    expect(newError, 'Rust 阅读器错误关闭和 reference 不一致').toEqual(oldError)
  } finally {
    await Promise.all([oldContext.close(), newContext.close(), oldErrorContext.close(), newErrorContext.close()])
  }
})

test('old/new 阅读器默认偏好、字号边界持久化和分层 Escape 行为一致', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    // null means that the browser starts without the preference key, so this
    // checks the actual reference defaults instead of the fixture defaults used
    // by the layout tests above.
    await prepare(page, baseUrl, {}, null)
    const initial = await readerSnapshot(page)

    await page.locator('#font-button').click()
    for (let i = 0; i < 20; i++) await page.locator('#font-smaller').click()
    const minimum = await page.evaluate(() => ({
      value: (document.querySelector('#font-slider') as HTMLInputElement).value,
      smallerDisabled: (document.querySelector('#font-smaller') as HTMLButtonElement).disabled,
      largerDisabled: (document.querySelector('#font-larger') as HTMLButtonElement).disabled,
    }))
    for (let i = 0; i < 30; i++) await page.locator('#font-larger').click()
    const maximum = await page.evaluate(() => ({
      value: (document.querySelector('#font-slider') as HTMLInputElement).value,
      smallerDisabled: (document.querySelector('#font-smaller') as HTMLButtonElement).disabled,
      largerDisabled: (document.querySelector('#font-larger') as HTMLButtonElement).disabled,
    }))

    await page.locator('.v2-lineheight .font-step').first().click()
    await page.locator('#theme-button').click()
    await expect
      .poll(() => page.locator('#flow').evaluate(element => getComputedStyle(element).fontSize), { timeout: 3000 })
      .toBe('32px')
    const changed = await readerSnapshot(page)
    const saved = await prefsSnapshot(page)

    await page.locator('#toc-button').click()
    await expect(page.locator('#toc-drawer')).toHaveClass(/open/)
    await page.keyboard.press('Escape')
    const tocEscape = await readerSnapshot(page)

    await page.locator('#font-button').click()
    await expect(page.locator('#font-popover')).toBeVisible()
    await page.keyboard.press('Escape')
    const fontEscape = await readerSnapshot(page)

    await page.keyboard.press('Escape')
    await expect(page.locator('#reader-view')).toHaveCount(0)
    await page.locator('.file-card').filter({ hasText: book.name }).click()
    await expect(page.locator('#reader-view')).toBeVisible()
    await expect(page.locator('#loading')).toBeHidden({ timeout: 20_000 })
    const restored = await readerSnapshot(page)
    return { initial, minimum, maximum, changed, saved, tocEscape, fontEscape, restored }
  }

  try {
    const [oldState, newState] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(newState, 'Rust 阅读器默认偏好、边界、持久化或 Escape 层级和 reference 不一致').toEqual(oldState)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})
