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
  base({ id: 'image', name: '图片.png', mime_type: 'image/png' }),
  base({ id: 'unknown', name: '未知.pdf', mime_type: 'application/pdf' }),
  base({ id: 'pending', name: '处理中.txt', status: 'pending', mime_type: 'text/plain' }),
  base({ id: 'failed', name: '失败.png', status: 'failed', mime_type: 'image/png' }),
  base({ id: 'future-status', name: '未来状态.txt', status: 'future_status', mime_type: 'text/plain' }),
  base({ id: 'future-kind', name: '未来类型.bin', kind: 'future_kind', mime_type: 'application/octet-stream' }),
  base({ id: 'nullable-fields', name: '可空字段.txt', mime_type: null, etag: null, has_cover: null }),
]

const cover = '<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40"><rect width="40" height="40" fill="#789"/></svg>'

async function mockFileBrowser(page: Page, options: { sparseTimestamps?: boolean } = {}) {
  const sparseItems = options.sparseTimestamps
    ? items.map((item, index) => {
        if (index === 0) {
          const { created_at: _createdAt, updated_at: _updatedAt, ...withoutTimestamps } = item
          return withoutTimestamps
        }
        if (index === 1) return { ...item, created_at: null, updated_at: null }
        if (index === 2) {
          const { created_at: _createdAt, updated_at: _updatedAt, ...withoutTimestamps } = item
          return withoutTimestamps
        }
        return item
      })
    : items
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
    if (path === `/api/files/${ROOT}/children`) return json({ items: sparseItems, total_bytes: 4096, file_count: items.filter(item => item.kind === 'file').length })
    if (path.endsWith('/thumbnail')) return route.fulfill({ contentType: 'image/svg+xml', body: cover })
    return json({ items: [] })
  })
}

test('old/new 文件列表在未使用的时间字段缺失或为 null 时仍保留条目', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([
      mockFileBrowser(oldPage, { sparseTimestamps: true }),
      mockFileBrowser(newPage, { sparseTimestamps: true }),
    ])
    await Promise.all([openBrowser(oldPage, oldUrl), openBrowser(newPage, newUrl)])
    expect(await oldPage.locator('.file-card').filter({ hasText: '普通目录' }).count()).toBe(1)
    expect(await newPage.locator('.file-card').filter({ hasText: '普通目录' }).count(), 'Rust 不应因未消费的文件时间字段丢失整个列表').toBe(1)
    await expect(oldPage.locator('.file-card')).toHaveCount(items.length)
    await expect(newPage.locator('.file-card')).toHaveCount(items.length)
    const oldSubtitle = await oldPage.locator('.file-card').filter({ hasText: '图片.png' }).locator('.card-info small').textContent()
    const newSubtitle = await newPage.locator('.file-card').filter({ hasText: '图片.png' }).locator('.card-info small').textContent()
    expect(newSubtitle, 'Rust 文件时间缺失/null 时的方块元信息回退与 reference 不一致').toBe(oldSubtitle)
    expect(
      await newPage.locator('.file-card').filter({ hasText: '未知.pdf' }).innerText(),
      'Rust 稀疏文件方块卡片与 reference 的元信息回退不一致',
    ).toBe(await oldPage.locator('.file-card').filter({ hasText: '未知.pdf' }).innerText())
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

async function openBrowser(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await expect(page.locator('.file-card')).toHaveCount(items.length)
  await page.waitForTimeout(180)
}

async function cardState(page: Page, name: string) {
  return page.locator('.file-card').filter({ hasText: name }).evaluate(card => {
    const read = (element: Element | null) => {
      if (!element) return null
      const style = getComputedStyle(element)
      const after = getComputedStyle(element, '::after')
      return {
        className: Array.from(element.classList).sort(),
        focusVisible: element.matches(':focus-visible'),
        backgroundColor: style.backgroundColor,
        borderColor: style.borderColor,
        borderWidth: style.borderWidth,
        borderRadius: style.borderRadius,
        boxShadow: style.boxShadow,
        transform: style.transform,
        opacity: style.opacity,
        color: style.color,
        cursor: style.cursor,
        outlineColor: style.outlineColor,
        outlineWidth: style.outlineWidth,
        afterBackground: after.backgroundColor,
        afterContent: after.content,
      }
    }
    return {
      card: read(card),
      preview: read(card.querySelector('.card-preview')),
      icon: read(card.querySelector('.card-preview svg')),
      info: read(card.querySelector('.card-info')),
    }
  })
}

test('文件卡的 hover、focus、selected、muted 状态保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 1000 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 1000 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockFileBrowser(oldPage), mockFileBrowser(newPage)])
    await Promise.all([openBrowser(oldPage, oldUrl), openBrowser(newPage, newUrl)])

    const oldImage = oldPage.locator('.file-card').filter({ hasText: '图片.png' })
    const newImage = newPage.locator('.file-card').filter({ hasText: '图片.png' })
    expect(await cardState(newPage, '图片.png'), 'Rust 方块正常状态与 reference 不一致').toEqual(await cardState(oldPage, '图片.png'))

    await Promise.all([oldImage.hover(), newImage.hover()])
    await Promise.all([oldPage.waitForTimeout(240), newPage.waitForTimeout(240)])
    expect(await cardState(newPage, '图片.png'), 'Rust 方块 hover 状态与 reference 不一致').toEqual(await cardState(oldPage, '图片.png'))

    await Promise.all([oldPage.mouse.move(2, 2), newPage.mouse.move(2, 2)])
    await Promise.all([oldPage.keyboard.press('Tab'), newPage.keyboard.press('Tab')])
    await Promise.all([oldImage.focus(), newImage.focus()])
    await Promise.all([oldPage.waitForTimeout(240), newPage.waitForTimeout(240)])
    expect(await oldImage.evaluate(element => element.matches(':focus-visible'))).toBe(true)
    expect(await cardState(newPage, '图片.png'), 'Rust 方块 focus 状态与 reference 不一致').toEqual(await cardState(oldPage, '图片.png'))

    const oldUnknown = oldPage.locator('.file-card').filter({ hasText: '未知.pdf' })
    const newUnknown = newPage.locator('.file-card').filter({ hasText: '未知.pdf' })
    await Promise.all([oldPage.mouse.move(2, 2), newPage.mouse.move(2, 2)])
    expect(await cardState(newPage, '未知.pdf'), 'Rust 方块 fallback 状态与 reference 不一致').toEqual(await cardState(oldPage, '未知.pdf'))
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})
