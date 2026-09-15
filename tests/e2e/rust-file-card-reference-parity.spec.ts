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
  base({ id: 'video', name: '视频.webm', mime_type: 'video/webm', etag: 'etag video&/?' }),
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

async function openBrowser(page: Page, baseUrl: string, expectedCount = items.length) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await expect(page.locator('.file-card')).toHaveCount(expectedCount)
  await page.waitForTimeout(150)
}

async function cardMetrics(page: Page) {
  return page.locator('.file-card').evaluateAll(cards => cards.map(card => {
    const preview = card.querySelector('.card-preview')
    const icon = preview?.querySelector('svg')
    const image = preview?.querySelector('img')
    const iconStyle = icon ? getComputedStyle(icon) : null
    const iconChildStyles = icon
      ? Array.from(icon.querySelectorAll('path, ellipse, circle')).map(element => {
        const style = getComputedStyle(element)
        return {
          className: element.getAttribute('class'),
          fill: style.fill,
          stroke: style.stroke,
          strokeWidth: style.strokeWidth,
          lineCap: style.strokeLinecap,
          lineJoin: style.strokeLinejoin,
          opacity: style.opacity,
        }
      })
      : []
    return {
      name: card.querySelector('.card-info strong')?.textContent?.trim(),
      classes: Array.from(card.classList).sort(),
      previewTitle: preview?.getAttribute('title'),
      previewTag: preview?.firstElementChild?.tagName.toLowerCase(),
      previewClass: preview?.firstElementChild?.getAttribute('class'),
      imageSrc: image?.getAttribute('src'),
      iconClass: icon?.getAttribute('class'),
      iconStyle: iconStyle && {
        color: iconStyle.color,
        fill: iconStyle.fill,
        stroke: iconStyle.stroke,
        strokeWidth: iconStyle.strokeWidth,
        width: iconStyle.width,
        height: iconStyle.height,
        opacity: iconStyle.opacity,
      },
      iconChildStyles,
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
    const iconStyle = icon ? getComputedStyle(icon) : null
    const iconChildStyles = icon
      ? Array.from(icon.querySelectorAll('path, ellipse, circle')).map(element => {
        const style = getComputedStyle(element)
        return {
          className: element.getAttribute('class'),
          fill: style.fill,
          stroke: style.stroke,
          strokeWidth: style.strokeWidth,
          lineCap: style.strokeLinecap,
          lineJoin: style.strokeLinejoin,
          opacity: style.opacity,
        }
      })
      : []
    return {
      name: row.querySelector('.row-info strong')?.textContent?.trim(),
      classes: Array.from(row.classList).sort(),
      cannotOpen: preview?.classList.contains('cannot-open'),
      previewTag: preview?.firstElementChild?.tagName.toLowerCase(),
      previewClass: preview?.firstElementChild?.getAttribute('class'),
      imageSrc: image?.getAttribute('src'),
      iconClass: icon?.getAttribute('class'),
      iconStyle: iconStyle && {
        color: iconStyle.color,
        fill: iconStyle.fill,
        stroke: iconStyle.stroke,
        strokeWidth: iconStyle.strokeWidth,
        width: iconStyle.width,
        height: iconStyle.height,
        opacity: iconStyle.opacity,
      },
      iconChildStyles,
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
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
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

const fallbackItems = [
  base({ id: 'delayed-image', name: '等待图片.png', mime_type: 'image/png' }),
  base({ id: 'fallback-image', name: '回退图片.png', mime_type: 'image/png' }),
  base({ id: 'broken-audio', name: '损坏封面.mp3', mime_type: 'audio/mpeg', has_cover: true }),
  base({ id: 'broken-epub', name: '损坏封面.epub', mime_type: 'application/epub+zip' }),
]

async function mockFallbackBrowser(page: Page) {
  let delayedThumbRequests = 0
  let fallbackThumbRequests = 0
  let fallbackPreviewRequests = 0
  let brokenAudioThumbRequests = 0
  let brokenEpubThumbRequests = 0
  let releaseDelayed!: () => void
  const delayed = new Promise<void>(resolve => {
    releaseDelayed = resolve
  })

  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: fallbackItems.length } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: fallbackItems.length })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: base({ id: ROOT, name: '我的文件', kind: 'directory', parent_id: null, size: 0, mime_type: '' }),
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: fallbackItems, total_bytes: 4096, file_count: fallbackItems.length })
    if (path === '/api/files/delayed-image/thumbnail') {
      delayedThumbRequests += 1
      await delayed
      return route.fulfill({ contentType: 'image/svg+xml', body: cover })
    }
    if (path === '/api/files/fallback-image/thumbnail') {
      fallbackThumbRequests += 1
      return route.fulfill({ status: 500, body: 'thumbnail unavailable' })
    }
    if (path === '/api/files/fallback-image/preview') {
      fallbackPreviewRequests += 1
      return route.fulfill({ contentType: 'image/svg+xml', body: cover })
    }
    if (path === '/api/files/broken-audio/thumbnail') {
      brokenAudioThumbRequests += 1
      return route.fulfill({ status: 500, body: 'thumbnail unavailable' })
    }
    if (path === '/api/files/broken-epub/thumbnail') {
      brokenEpubThumbRequests += 1
      return route.fulfill({ status: 500, body: 'thumbnail unavailable' })
    }
    if (path.endsWith('/thumbnail')) return route.fulfill({ contentType: 'image/svg+xml', body: cover })
    return json({ items: [] })
  })

  return {
    delayedThumbRequests: () => delayedThumbRequests,
    fallbackThumbRequests: () => fallbackThumbRequests,
    fallbackPreviewRequests: () => fallbackPreviewRequests,
    brokenAudioThumbRequests: () => brokenAudioThumbRequests,
    brokenEpubThumbRequests: () => brokenEpubThumbRequests,
    releaseDelayed,
  }
}

async function fallbackState(page: Page) {
  return page.locator('.file-card').evaluateAll(cards => cards.map(card => {
    const preview = card.querySelector('.card-preview')
    const image = preview?.querySelector('img')
    return {
      name: card.querySelector('.card-info strong')?.textContent?.trim(),
      classes: Array.from(card.classList).sort(),
      previewClasses: Array.from(preview?.classList ?? []).sort(),
      imageSrc: image?.getAttribute('src') ?? null,
      imageLoading: image?.getAttribute('loading') ?? null,
      iconClass: preview?.querySelector('svg')?.getAttribute('class') ?? null,
    }
  }))
}

test('文件卡缩略图 loading 与各类失败回退保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 1000 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 1000 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    const [oldMock, newMock] = await Promise.all([mockFallbackBrowser(oldPage), mockFallbackBrowser(newPage)])
    await Promise.all([
      openBrowser(oldPage, oldUrl, fallbackItems.length),
      openBrowser(newPage, newUrl, fallbackItems.length),
    ])

    await expect.poll(oldMock.delayedThumbRequests).toBe(1)
    await expect.poll(newMock.delayedThumbRequests).toBe(1)
    expect(await fallbackState(newPage), 'Rust 缩略图 loading 初始节点与 reference 不一致').toEqual(await fallbackState(oldPage))

    oldMock.releaseDelayed()
    newMock.releaseDelayed()
    await expect.poll(oldMock.fallbackPreviewRequests).toBe(1)
    await expect.poll(newMock.fallbackPreviewRequests).toBe(1)
    await expect.poll(() => oldPage.locator('.file-card').filter({ hasText: '损坏封面.mp3' }).locator('.audio-type-icon').count()).toBe(1)
    await expect.poll(() => newPage.locator('.file-card').filter({ hasText: '损坏封面.mp3' }).locator('.audio-type-icon').count()).toBe(1)
    await expect.poll(() => oldPage.locator('.file-card').filter({ hasText: '损坏封面.epub' }).locator('.book-type-icon').count()).toBe(1)
    await expect.poll(() => newPage.locator('.file-card').filter({ hasText: '损坏封面.epub' }).locator('.book-type-icon').count()).toBe(1)
    expect(await fallbackState(newPage), 'Rust 缩略图错误回退与 reference 不一致').toEqual(await fallbackState(oldPage))
    expect(oldMock.fallbackThumbRequests()).toBe(1)
    expect(newMock.fallbackThumbRequests()).toBe(1)
    expect(oldMock.brokenAudioThumbRequests()).toBe(1)
    expect(newMock.brokenAudioThumbRequests()).toBe(1)
    expect(oldMock.brokenEpubThumbRequests()).toBe(1)
    expect(newMock.brokenEpubThumbRequests()).toBe(1)

    await Promise.all([oldPage.getByTitle('列表视图').click(), newPage.getByTitle('列表视图').click()])
    await expect(oldPage.locator('.file-row')).toHaveCount(fallbackItems.length)
    await expect(newPage.locator('.file-row')).toHaveCount(fallbackItems.length)
    await expect.poll(() => oldPage.locator('.file-row').filter({ hasText: '回退图片.png' }).locator('img').getAttribute('src')).toContain('/preview')
    await expect.poll(() => newPage.locator('.file-row').filter({ hasText: '回退图片.png' }).locator('img').getAttribute('src')).toContain('/preview')
    expect(
      await newPage.locator('.file-row').filter({ hasText: '损坏封面.mp3' }).locator('.audio-type-icon').count(),
    ).toBe(await oldPage.locator('.file-row').filter({ hasText: '损坏封面.mp3' }).locator('.audio-type-icon').count())
    expect(
      await newPage.locator('.file-row').filter({ hasText: '损坏封面.epub' }).locator('.book-type-icon').count(),
    ).toBe(await oldPage.locator('.file-row').filter({ hasText: '损坏封面.epub' }).locator('.book-type-icon').count())
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

const longNameItem = base({
  id: 'long-name',
  name: '这是一个用于验证文件名截断行为的超长文本文档-abcdefghijklmnopqrstuvwxyz-0123456789-最终版本.txt',
  mime_type: 'text/plain',
})

async function mockLongNameBrowser(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 1 } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 1 })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: base({ id: ROOT, name: '我的文件', kind: 'directory', parent_id: null, size: 0, mime_type: '' }),
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) {
      return json({ items: [longNameItem], total_bytes: longNameItem.size, file_count: 1 })
    }
    return json({ items: [] })
  })
}

async function longNameMetrics(page: Page) {
  return page.evaluate(() => {
    const measure = (selector: string) => {
      const element = document.querySelector<HTMLElement>(selector)
      if (!element) return null
      const style = getComputedStyle(element)
      const rect = element.getBoundingClientRect()
      return {
        text: element.textContent?.trim() ?? '',
        title: element.getAttribute('title'),
        display: style.display,
        minWidth: style.minWidth,
        maxWidth: style.maxWidth,
        width: Math.round(rect.width * 100) / 100,
        clientWidth: element.clientWidth,
        scrollWidth: element.scrollWidth,
        overflow: style.overflow,
        textOverflow: style.textOverflow,
        whiteSpace: style.whiteSpace,
      }
    }
    return {
      bodyOverflowX: getComputedStyle(document.body).overflowX,
      card: {
        info: measure('.file-card .card-info'),
        name: measure('.file-card .card-info strong'),
        meta: measure('.file-card .card-info small'),
      },
      row: {
        info: measure('.file-row .row-info'),
        name: measure('.file-row .row-info strong'),
        meta: measure('.file-row .row-info small'),
      },
    }
  })
}

test('长文件名在方块与列表布局中保持 reference 截断和溢出行为', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'

  for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }]) {
    const oldContext = await browser.newContext({ viewport })
    const newContext = await browser.newContext({ viewport })
    const oldPage = await oldContext.newPage()
    const newPage = await newContext.newPage()

    try {
      await Promise.all([mockLongNameBrowser(oldPage), mockLongNameBrowser(newPage)])
      await Promise.all([
        openBrowser(oldPage, oldUrl, 1),
        openBrowser(newPage, newUrl, 1),
      ])
      expect(await longNameMetrics(newPage), 'Rust 方块长文件名截断与 reference 不一致')
        .toEqual(await longNameMetrics(oldPage))

      await Promise.all([
        oldPage.getByTitle('列表视图').click(),
        newPage.getByTitle('列表视图').click(),
      ])
      await Promise.all([
        expect(oldPage.locator('.file-row')).toHaveCount(1),
        expect(newPage.locator('.file-row')).toHaveCount(1),
      ])
      expect(await longNameMetrics(newPage), 'Rust 列表长文件名截断与 reference 不一致')
        .toEqual(await longNameMetrics(oldPage))
    } finally {
      await Promise.all([oldContext.close(), newContext.close()])
    }
  }
})
