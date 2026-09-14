import { expect, test, type BrowserContext, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'
const folder = (id: string, name: string) => ({ id, name })
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
  book: [file({ id: 'book-1', name: '星海拾遗 第1卷.epub', mime_type: 'application/epub+zip', folder_path: [folder('books', '书籍')] })],
  image: [file({ id: 'image-1', name: '参考图片.png', mime_type: 'image/png', folder_path: [folder('photos', '照片')] })],
  video: [file({ id: 'video-1', name: '参考视频.webm', mime_type: 'video/webm', folder_path: [folder('videos', '视频')] })],
  audio: [file({ id: 'audio-1', name: '参考音频.mp3', mime_type: 'audio/mpeg', duration_ms: 125000, folder_path: [folder('music', '音乐')] })],
}

const counts = { book: 1, image: 1, video: 1, audio: 1, file: 1 }
const root = file({ id: ROOT, name: '我的文件', kind: 'directory', parent_id: null, size: 0, mime_type: '' })
const rootChildren = [file({ id: 'root-file', name: '根目录.txt', mime_type: 'text/plain' })]
const thumbnail = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 2 2"><rect width="2" height="2" fill="#41628a"/></svg>'

async function mockLibrary(page: Page) {
  await page.route('**/api/**', async route => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') return json({ items: library, counts })
    if (path === '/api/library/counts') return json(counts)
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: rootChildren, total_bytes: 1000, file_count: 1 })
    if (path.endsWith('/audio')) return json({ duration: 125, chapters: [], cover_url: '', has_cover: false })
    if (path.endsWith('/thumbnail') || path.endsWith('/preview') || path.endsWith('/cover')) {
      return route.fulfill({ contentType: 'image/svg+xml', body: thumbnail })
    }
    return json({ items: [] })
  })
}

async function openShell(page: Page, url: string) {
  await page.goto(`${url}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
}

async function clearPreferences(context: BrowserContext) {
  await context.addInitScript(() => {
    for (const key of ['revaro:sidebar:collapsed', 'revaro:sidebar:expanded', 'revaro:library:gallery:image', 'revaro:library:gallery:video', 'revaro:library:media:audio']) {
      localStorage.removeItem(key)
    }
  })
}

async function headerSnapshot(page: Page) {
  return page.locator('.library-head').evaluate(element => ({
    eyebrow: element.querySelector('.eyebrow')?.textContent?.trim(),
    title: element.querySelector('h1')?.textContent?.trim(),
    meta: element.querySelector('.folder-meta')?.textContent?.replace(/\s+/g, ' ').trim(),
    actions: Array.from(element.querySelectorAll(':scope > .actions > button, :scope > .actions > .view-switch button')).map(button => ({
      text: button.textContent?.replace(/\s+/g, ' ').trim(),
      className: button.className,
      pressed: button.getAttribute('aria-pressed'),
    })),
  }))
}

async function itemSnapshot(page: Page) {
  return page.locator('.library-view').evaluate(element => ({
    cards: Array.from(element.querySelectorAll('.file-card')).map(card => ({
      text: card.textContent?.replace(/\s+/g, ' ').trim(),
      className: card.className.split(/\s+/).sort().join(' '),
      previewTitle: card.querySelector('.card-preview')?.getAttribute('title'),
    })),
    rows: Array.from(element.querySelectorAll('.file-row')).map(row => ({
      text: row.textContent?.replace(/\s+/g, ' ').trim(),
      className: row.className.split(/\s+/).sort().join(' '),
    })),
    books: Array.from(element.querySelectorAll('.shelf-card')).map(card => ({
      text: card.textContent?.replace(/\s+/g, ' ').trim(),
      className: card.className,
      title: card.getAttribute('title'),
    })),
    albums: Array.from(element.querySelectorAll('.album')).map(album => album.textContent?.replace(/\s+/g, ' ').trim()),
  }))
}

async function contextMenuIsPrevented(page: Page, selector: string) {
  return page.locator(selector).first().evaluate(element => {
    const event = new MouseEvent('contextmenu', { bubbles: true, cancelable: true, button: 2 })
    element.dispatchEvent(event)
    return event.defaultPrevented
  })
}

async function compareCategory(oldPage: Page, newPage: Page, category: string) {
  await Promise.all([
    oldPage.locator(`[data-category="${category}"]`).click(),
    newPage.locator(`[data-category="${category}"]`).click(),
  ])
  await expect(oldPage.locator('.library-view')).toBeVisible()
  await expect(newPage.locator('.library-view')).toBeVisible()
  expect(await headerSnapshot(newPage), `${category} 分类头部应保持 reference`).toEqual(await headerSnapshot(oldPage))
  expect(await itemSnapshot(newPage), `${category} 分类内容应保持 reference`).toEqual(await itemSnapshot(oldPage))
}

test('旧版与 Rust 版分类页保留相同内容视图与右键交互', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 950 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 950 } })
  await clearPreferences(oldContext)
  await clearPreferences(newContext)
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockLibrary(oldPage), mockLibrary(newPage)])
    await Promise.all([openShell(oldPage, oldUrl), openShell(newPage, newUrl)])

    await compareCategory(oldPage, newPage, 'book')
    await compareCategory(oldPage, newPage, 'image')
    expect(await contextMenuIsPrevented(newPage, '.library-view .file-card')).toBe(
      await contextMenuIsPrevented(oldPage, '.library-view .file-card'),
    )

    await oldPage.getByRole('button', { name: '图库分类' }).click()
    await newPage.getByRole('button', { name: '图库分类' }).click()
    expect(await itemSnapshot(newPage), '图库分类内容应保持 reference').toEqual(await itemSnapshot(oldPage))

    await compareCategory(oldPage, newPage, 'audio')
    await oldPage.getByRole('button', { name: '列表' }).click()
    await newPage.getByRole('button', { name: '列表' }).click()
    expect(await itemSnapshot(newPage), '音乐列表内容应保持 reference').toEqual(await itemSnapshot(oldPage))
    expect(await contextMenuIsPrevented(newPage, '.library-view .file-row')).toBe(
      await contextMenuIsPrevented(oldPage, '.library-view .file-row'),
    )

    await compareCategory(oldPage, newPage, 'video')
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})
