import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

const base = (value: Record<string, unknown>) => ({
  parent_id: ROOT,
  kind: 'file',
  size: 4096,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'application/octet-stream',
  etag: `etag-${String(value.id ?? 'item')}`,
  has_cover: false,
  ...value,
})

const items = [
  base({ id: 'dir', name: '普通目录', kind: 'directory', size: 0, mime_type: '' }),
  base({ id: 'dir-epub', name: '目录.epub', kind: 'directory', size: 0, mime_type: '' }),
  base({ id: 'txt', name: '笔记.txt', mime_type: 'text/plain' }),
  base({ id: 'epub', name: '书籍.epub', mime_type: 'application/epub+zip' }),
  base({ id: 'image', name: '图片.png', mime_type: 'image/png' }),
  base({ id: 'audio-cover', name: '带封面.mp3', mime_type: 'audio/mpeg', has_cover: true }),
  base({ id: 'audio', name: '无封面.mp3', mime_type: 'audio/mpeg' }),
  base({ id: 'video', name: '视频.webm', mime_type: 'video/webm' }),
  base({ id: 'archive', name: '归档.zip', mime_type: 'application/zip' }),
  base({ id: 'unknown', name: '未知.pdf', mime_type: 'application/pdf' }),
  base({ id: 'pending-epub', name: '处理中.epub', status: 'pending', mime_type: 'application/epub+zip' }),
  base({ id: 'failed-image', name: '失败.png', status: 'failed', mime_type: 'image/png' }),
]

const cover = '<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40"><rect width="40" height="40" fill="#789"/></svg>'

async function mockFileBrowser(page: Page) {
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
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: base({ id: ROOT, name: '我的文件', kind: 'directory', parent_id: null, size: 0, mime_type: '' }),
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items, total_bytes: 4096, file_count: items.filter(item => item.kind === 'file').length })
    if (path.endsWith('/thumbnail')) return route.fulfill({ contentType: 'image/svg+xml', body: cover })
    return json({ items: [] })
  })
}

async function openBrowser(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await expect(page.locator('.file-card')).toHaveCount(items.length)
  await page.waitForTimeout(150)
}

async function cardMetrics(page: Page) {
  return page.locator('.file-card').evaluateAll(cards => cards.map(card => {
    const preview = card.querySelector('.card-preview')
    const icon = preview?.querySelector('svg')
    const image = preview?.querySelector('img')
    return {
      name: card.querySelector('.card-info strong')?.textContent?.trim(),
      classes: Array.from(card.classList).sort(),
      previewTitle: preview?.getAttribute('title'),
      previewTag: preview?.firstElementChild?.tagName.toLowerCase(),
      previewClass: preview?.firstElementChild?.getAttribute('class'),
      imageSrc: image?.getAttribute('src'),
      iconClass: icon?.getAttribute('class'),
      iconPaths: Array.from(preview?.querySelectorAll('path, ellipse, circle') ?? []).map(element => ({
        tag: element.tagName.toLowerCase(),
        className: element.getAttribute('class'),
        d: element.getAttribute('d'),
      })),
      meta: card.querySelector('.card-info small')?.textContent?.replace(/\s+/g, ' ').trim(),
    }
  }))
}

async function rowMetrics(page: Page) {
  return page.locator('.file-row').evaluateAll(rows => rows.map(row => {
    const preview = row.querySelector('.row-preview')
    const icon = preview?.querySelector('svg')
    const image = preview?.querySelector('img')
    return {
      name: row.querySelector('.row-info strong')?.textContent?.trim(),
      classes: Array.from(row.classList).sort(),
      cannotOpen: preview?.classList.contains('cannot-open'),
      previewTag: preview?.firstElementChild?.tagName.toLowerCase(),
      previewClass: preview?.firstElementChild?.getAttribute('class'),
      imageSrc: image?.getAttribute('src'),
      iconClass: icon?.getAttribute('class'),
      iconPaths: Array.from(preview?.querySelectorAll('path, ellipse, circle') ?? []).map(element => ({
        tag: element.tagName.toLowerCase(),
        className: element.getAttribute('class'),
        d: element.getAttribute('d'),
      })),
      meta: row.querySelector('.row-info small')?.textContent?.replace(/\s+/g, ' ').trim(),
      select: row.querySelector('.row-select') !== null,
    }
  }))
}

test('文件卡与列表行全类型、状态和预览节点保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 1000 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 1000 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockFileBrowser(oldPage), mockFileBrowser(newPage)])
    await Promise.all([openBrowser(oldPage, oldUrl), openBrowser(newPage, newUrl)])
    expect(await cardMetrics(newPage), 'Rust 方块文件项与 reference 不一致').toEqual(await cardMetrics(oldPage))

    await oldPage.getByTitle('列表视图').click()
    await newPage.getByTitle('列表视图').click()
    await expect(oldPage.locator('.file-row')).toHaveCount(items.length)
    await expect(newPage.locator('.file-row')).toHaveCount(items.length)
    await oldPage.waitForTimeout(150)
    await newPage.waitForTimeout(150)
    expect(await rowMetrics(newPage), 'Rust 列表文件项与 reference 不一致').toEqual(await rowMetrics(oldPage))
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})
