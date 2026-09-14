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
  mime_type: 'image/png',
  etag: `etag-${String(value.id ?? 'file')}`,
  ...value,
})

const library = {
  book: [],
  image: [
    file({ id: 'image-root', name: '根目录.png', folder_path: [] }),
    file({
      id: 'image-travel-1',
      name: '旅行一.png',
      folder_path: [folder('archive', '归档'), folder('travel', '旅行')],
    }),
    file({
      id: 'image-travel-2',
      name: '旅行二.png',
      folder_path: [folder('archive', '归档'), folder('travel', '旅行')],
    }),
    file({
      id: 'image-work',
      name: '工作.png',
      folder_path: [folder('archive', '归档'), folder('work', '工作')],
    }),
  ],
  video: [],
  audio: [],
}

const counts = { book: 0, image: 4, video: 0, audio: 0, file: 3 }
const root = file({ id: ROOT, name: '我的文件', kind: 'directory', parent_id: null, size: 0, mime_type: '' })

async function mockSidebarTree(page: Page) {
  const directory = (id: string, name: string, parent_id: string) => file({
    id,
    name,
    parent_id,
    kind: 'directory',
    size: 0,
    mime_type: '',
  })
  const rootChildren = [
    directory('dir-archive', '归档', ROOT),
    directory('dir-empty', '空目录', ROOT),
  ]
  const archiveChildren = [directory('dir-travel', '旅行', 'dir-archive')]

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
    if (path === `/api/files/${ROOT}/children`) {
      return json({ items: rootChildren, total_bytes: 0, file_count: rootChildren.length })
    }
    if (path === '/api/files/dir-archive') {
      return json({ file: rootChildren[0], breadcrumbs: [root] })
    }
    if (path === '/api/files/dir-archive/children') {
      return json({ items: archiveChildren, total_bytes: 0, file_count: archiveChildren.length })
    }
    if (path === '/api/files/dir-empty') {
      return json({ file: rootChildren[1], breadcrumbs: [root] })
    }
    if (path === '/api/files/dir-empty/children') return json({ items: [], total_bytes: 0, file_count: 0 })
    if (path.endsWith('/thumbnail') || path.endsWith('/preview') || path.endsWith('/cover')) {
      return route.fulfill({
        contentType: 'image/svg+xml',
        body: '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 2 2"><rect width="2" height="2" fill="#41628a"/></svg>',
      })
    }
    return json({ items: [] })
  })
}

async function clearPreferences(context: BrowserContext) {
  await context.addInitScript(() => {
    localStorage.removeItem('revaro:sidebar:collapsed')
    localStorage.removeItem('revaro:sidebar:expanded')
  })
}

async function openShell(page: Page, url: string) {
  await page.goto(`${url}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
}

async function pathSnapshot(page: Page) {
  return page.locator('.sidebar-category').filter({ has: page.locator('[data-category="image"]') }).locator('.category-paths').evaluate(element => ({
    rows: Array.from(element.querySelectorAll('.path-row')).map(row => ({
      text: row.textContent?.replace(/\s+/g, ' ').trim(),
      active: row.classList.contains('active'),
      depth: (row as HTMLElement).style.getPropertyValue('--depth').trim(),
      title: row.querySelector('.path-label')?.getAttribute('title'),
      expanded: row.querySelector('.path-toggle:not(.spacer)')?.getAttribute('aria-expanded') ?? null,
      toggleLabel: row.querySelector('.path-toggle:not(.spacer)')?.getAttribute('aria-label') ?? null,
    })),
    visibleLabels: Array.from(element.querySelectorAll('.path-label')).map(label => label.textContent?.replace(/\s+/g, ' ').trim()),
  }))
}

async function selectCategory(page: Page) {
  await page.locator('[data-category="image"]').click()
  await expect(page.locator('.library-view')).toBeVisible()
  await expect(page.locator('.category-paths .path-label', { hasText: '归档' })).toBeVisible()
}

test('旧版与 Rust 版分类路径树保留递归展开、计数、过滤、active 与根路径行为', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 950 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 950 } })
  await clearPreferences(oldContext)
  await clearPreferences(newContext)
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockSidebarTree(oldPage), mockSidebarTree(newPage)])
    await Promise.all([openShell(oldPage, oldUrl), openShell(newPage, newUrl)])
    await Promise.all([selectCategory(oldPage), selectCategory(newPage)])

    expect(await pathSnapshot(newPage)).toEqual(await pathSnapshot(oldPage))

    const oldPaths = oldPage.locator('.category-paths')
    const newPaths = newPage.locator('.category-paths')
    const oldArchive = oldPaths.locator('.path-label', { hasText: '归档' }).locator('..')
    const newArchive = newPaths.locator('.path-label', { hasText: '归档' }).locator('..')

    await Promise.all([oldArchive.locator('.path-label').click(), newArchive.locator('.path-label').click()])
    await Promise.all([
      expect(oldPage.locator('.library-view .folder-meta')).toContainText('归档'),
      expect(newPage.locator('.library-view .folder-meta')).toContainText('归档'),
    ])
    expect(await pathSnapshot(newPage)).toEqual(await pathSnapshot(oldPage))
    await expect(oldPage.locator('.library-view .file-card')).toHaveCount(3)
    await expect(newPage.locator('.library-view .file-card')).toHaveCount(3)

    await Promise.all([oldArchive.locator('.path-toggle').click(), newArchive.locator('.path-toggle').click()])
    await Promise.all([
      expect(oldPaths.locator('.path-label', { hasText: '旅行' })).toBeVisible(),
      expect(newPaths.locator('.path-label', { hasText: '旅行' })).toBeVisible(),
    ])
    expect(await pathSnapshot(newPage)).toEqual(await pathSnapshot(oldPage))

    const oldTravel = oldPaths.locator('.path-label', { hasText: '旅行' })
    const newTravel = newPaths.locator('.path-label', { hasText: '旅行' })
    await Promise.all([oldTravel.click(), newTravel.click()])
    await Promise.all([
      expect(oldPage.locator('.library-view .folder-meta')).toContainText('归档 / 旅行'),
      expect(newPage.locator('.library-view .folder-meta')).toContainText('归档 / 旅行'),
    ])
    expect(await pathSnapshot(newPage)).toEqual(await pathSnapshot(oldPage))
    await expect(oldPage.locator('.library-view .file-card')).toHaveCount(2)
    await expect(newPage.locator('.library-view .file-card')).toHaveCount(2)

    await Promise.all([
      oldPaths.locator('.path-label').first().click(),
      newPaths.locator('.path-label').first().click(),
    ])
    await Promise.all([
      expect(oldPage.locator('.library-view .folder-meta')).toContainText('全部位置'),
      expect(newPage.locator('.library-view .folder-meta')).toContainText('全部位置'),
    ])
    expect(await pathSnapshot(newPage)).toEqual(await pathSnapshot(oldPage))
    await expect(oldPage.locator('.library-view .file-card')).toHaveCount(4)
    await expect(newPage.locator('.library-view .file-card')).toHaveCount(4)
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('文件目录树反向审计记录 reference 的组件注册缺陷并保护 Rust 递归导航', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 950 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 950 } })
  await clearPreferences(oldContext)
  await clearPreferences(newContext)
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockSidebarTree(oldPage), mockSidebarTree(newPage)])
    await Promise.all([openShell(oldPage, oldUrl), openShell(newPage, newUrl)])
    await Promise.all([
      oldPage.locator('[data-category="file"]').click(),
      newPage.locator('[data-category="file"]').click(),
    ])
    await Promise.all([
      expect(oldPage.locator('.file-tree-root .path-label').first()).toContainText('我的文件'),
      expect(newPage.locator('.file-tree-root .path-label').first()).toContainText('我的文件'),
    ])
    await expect.poll(() => oldPage.locator('sidebardirectorynode').count()).toBe(2)
    await expect(newPage.locator('.file-tree-root > .path-children > .path-node')).toHaveCount(2)

    // SidebarFileTree.vue in the pinned reference omits the
    // SidebarDirectoryNode import, so Vue leaves unresolved custom elements
    // instead of rendering the returned directory rows. Keep that observation
    // explicit; the Rust implementation retains the intended usable tree.
    await expect(oldPage.locator('.file-tree-root > .path-children > .path-node')).toHaveCount(0)
    await expect(newPage.locator('.file-tree-root .path-label', { hasText: '归档' })).toBeVisible()
    await newPage.locator('.file-tree-root .path-label', { hasText: '归档' }).click()
    await expect(newPage.getByRole('heading', { name: '归档', exact: true })).toBeVisible()
    expect(new URL(newPage.url()).pathname).toBe('/f/dir-archive')
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})
