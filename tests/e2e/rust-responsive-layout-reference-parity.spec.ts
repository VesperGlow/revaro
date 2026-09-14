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

const items = [
  {
    id: 'responsive-folder',
    parent_id: ROOT,
    name: '响应式目录',
    kind: 'directory',
    size: 0,
    status: 'ready',
    created_at: STAMP,
    updated_at: STAMP,
    mime_type: '',
  },
  {
    id: 'responsive-note',
    parent_id: ROOT,
    name: '响应式说明.txt',
    kind: 'file',
    size: 2048,
    status: 'ready',
    created_at: STAMP,
    updated_at: STAMP,
    mime_type: 'text/plain',
    etag: 'responsive-etag',
  },
]

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
      return json({
        items: { book: [], image: [], video: [], audio: [] },
        counts: { book: 0, image: 0, video: 0, audio: 0, file: items.length },
      })
    }
    if (path === '/api/library/counts') {
      return json({ book: 0, image: 0, video: 0, audio: 0, file: items.length })
    }
    if (path === '/api/trash') return json({ items: [], total_bytes: 0, file_count: 0 })
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) {
      return json({ items, total_bytes: 2048, file_count: 1 })
    }
    return json({ items: [] })
  })
}

async function openShell(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?responsive-reference=${crypto.randomUUID()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await expect(page.locator('.file-card')).toHaveCount(items.length)
}

async function layoutSnapshot(page: Page) {
  return page.evaluate(() => {
    const rect = (selector: string) => {
      const element = document.querySelector<HTMLElement>(selector)
      if (!element) return null
      const box = element.getBoundingClientRect()
      return { x: box.x, y: box.y, width: box.width, height: box.height }
    }
    const style = (selector: string, property: string) => {
      const element = document.querySelector<HTMLElement>(selector)
      return element ? getComputedStyle(element).getPropertyValue(property) : null
    }
    const visible = (selector: string) => {
      const element = document.querySelector<HTMLElement>(selector)
      if (!element) return false
      const computed = getComputedStyle(element)
      return computed.display !== 'none' && computed.visibility !== 'hidden'
    }
    return {
      viewport: { width: window.innerWidth, height: window.innerHeight },
      body: { clientWidth: document.body.clientWidth, scrollWidth: document.body.scrollWidth },
      shell: rect('.app-shell'),
      shellColumns: style('.app-shell', 'grid-template-columns'),
      topbar: rect('.topbar'),
      sidebar: rect('.app-sidebar'),
      content: rect('.content'),
      contentHead: rect('.content-head'),
      fileGrid: rect('.file-grid'),
      gridColumns: style('.file-grid', 'grid-template-columns'),
      desktopCreateActions: visible('.desktop-create-actions'),
      createMenu: visible('.create-menu'),
      uploadMenu: visible('.upload-menu'),
      mobileSidebarToggle: visible('button[aria-label="打开文件分类菜单"]'),
      mobileToolsToggle: visible('summary[aria-label="打开账户与工具菜单"]'),
      cardCount: document.querySelectorAll('.file-card').length,
      rowCount: document.querySelectorAll('.file-row').length,
    }
  })
}

test('六个 viewport 断点的页面几何和可见入口保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const viewports = [
    { width: 1440, height: 900 },
    { width: 1024, height: 900 },
    { width: 851, height: 900 },
    { width: 850, height: 900 },
    { width: 390, height: 844 },
    { width: 320, height: 700 },
  ]

  for (const viewport of viewports) {
    const oldContext = await browser.newContext({ viewport })
    const newContext = await browser.newContext({ viewport })
    const oldPage = await oldContext.newPage()
    const newPage = await newContext.newPage()
    try {
      await Promise.all([mockShell(oldPage), mockShell(newPage)])
      await Promise.all([openShell(oldPage, oldUrl), openShell(newPage, newUrl)])
      const oldLayout = await layoutSnapshot(oldPage)
      const newLayout = await layoutSnapshot(newPage)
      expect(newLayout, `Rust ${viewport.width}px 页面布局与 reference 不一致`).toEqual(oldLayout)
      expect(newLayout.body.scrollWidth, `Rust ${viewport.width}px 出现横向页面溢出`).toBeLessThanOrEqual(viewport.width)
      expect(newLayout.cardCount).toBe(items.length)
      expect(newLayout.rowCount).toBe(0)
    } finally {
      await Promise.all([oldContext.close(), newContext.close()])
    }
  }
})

test('移动端列表/方块切换及刷新偏好保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 390, height: 844 } })
  const newContext = await browser.newContext({ viewport: { width: 390, height: 844 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockShell(oldPage), mockShell(newPage)])
    await Promise.all([openShell(oldPage, oldUrl), openShell(newPage, newUrl)])
    await Promise.all([
      oldPage.getByTitle('列表视图').click(),
      newPage.getByTitle('列表视图').click(),
    ])
    await Promise.all([
      expect(oldPage.locator('.file-row')).toHaveCount(items.length),
      expect(newPage.locator('.file-row')).toHaveCount(items.length),
    ])
    expect(await layoutSnapshot(newPage), 'Rust 移动端列表布局与 reference 不一致')
      .toEqual(await layoutSnapshot(oldPage))

    await Promise.all([oldPage.reload(), newPage.reload()])
    await Promise.all([
      expect(oldPage.locator('.file-row')).toHaveCount(items.length),
      expect(newPage.locator('.file-row')).toHaveCount(items.length),
    ])
    expect(await layoutSnapshot(newPage), 'Rust 移动端刷新后的列表偏好与 reference 不一致')
      .toEqual(await layoutSnapshot(oldPage))

    await Promise.all([
      oldPage.getByTitle('方块视图').click(),
      newPage.getByTitle('方块视图').click(),
    ])
    expect(await layoutSnapshot(newPage), 'Rust 移动端切回方块布局与 reference 不一致')
      .toEqual(await layoutSnapshot(oldPage))
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})
