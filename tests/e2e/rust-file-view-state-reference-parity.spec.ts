import { expect, test, type BrowserContext, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const VIEW_KEY = 'revaro:library:media:file'
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

const items = Array.from({ length: 40 }, (_, index) => ({
  id: `view-state-${index}`,
  parent_id: ROOT,
  name: `视图状态-${String(index + 1).padStart(2, '0')}.txt`,
  kind: 'file',
  size: 1024 + index,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'text/plain',
  etag: `view-state-etag-${index}`,
}))

async function mockShell(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: items.length } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: items.length })
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) {
      return json({ items, total_bytes: items.reduce((total, item) => total + item.size, 0), file_count: items.length })
    }
    if (path === '/api/trash') return json({ items: [], total_bytes: 0, file_count: 0 })
    return json({ items: [] })
  })
}

async function prepare(context: BrowserContext, page: Page) {
  await context.addInitScript(key => localStorage.removeItem(key), VIEW_KEY)
  await mockShell(page)
}

async function openRoot(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?view-state-reference=${crypto.randomUUID()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await expect(page.locator('.file-card')).toHaveCount(items.length)
}

async function snapshot(page: Page) {
  return page.evaluate(key => {
    const button = (title: string) => document.querySelector<HTMLButtonElement>(`button[title="${title}"]`)
    const grid = document.querySelector<HTMLElement>('.file-grid')
    const rows = document.querySelector<HTMLElement>('.file-rows')
    return {
      buttons: ['方块视图', '列表视图'].map(title => {
        const element = button(title)
        return {
          title,
          className: element?.className ?? null,
          pressed: element?.getAttribute('aria-pressed') ?? null,
          disabled: element?.disabled ?? null,
          svgPath: element?.querySelector('svg path')?.getAttribute('d') ?? null,
        }
      }),
      gridVisible: !!grid,
      rowVisible: !!rows,
      cardCount: document.querySelectorAll('.file-card').length,
      rowCount: document.querySelectorAll('.file-row').length,
      scrollY: window.scrollY,
      bodyScrollHeight: document.body.scrollHeight,
      storedMode: localStorage.getItem(key),
    }
  }, VIEW_KEY)
}

test('文件视图切换的 active、持久化、滚动和非法偏好保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([prepare(oldContext, oldPage), prepare(newContext, newPage)])
    await Promise.all([openRoot(oldPage, oldUrl), openRoot(newPage, newUrl)])
    expect(await snapshot(newPage), 'Rust 默认方块视图与 reference 不一致').toEqual(await snapshot(oldPage))

    await Promise.all([
      oldPage.evaluate(() => window.scrollTo(0, 420)),
      newPage.evaluate(() => window.scrollTo(0, 420)),
    ])
    await Promise.all([
      oldPage.getByTitle('列表视图').click(),
      newPage.getByTitle('列表视图').click(),
    ])
    await Promise.all([
      expect(oldPage.locator('.file-row')).toHaveCount(items.length),
      expect(newPage.locator('.file-row')).toHaveCount(items.length),
    ])
    expect(await snapshot(newPage), 'Rust 列表切换后的 active/滚动/持久化与 reference 不一致')
      .toEqual(await snapshot(oldPage))

    await Promise.all([
      oldPage.getByTitle('方块视图').click(),
      newPage.getByTitle('方块视图').click(),
    ])
    await expect(oldPage.locator('.file-card')).toHaveCount(items.length)
    await expect(newPage.locator('.file-card')).toHaveCount(items.length)
    expect(await snapshot(newPage), 'Rust 方块切回后的 active/滚动/持久化与 reference 不一致')
      .toEqual(await snapshot(oldPage))

    await Promise.all([
      oldPage.evaluate(key => localStorage.setItem(key, 'invalid'), VIEW_KEY),
      newPage.evaluate(key => localStorage.setItem(key, 'invalid'), VIEW_KEY),
      oldPage.reload(),
      newPage.reload(),
    ])
    await Promise.all([
      expect(oldPage.locator('.file-card')).toHaveCount(items.length),
      expect(newPage.locator('.file-card')).toHaveCount(items.length),
    ])
    expect(await snapshot(newPage), 'Rust 非法视图偏好处理与 reference 不一致')
      .toEqual(await snapshot(oldPage))
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})
