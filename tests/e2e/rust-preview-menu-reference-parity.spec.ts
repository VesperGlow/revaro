import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'
const image = {
  id: 'preview-menu-image',
  parent_id: ROOT,
  name: '菜单测试.png',
  kind: 'file',
  size: 4096,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'image/png',
  etag: 'preview-menu-etag',
}

const picture = '<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="800"><rect width="1200" height="800" fill="#789"/><circle cx="840" cy="240" r="120" fill="#eddfb8"/></svg>'

async function mockPreview(page: Page) {
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
        file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: [image], total_bytes: image.size, file_count: 1 })
    if (path.endsWith('/thumbnail') || path.endsWith('/preview')) {
      return route.fulfill({ contentType: 'image/svg+xml', body: picture })
    }
    return json({ items: [] })
  })
}

async function openImage(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?preview-menu-parity=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.locator('.file-card').filter({ hasText: image.name }).click()
  await expect(page.locator('.preview-modal')).toBeVisible()
  await expect(page.locator('.preview-image')).toBeVisible()
}

async function menuMetrics(page: Page) {
  return page.locator('.preview-commandbar .preview-menu').evaluate(menu => {
    const summary = menu.querySelector('summary')!
    const panel = menu.querySelector('.preview-menu-panel')!
    const items = Array.from(panel.querySelectorAll(':scope > button'))
    const styleOf = (element: Element) => {
      const style = getComputedStyle(element)
      const rect = element.getBoundingClientRect()
      return {
        display: style.display,
        position: style.position,
        background: style.backgroundColor,
        color: style.color,
        border: style.border,
        borderRadius: style.borderRadius,
        boxShadow: style.boxShadow,
        padding: style.padding,
        gap: style.gap,
        width: Math.round(rect.width * 100) / 100,
        height: Math.round(rect.height * 100) / 100,
      }
    }
    return {
      open: menu.hasAttribute('open'),
      label: summary.getAttribute('aria-label'),
      title: summary.getAttribute('title'),
      summary: styleOf(summary),
      panel: styleOf(panel),
      items: items.map(item => ({
        text: item.textContent?.replace(/\s+/g, ' ').trim(),
        style: styleOf(item),
        icon: styleOf(item.querySelector('svg')!),
      })),
    }
  })
}

async function hoverMetrics(page: Page) {
  const item = page.locator('.preview-commandbar .preview-menu-panel > button').first()
  await item.hover()
  await page.waitForTimeout(220)
  return item.evaluate(element => {
    const style = getComputedStyle(element)
    const icon = getComputedStyle(element.querySelector('svg')!)
    return { background: style.backgroundColor, color: style.color, iconColor: icon.color }
  })
}

async function contextMenuDefault(page: Page) {
  return page.evaluate(() => new Promise<boolean>(resolve => {
    const listener = (event: MouseEvent) => {
      window.removeEventListener('contextmenu', listener)
      resolve(event.defaultPrevented)
    }
    window.addEventListener('contextmenu', listener)
  }))
}

test('媒体更多菜单和文件右键默认行为保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockPreview(oldPage), mockPreview(newPage)])
    await Promise.all([
      oldPage.goto(`${oldUrl}/?preview-menu-context=${Date.now()}`),
      newPage.goto(`${newUrl}/?preview-menu-context=${Date.now()}`),
    ])
    await Promise.all([
      expect(oldPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible(),
      expect(newPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible(),
    ])
    const oldContextEvent = contextMenuDefault(oldPage)
    const newContextEvent = contextMenuDefault(newPage)
    await Promise.all([
      oldPage.locator('.file-card').filter({ hasText: image.name }).click({ button: 'right' }),
      newPage.locator('.file-card').filter({ hasText: image.name }).click({ button: 'right' }),
    ])
    expect(await newContextEvent, 'Rust 文件卡右键默认行为与 reference 不一致').toBe(await oldContextEvent)

    await Promise.all([openImage(oldPage, oldUrl), openImage(newPage, newUrl)])
    const selector = '.preview-commandbar .preview-menu'
    expect(await menuMetrics(newPage), 'Rust 媒体更多菜单初始状态与 reference 不一致')
      .toEqual(await menuMetrics(oldPage))

    await Promise.all([
      oldPage.locator(selector).locator('summary').click(),
      newPage.locator(selector).locator('summary').click(),
    ])
    await expect(oldPage.locator(selector)).toHaveAttribute('open', '')
    await expect(newPage.locator(selector)).toHaveAttribute('open', '')
    await Promise.all([oldPage.waitForTimeout(220), newPage.waitForTimeout(220)])
    expect(await menuMetrics(newPage), 'Rust 媒体更多菜单展开状态与 reference 不一致')
      .toEqual(await menuMetrics(oldPage))
    expect(await hoverMetrics(newPage), 'Rust 媒体更多菜单 hover 与 reference 不一致')
      .toEqual(await hoverMetrics(oldPage))

    await Promise.all([
      oldPage.locator('.preview-file-meta strong').click(),
      newPage.locator('.preview-file-meta strong').click(),
    ])
    await expect(oldPage.locator(selector)).not.toHaveAttribute('open', '')
    await expect(newPage.locator(selector)).not.toHaveAttribute('open', '')

    const oldDefault = oldPage.evaluate(() => new Promise<boolean>(resolve => {
      const listener = (event: KeyboardEvent) => {
        window.removeEventListener('keydown', listener)
        window.setTimeout(() => resolve(event.defaultPrevented), 0)
      }
      window.addEventListener('keydown', listener, true)
    }))
    const newDefault = newPage.evaluate(() => new Promise<boolean>(resolve => {
      const listener = (event: KeyboardEvent) => {
        window.removeEventListener('keydown', listener)
        window.setTimeout(() => resolve(event.defaultPrevented), 0)
      }
      window.addEventListener('keydown', listener, true)
    }))
    await Promise.all([
      oldPage.locator(selector).locator('summary').click(),
      newPage.locator(selector).locator('summary').click(),
    ])
    await Promise.all([oldPage.keyboard.press('Escape'), newPage.keyboard.press('Escape')])
    expect(await newDefault, 'Rust 媒体更多菜单 Escape 默认事件与 reference 不一致').toBe(await oldDefault)
    await expect(oldPage.locator(selector)).not.toHaveAttribute('open', '')
    await expect(newPage.locator(selector)).not.toHaveAttribute('open', '')
    await expect(oldPage.locator(selector).locator('summary')).toBeFocused()
    await expect(newPage.locator(selector).locator('summary')).toBeFocused()
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})
