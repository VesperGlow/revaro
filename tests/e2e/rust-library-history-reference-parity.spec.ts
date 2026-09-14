import { expect, test, type BrowserContext, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

const file = (value: Record<string, unknown>) => ({
  parent_id: ROOT,
  kind: 'file',
  size: 1000,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'application/octet-stream',
  etag: `etag-${String(value.id ?? 'file')}`,
  ...value,
})

const library = {
  book: [file({ id: 'history-book', name: '历史书籍.epub', mime_type: 'application/epub+zip', folder_path: [{ id: 'books', name: '书籍' }] })],
  image: [
    file({ id: 'history-image-root', name: '根图片.png', mime_type: 'image/png', folder_path: [] }),
    file({ id: 'history-image-photos', name: '照片图片.png', mime_type: 'image/png', folder_path: [{ id: 'photos', name: '照片' }] }),
  ],
  video: [],
  audio: [],
}

const counts = { book: 1, image: 2, video: 0, audio: 0, file: 2 }
const root = file({ id: ROOT, name: '我的文件', kind: 'directory', parent_id: null, size: 0, mime_type: '' })
const rootChildren = [file({ id: 'history-folder', name: '历史目录', kind: 'directory', mime_type: '' })]

async function mockHistory(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') return json({ items: library, counts })
    if (path === '/api/library/counts') return json(counts)
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: rootChildren, total_bytes: 0, file_count: 1 })
    if (path.endsWith('/thumbnail')) {
      return route.fulfill({ contentType: 'image/svg+xml', body: '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 2 2"><rect width="2" height="2" fill="#41628a"/></svg>' })
    }
    return json({ items: [] })
  })
}

async function clearPreferences(context: BrowserContext) {
  await context.addInitScript(() => {
    for (const key of ['revaro:sidebar:collapsed', 'revaro:sidebar:expanded', 'revaro:library:gallery:image']) {
      localStorage.removeItem(key)
    }
  })
}

async function openShell(page: Page, url: string) {
  await page.goto(`${url}/?history-parity=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
}

async function viewSnapshot(page: Page) {
  return page.evaluate(() => ({
    pathname: window.location.pathname,
    heading: document.querySelector('.content-head h1, .library-head h1')?.textContent?.trim(),
    folderMeta: document.querySelector('.content-head .folder-meta, .library-head .folder-meta')?.textContent?.replace(/\s+/g, ' ').trim(),
    cards: Array.from(document.querySelectorAll('.file-card')).map(card => card.textContent?.replace(/\s+/g, ' ').trim()),
    rows: Array.from(document.querySelectorAll('.file-row')).map(row => row.textContent?.replace(/\s+/g, ' ').trim()),
    books: Array.from(document.querySelectorAll('.shelf-card')).map(card => card.textContent?.replace(/\s+/g, ' ').trim()),
  }))
}

async function chooseCategory(page: Page, category: string) {
  await page.locator(`[data-category="${category}"]`).click()
  await expect(page.locator('.library-view, .content-head')).toBeVisible()
}

test('筛选分类后的浏览器返回/前进保持 reference 的页面状态与 URL 结果', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 950 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 950 } })
  await clearPreferences(oldContext)
  await clearPreferences(newContext)
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockHistory(oldPage), mockHistory(newPage)])
    await Promise.all([openShell(oldPage, oldUrl), openShell(newPage, newUrl)])

    await Promise.all([chooseCategory(oldPage, 'image'), chooseCategory(newPage, 'image')])
    await Promise.all([
      expect(oldPage.getByRole('heading', { name: '图片', exact: true })).toBeVisible(),
      expect(newPage.getByRole('heading', { name: '图片', exact: true })).toBeVisible(),
    ])

    await Promise.all([
      oldPage.locator('.category-paths .path-label', { hasText: '照片' }).click(),
      newPage.locator('.category-paths .path-label', { hasText: '照片' }).click(),
    ])
    await Promise.all([
      expect(oldPage.locator('.library-view .file-card')).toHaveCount(1),
      expect(newPage.locator('.library-view .file-card')).toHaveCount(1),
    ])
    const filteredOld = await viewSnapshot(oldPage)
    const filteredNew = await viewSnapshot(newPage)
    expect(filteredNew, 'Rust 分类路径筛选状态与 reference 不一致').toEqual(filteredOld)
    expect(filteredOld.pathname).toBe('/library/image')

    await Promise.all([chooseCategory(oldPage, 'book'), chooseCategory(newPage, 'book')])
    await Promise.all([
      expect(oldPage.getByRole('heading', { name: '书架', exact: true })).toBeVisible(),
      expect(newPage.getByRole('heading', { name: '书架', exact: true })).toBeVisible(),
    ])
    const bookOld = await viewSnapshot(oldPage)
    const bookNew = await viewSnapshot(newPage)
    expect(bookNew, 'Rust 分类切换状态与 reference 不一致').toEqual(bookOld)

    await Promise.all([oldPage.goBack(), newPage.goBack()])
    await Promise.all([
      expect(oldPage.getByRole('heading', { name: '图片', exact: true })).toBeVisible(),
      expect(newPage.getByRole('heading', { name: '图片', exact: true })).toBeVisible(),
    ])
    const returnedImageOld = await viewSnapshot(oldPage)
    const returnedImageNew = await viewSnapshot(newPage)
    expect(returnedImageNew, 'Rust 浏览器返回后的分类状态与 reference 不一致').toEqual(returnedImageOld)
    expect(returnedImageOld.pathname).toBe('/library/image')
    expect(returnedImageOld.folderMeta).toContain('全部位置')
    expect(returnedImageOld.cards).toHaveLength(2)

    await Promise.all([oldPage.goBack(), newPage.goBack()])
    await Promise.all([
      expect(oldPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible(),
      expect(newPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible(),
    ])
    const returnedRootOld = await viewSnapshot(oldPage)
    const returnedRootNew = await viewSnapshot(newPage)
    expect(returnedRootNew, 'Rust 返回普通目录后的状态与 reference 不一致').toEqual(returnedRootOld)
    expect(returnedRootOld.pathname).toBe('/')

    await Promise.all([oldPage.goForward(), newPage.goForward()])
    await pageSettled(oldPage)
    await pageSettled(newPage)
    expect(await viewSnapshot(newPage), 'Rust 浏览器前进结果与 reference 不一致').toEqual(await viewSnapshot(oldPage))
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

async function pageSettled(page: Page) {
  await page.waitForTimeout(180)
}
