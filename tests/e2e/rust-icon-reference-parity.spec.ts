import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

const task = (value: Record<string, unknown>) => ({
  id: `compat-${String(value.id ?? 'task')}`,
  type: 'upload',
  status: 'completed',
  phase: 'completed',
  progress: 100,
  speed: 0,
  retry_count: 0,
  max_retries: 3,
  error: '',
  source_type: '',
  source_id: '',
  cancel_requested: false,
  created_at: STAMP,
  updated_at: STAMP,
  finished_at: STAMP,
  name: '已完成任务',
  ...value,
})

const image = {
  id: 'compat-image',
  parent_id: ROOT,
  kind: 'file',
  size: 100,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'image/png',
  etag: 'compat-image-etag',
  name: '参考图片.png',
  folder_path: [{ id: 'compat-folder', name: '参考路径' }],
}

const status = {
  status: 'ok',
  database: { status: 'ok', bytes: 1024 },
  storage: { status: 'ok', bytes: 2048, trash_bytes: 512, file_count: 1 },
  cache: {
    status: 'ok',
    memory_bytes: 1024,
    disk_bytes: 2048,
    memory_entries: 1,
    disk_entries: 2,
    classes: { default: { hits: 3, misses: 1, loads: 1, load_errors: 0, evictions: 0 } },
  },
}

async function mockShell(page: Page) {
  const tasks = [
    task({ id: 'active', status: 'running', phase: '上传中', progress: 42, name: '活动任务.bin' }),
    task({ id: 'waiting', type: 'archive_extract', status: 'waiting_input', phase: 'waiting_input', progress: 12, name: '密码归档.zip' }),
    ...Array.from({ length: 5 }, (_, index) => task({ id: `done-${index}`, name: `已完成 ${index + 1}` })),
    task({ id: 'failed', status: 'failed', phase: 'failed', progress: 100, name: '失败任务.zip', error: '失败' }),
  ]

  await page.route('**/api/**', async route => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })

    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/system/status/stream') {
      return route.fulfill({
        contentType: 'text/event-stream',
        body: `event: status\ndata: ${JSON.stringify(status)}\n\n`,
      })
    }
    if (path === '/api/tasks' && request.method() === 'GET') return json({ items: tasks })
    if (path === '/api/library/all') {
      return json({
        items: { book: [], image: [image], video: [], audio: [] },
        counts: { book: 0, image: 1, video: 0, audio: 0, file: 1 },
      })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 1, video: 0, audio: 0, file: 1 })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: {
          id: ROOT,
          parent_id: null,
          name: '我的文件',
          kind: 'directory',
          size: 0,
          status: 'ready',
          created_at: STAMP,
          updated_at: STAMP,
          mime_type: '',
        },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    if (path.endsWith('/thumbnail')) {
      return route.fulfill({ contentType: 'image/svg+xml', body: '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 2 2"><rect width="2" height="2" fill="white"/></svg>' })
    }
    return json({ items: [] })
  })
}

async function mockAmbiguousFileKinds(page: Page) {
  const files = [
    { id: 'icon-ambiguous-book', name: 'MIME 冲突.epub', kind: 'file', size: 100, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: 'audio/mpeg' },
    { id: 'icon-ambiguous-editable', name: 'MIME 冲突.txt', kind: 'file', size: 100, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: 'audio/mpeg' },
    { id: 'icon-audio', name: '普通音频.mp3', kind: 'file', size: 100, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: 'audio/mpeg' },
    { id: 'icon-generic', name: '普通归档.zip', kind: 'file', size: 100, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: 'application/zip' },
  ]
  await page.route('**/api/**', async route => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: files.length } })
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: files.length })
    if (path === `/api/files/${ROOT}`) return json({ file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' }, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: files, total_bytes: files.length * 100, file_count: files.length })
    if (path.endsWith('/thumbnail')) return route.fulfill({ status: 404, body: '' })
    return json({ items: [] })
  })
}

async function openShell(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
}

async function iconGeometry(page: Page, selector: string) {
  return page.locator(selector).evaluateAll(svgs => svgs.map(svg => Array.from(
    svg.querySelectorAll('path,rect,circle,ellipse,line,polyline,polygon'),
  ).map(element => ({
    tag: element.tagName.toLowerCase(),
    attributes: Object.fromEntries(Array.from(element.attributes)
      .filter(attribute => ['d', 'points', 'x', 'y', 'width', 'height', 'rx', 'ry', 'cx', 'cy', 'r', 'x1', 'x2', 'y1', 'y2', 'fill'].includes(attribute.name))
      .sort((left, right) => left.name.localeCompare(right.name))
      .map(attribute => [attribute.name, attribute.value])),
  }))))
}

async function compareIcons(oldPage: Page, newPage: Page, selector: string, label: string) {
  const oldGeometry = await iconGeometry(oldPage, selector)
  const newGeometry = await iconGeometry(newPage, selector)
  expect(newGeometry, `${label} 的 Rust SVG 几何应保持 reference`).toEqual(oldGeometry)
}

async function cssTransform(page: Page, selector: string) {
  return page.locator(selector).first().evaluate(element => getComputedStyle(element).transform)
}

test('旧版与 Rust 版全局入口、任务中心和文件操作图标保持 reference 几何', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 950 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 950 } })
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
    await Promise.all([mockShell(oldPage), mockShell(newPage)])
    await Promise.all([openShell(oldPage, oldUrl), openShell(newPage, newUrl)])

    await compareIcons(oldPage, newPage, '.task-center > summary[title="任务中心"] svg', '任务中心入口')
    await compareIcons(oldPage, newPage, '.app-sidebar .sidebar-collapse svg', '侧栏折叠入口')
    await compareIcons(oldPage, newPage, '.app-sidebar .category-icon svg', '侧栏分类入口')
    await compareIcons(oldPage, newPage, '.app-sidebar .category-expand svg', '侧栏路径展开入口')
    await compareIcons(oldPage, newPage, '.app-sidebar .trash-entry svg', '侧栏回收站入口')
    await compareIcons(oldPage, newPage, '.file-view-switch svg', '文件视图切换')
    await compareIcons(oldPage, newPage, '.desktop-create-actions svg', '快速新建操作')
    await compareIcons(oldPage, newPage, '.create-menu summary svg', '新建菜单入口')
    await compareIcons(oldPage, newPage, '.create-menu .create-menu-popover svg', '新建菜单项')
    await compareIcons(oldPage, newPage, '.upload-menu summary svg', '上传菜单入口')
    await compareIcons(oldPage, newPage, '.upload-menu .upload-menu-popover svg', '上传菜单项')

    await oldPage.locator('.sidebar-nav [data-category="image"]').click()
    await newPage.locator('.sidebar-nav [data-category="image"]').click()
    const oldImagePaths = oldPage.locator('.sidebar-category:has([data-category="image"]) .category-paths .path-label')
    const newImagePaths = newPage.locator('.sidebar-category:has([data-category="image"]) .category-paths .path-label')
    await expect(oldImagePaths).toHaveCount(2)
    await expect(newImagePaths).toHaveCount(2)
    await oldPage.waitForTimeout(250)
    await newPage.waitForTimeout(250)
    expect(
      await cssTransform(oldPage, '.sidebar-nav [data-category="image"] ~ .category-expand svg'),
      'reference 分类展开箭头应旋转',
    ).not.toBe('none')
    expect(
      await cssTransform(newPage, '.sidebar-nav [data-category="image"] ~ .category-expand svg'),
      'Rust 分类展开箭头应保持 reference 旋转状态',
    ).toBe(await cssTransform(oldPage, '.sidebar-nav [data-category="image"] ~ .category-expand svg'))
    expect(
      await cssTransform(oldPage, '.sidebar-category:has([data-category="image"]) .category-paths button.path-toggle svg'),
      'reference 路径树根箭头应旋转',
    ).not.toBe('none')
    expect(
      await cssTransform(newPage, '.sidebar-category:has([data-category="image"]) .category-paths button.path-toggle svg'),
      'Rust 路径树根箭头应保持 reference 旋转状态',
    ).toBe(await cssTransform(oldPage, '.sidebar-category:has([data-category="image"]) .category-paths button.path-toggle svg'))
    await compareIcons(oldPage, newPage, '.sidebar-category:has([data-category="image"]) .category-paths .path-label svg', '媒体路径文件夹')

    await oldPage.locator('.task-center > summary[title="任务中心"]').click()
    await newPage.locator('.task-center > summary[title="任务中心"]').click()
    await expect(oldPage.locator('.task-center .active-group')).toBeVisible()
    await expect(newPage.locator('.task-center .active-group')).toBeVisible()
    await compareIcons(oldPage, newPage, '.task-center .active-group .actions button[title="取消"] svg', '任务取消')
    await compareIcons(oldPage, newPage, '.task-center .active-group .actions button[title="输入密码"] svg', '任务密码')
    await compareIcons(oldPage, newPage, '.task-center .failed-group .actions button[title="重试"] svg', '任务重试')
    await compareIcons(oldPage, newPage, '.task-center .completed-group > .expand svg', '任务完成列表展开')

    await oldPage.locator('.system-status summary').click()
    await newPage.locator('.system-status summary').click()
    await expect(oldPage.locator('.status-grid .service-card')).toHaveCount(3)
    await expect(newPage.locator('.status-grid .service-card')).toHaveCount(3)
    await compareIcons(oldPage, newPage, '.status-grid .service-icon svg', '系统状态服务卡')

    await oldPage.setViewportSize({ width: 390, height: 844 })
    await newPage.setViewportSize({ width: 390, height: 844 })
    await oldPage.waitForTimeout(200)
    await newPage.waitForTimeout(200)
    await compareIcons(oldPage, newPage, '.sidebar-handle svg', '移动端分类抽屉')
    await compareIcons(oldPage, newPage, '.mobile-account-menu .mobile-tool-item:first-of-type svg', '移动端任务中心')
    await compareIcons(oldPage, newPage, '.mobile-account-menu .mobile-trash svg', '移动端回收站')
    await compareIcons(oldPage, newPage, '.mobile-account-menu .mobile-tool-item:last-of-type svg', '移动端账户设置')
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('文件图标的重叠类型回退顺序保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function snapshot(page: Page, baseUrl: string) {
    await mockAmbiguousFileKinds(page)
    await page.goto(`${baseUrl}/?icon-overlap-reference=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    const result: Record<string, string> = {}
    for (const name of ['MIME 冲突.epub', 'MIME 冲突.txt', '普通音频.mp3', '普通归档.zip']) {
      const card = page.locator('.file-card').filter({ hasText: name })
      await expect(card).toBeVisible()
      await expect.poll(async () => card.locator('.file-type-icon, .large-video').count(), { timeout: 3000 }).toBe(1)
      result[name] = await card.locator('.file-type-icon, .large-video').evaluate(element => {
        if (element.classList.contains('large-video')) return 'video'
        return ['book-type-icon', 'document-type-icon', 'audio-type-icon', 'folder-type-icon', 'generic-type-icon']
          .find(value => element.classList.contains(value)) ?? ''
      })
    }
    return result
  }

  try {
    const [oldState, newState] = await Promise.all([
      snapshot(oldPage, oldUrl),
      snapshot(newPage, newUrl),
    ])
    expect(newState, 'Rust 文件重叠类型图标回退顺序与 reference 不一致').toEqual(oldState)
    expect(oldState).toEqual({
      'MIME 冲突.epub': 'book-type-icon',
      'MIME 冲突.txt': 'document-type-icon',
      '普通音频.mp3': 'audio-type-icon',
      '普通归档.zip': 'generic-type-icon',
    })
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})
