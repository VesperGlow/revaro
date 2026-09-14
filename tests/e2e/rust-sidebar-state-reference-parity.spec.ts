import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

const image = {
  id: 'sidebar-state-image',
  parent_id: ROOT,
  name: '侧栏状态图片.png',
  kind: 'file',
  size: 100,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'image/png',
  etag: 'sidebar-state-image-etag',
  folder_path: [{ id: 'sidebar-state-folder', name: '参考路径' }],
}

async function mockSidebar(page: Page) {
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
        items: { book: [], image: [image], video: [], audio: [] },
        counts: { book: 0, image: 1, video: 0, audio: 0, file: 1 },
      })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 1, video: 0, audio: 0, file: 1 })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    return json({ items: [] })
  })
}

async function openShell(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
}

async function sidebarMetrics(page: Page) {
  return page.locator('.app-sidebar').evaluate(element => {
    const styleOf = (target: Element | null) => {
      if (!target) return null
      const style = getComputedStyle(target)
      const rect = target.getBoundingClientRect()
      return {
        background: style.backgroundColor,
        color: style.color,
        padding: style.padding,
        gap: style.gap,
        minHeight: style.minHeight,
        width: Math.round(rect.width * 100) / 100,
        height: Math.round(rect.height * 100) / 100,
      }
    }
    return {
      className: element.className,
      rect: (() => {
        const rect = element.getBoundingClientRect()
        return { x: rect.x, y: rect.y, width: rect.width, height: rect.height }
      })(),
      categories: Array.from(element.querySelectorAll('.sidebar-category')).map(category => {
        const main = category.querySelector('.category-main')
        const expand = category.querySelector('.category-expand')
        const icon = category.querySelector('.category-icon')
        return {
          categoryClass: category.className,
          mainClass: main?.className,
          mainTitle: main?.getAttribute('title'),
          mainCurrent: main?.getAttribute('aria-current'),
          main: styleOf(main),
          icon: styleOf(icon),
          count: styleOf(category.querySelector('.category-count')),
          expand: styleOf(expand),
          expandLabel: expand?.getAttribute('aria-label'),
          expandState: expand?.getAttribute('aria-expanded'),
          expandTransform: expand ? getComputedStyle(expand.querySelector('svg') as Element).transform : null,
        }
      }),
      trash: (() => {
        const trash = element.querySelector('.trash-entry')
        return { className: trash?.className, style: styleOf(trash), title: trash?.getAttribute('title') }
      })(),
    }
  })
}

async function hoverMetrics(page: Page, selector: string) {
  await page.locator(selector).hover()
  await page.waitForTimeout(220)
  return page.locator(selector).evaluate(element => {
    const style = getComputedStyle(element)
    const icon = element.querySelector('svg') || element.querySelector('.category-icon')
    const iconStyle = icon ? getComputedStyle(icon) : null
    return {
      background: style.backgroundColor,
      color: style.color,
      iconColor: iconStyle?.color,
      outline: style.outline,
    }
  })
}

test('侧栏 active/hover/折叠与移动抽屉状态保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  await oldContext.addInitScript(() => {
    localStorage.removeItem('revaro:sidebar:collapsed')
    localStorage.removeItem('revaro:sidebar:expanded')
  })
  await newContext.addInitScript(() => {
    localStorage.removeItem('revaro:sidebar:collapsed')
    localStorage.removeItem('revaro:sidebar:expanded')
  })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockSidebar(oldPage), mockSidebar(newPage)])
    await Promise.all([openShell(oldPage, oldUrl), openShell(newPage, newUrl)])

    expect(await sidebarMetrics(newPage), 'Rust 侧栏初始 active/计数/布局与 reference 不一致')
      .toEqual(await sidebarMetrics(oldPage))
    expect(await hoverMetrics(newPage, '.category-main[data-category="book"]'))
      .toEqual(await hoverMetrics(oldPage, '.category-main[data-category="book"]'))
    expect(await hoverMetrics(newPage, '.sidebar-collapse'))
      .toEqual(await hoverMetrics(oldPage, '.sidebar-collapse'))

    await Promise.all([
      oldPage.locator('.category-main[data-category="image"]').click(),
      newPage.locator('.category-main[data-category="image"]').click(),
    ])
    await expect(oldPage.locator('.sidebar-category:has([data-category="image"]) .category-paths')).toBeVisible()
    await expect(newPage.locator('.sidebar-category:has([data-category="image"]) .category-paths')).toBeVisible()
    await Promise.all([oldPage.waitForTimeout(250), newPage.waitForTimeout(250)])
    expect(await sidebarMetrics(newPage), 'Rust 侧栏 active 分类与路径展开状态与 reference 不一致')
      .toEqual(await sidebarMetrics(oldPage))
    expect(await hoverMetrics(newPage, '.category-main[data-category="image"]'))
      .toEqual(await hoverMetrics(oldPage, '.category-main[data-category="image"]'))

    await Promise.all([
      oldPage.locator('.sidebar-collapse').click(),
      newPage.locator('.sidebar-collapse').click(),
    ])
    expect(await sidebarMetrics(newPage), 'Rust 侧栏折叠 rail 的 class/尺寸/标题与 reference 不一致')
      .toEqual(await sidebarMetrics(oldPage))
    expect(await hoverMetrics(newPage, '.category-main[data-category="image"]'))
      .toEqual(await hoverMetrics(oldPage, '.category-main[data-category="image"]'))

    await Promise.all([oldPage.setViewportSize({ width: 390, height: 844 }), newPage.setViewportSize({ width: 390, height: 844 })])
    await Promise.all([
      oldPage.waitForTimeout(300),
      newPage.waitForTimeout(300),
      oldPage.mouse.move(380, 800),
      newPage.mouse.move(380, 800),
    ])
    await expect(oldPage.locator('.sidebar-handle')).toBeVisible()
    await expect(newPage.locator('.sidebar-handle')).toBeVisible()
    expect(await sidebarMetrics(newPage), 'Rust 移动端侧栏初始状态与 reference 不一致')
      .toEqual(await sidebarMetrics(oldPage))
    await Promise.all([oldPage.locator('.sidebar-handle').click(), newPage.locator('.sidebar-handle').click()])
    await expect(oldPage.locator('.app-sidebar')).toHaveClass(/mobile-open/)
    await expect(newPage.locator('.app-sidebar')).toHaveClass(/mobile-open/)
    await Promise.all([oldPage.waitForTimeout(300), newPage.waitForTimeout(300)])
    expect(await sidebarMetrics(newPage), 'Rust 移动端侧栏抽屉状态与 reference 不一致')
      .toEqual(await sidebarMetrics(oldPage))
    expect(await hoverMetrics(newPage, '.category-main[data-category="image"]'))
      .toEqual(await hoverMetrics(oldPage, '.category-main[data-category="image"]'))
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})
