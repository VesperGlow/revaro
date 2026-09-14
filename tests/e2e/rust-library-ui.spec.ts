import { expect, test, type Page } from '@playwright/test'

// The reference library-ui.spec.ts predates the Rust Timestamp DTO and used
// empty date strings. Keep the same interaction assertions with valid wire
// timestamps so the test exercises the Rust deserializer as well as the DOM.

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'
const folder = (id: string, name: string) => ({ id, name })
const base = (value: Record<string, unknown>) => ({
  parent_id: ROOT,
  kind: 'file',
  size: 100000,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'application/octet-stream',
  etag: `etag-${String(value.id ?? 'item')}`,
  ...value,
})

const library = {
  book: [
    base({ id: 'book-1', name: '星海拾遗 第1卷.epub', mime_type: 'application/epub+zip', folder_path: [folder('books', '书籍')] }),
    base({ id: 'book-2', name: '星海拾遗 第2卷.epub', mime_type: 'application/epub+zip', folder_path: [folder('books', '书籍')] }),
    base({ id: 'book-3', name: '星海拾遗 第3卷.epub', mime_type: 'application/epub+zip', folder_path: [folder('books', '书籍')] }),
    base({ id: 'book-4', name: '独立短篇.epub', mime_type: 'application/epub+zip', folder_path: [folder('books', '书籍')] }),
  ],
  image: [
    base({ id: 'image-1', name: '群山.png', mime_type: 'image/png', folder_path: [folder('photos', 'Photos')] }),
    base({ id: 'image-2', name: '远山.png', mime_type: 'image/png', folder_path: [folder('photos', 'Photos')] }),
    base({ id: 'image-3', name: '海边.png', mime_type: 'image/png', folder_path: [] }),
  ],
  video: [
    base({ id: 'video-1', name: '山间漫步.webm', mime_type: 'video/webm', folder_path: [folder('videos', 'Videos')] }),
  ],
  audio: [
    base({ id: 'audio-1', name: '山间来信.m4a', mime_type: 'audio/mp4', duration_ms: 120000, folder_path: [folder('music', 'Music')] }),
    // The reference client probes this item because the library listing does
    // not include a duration for it.
    base({ id: 'audio-2', name: '夜航.mp3', mime_type: 'audio/mpeg', folder_path: [folder('music', 'Music')] }),
  ],
}

const counts = { book: 4, image: 3, video: 1, audio: 2, file: 11 }
const rootItems = [
  base({ id: 'dir-1', name: 'Photos', kind: 'directory', size: 0, mime_type: '' }),
  base({ id: 'dir-2', name: 'Music', kind: 'directory', size: 0, mime_type: '' }),
  base({ id: 'file-1', name: '报告.pdf', mime_type: 'application/pdf', size: 5000 }),
  base({ id: 'book-4', name: '独立短篇.epub', mime_type: 'application/epub+zip', size: 90000 }),
]

const cover = '<svg xmlns="http://www.w3.org/2000/svg" width="600" height="900" viewBox="0 0 600 900"><rect width="600" height="900" fill="#3f6b8f"/><rect x="40" y="60" width="520" height="780" fill="#2b4c68"/><text x="300" y="470" fill="#e8f1f8" font-size="64" text-anchor="middle">BOOK</text></svg>'

async function mockLibrary(page: Page, books = library.book) {
  const payload = { ...library, book: books }
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/library/all') return json({ items: payload, counts: { ...counts, book: books.length } })
    if (path === '/api/library/counts') return json({ ...counts, book: books.length })
    if (path === `/api/files/audio-2/audio`) return json({ duration: 125, chapters: [], cover_url: '', has_cover: false })
    if (path === `/api/files/${ROOT}/children`) return json({ items: rootItems, total_bytes: 900000, file_count: 4 })
    if (path === `/api/files/${ROOT}`) {
      return json({ file: base({ id: ROOT, name: '我的文件', kind: 'directory', parent_id: null, size: 0, mime_type: '' }), breadcrumbs: [] })
    }
    if (path.endsWith('/thumbnail') || path.endsWith('/preview') || path.endsWith('/cover')) {
      return route.fulfill({ contentType: 'image/svg+xml', body: cover })
    }
    return json({ items: [] })
  })
}

async function mockNestedLibrary(page: Page) {
  const nestedImages = [
    base({ id: 'image-root', name: '根目录.png', mime_type: 'image/png', folder_path: [] }),
    base({ id: 'image-travel-1', name: '旅行一.png', mime_type: 'image/png', folder_path: [folder('archive', '归档'), folder('travel', '旅行')] }),
    base({ id: 'image-travel-2', name: '旅行二.png', mime_type: 'image/png', folder_path: [folder('archive', '归档'), folder('travel', '旅行')] }),
    base({ id: 'image-work', name: '工作.png', mime_type: 'image/png', folder_path: [folder('archive', '归档'), folder('work', '工作')] }),
  ]
  const nestedCounts = { book: 0, image: 4, video: 0, audio: 0, file: 2 }
  const directory = (id: string, name: string, parent_id: string) => base({
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
  const archiveChildren = [
    directory('dir-travel', '旅行', 'dir-archive'),
    directory('dir-work', '工作', 'dir-archive'),
  ]

  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: nestedImages, video: [], audio: [] }, counts: nestedCounts })
    }
    if (path === '/api/library/counts') return json(nestedCounts)
    if (path === `/api/files/${ROOT}`) {
      return json({ file: base({ id: ROOT, name: '我的文件', kind: 'directory', parent_id: null, size: 0, mime_type: '' }), breadcrumbs: [] })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: rootChildren, total_bytes: 0, file_count: 0 })
    if (path === '/api/files/dir-archive') {
      return json({ file: directory('dir-archive', '归档', ROOT), breadcrumbs: [base({ id: ROOT, name: '我的文件', kind: 'directory', parent_id: null, mime_type: '' })] })
    }
    if (path === '/api/files/dir-archive/children') return json({ items: archiveChildren, total_bytes: 0, file_count: 0 })
    if (path === '/api/files/dir-travel') {
      return json({ file: directory('dir-travel', '旅行', 'dir-archive'), breadcrumbs: [base({ id: ROOT, name: '我的文件', kind: 'directory', parent_id: null, mime_type: '' }), directory('dir-archive', '归档', ROOT)] })
    }
    if (path === '/api/files/dir-travel/children') return json({ items: [], total_bytes: 0, file_count: 0 })
    if (path.endsWith('/thumbnail')) return route.fulfill({ contentType: 'image/svg+xml', body: cover })
    return json({ items: [] })
  })
}

async function openApp(page: Page) {
  await page.goto('/')
  await expect(page.getByRole('heading', { name: '我的文件' })).toBeVisible()
}

test('分类栏五个入口与书架、图库、音乐、文件视图完整切换', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 950 })
  await mockLibrary(page)
  await openApp(page)

  const sidebar = page.locator('.app-sidebar')
  for (const type of ['book', 'image', 'video', 'audio', 'file'] as const) {
    await expect(sidebar.locator(`[data-category="${type}"]`)).toBeVisible()
  }

  await sidebar.locator('[data-category="book"]').click()
  await expect(page.locator('.book-shelf')).toBeVisible()
  await expect(page.locator('.shelf-card')).toHaveCount(2)
  const series = page.locator('.series-card')
  await expect(series).toContainText('同系列 · 3 本')
  await expect(series.locator('.series-cover-main')).toHaveCount(1)
  await expect(series.locator('.series-cover-fan')).toHaveCount(2)
  expect(await page.locator('.shelf-card .shelf-meta strong').allTextContents()).toEqual(['星海拾遗', '独立短篇'])
  await expect(page.locator('.book-cover img').first()).toHaveAttribute('src', /\/thumbnail\?v=etag-book-1/)
  const single = page.locator('.shelf-card:not(.series-card)').first()
  const seriesBox = await series.boundingBox()
  const singleBox = await single.boundingBox()
  expect(seriesBox!.width).toBeCloseTo(singleBox!.width, 0)
  expect(seriesBox!.height).toBeCloseTo(singleBox!.height, 0)
  const singleCover = await single.locator('.shelf-cover').boundingBox()
  const singleImage = await single.locator('.book-cover').boundingBox()
  expect(singleImage!.width).toBeCloseTo(singleCover!.width, 0)
  expect(singleImage!.height).toBeCloseTo(singleCover!.height, 0)

  await sidebar.locator('[data-category="image"]').click()
  await expect(page.locator('.gallery-switch')).toBeVisible()
  await expect(page.locator('.library-view .file-card')).toHaveCount(3)
  await expect(page.locator('.library-view .file-card').first().locator('.card-preview')).toHaveAttribute('title', '预览图片')
  await page.getByRole('button', { name: '图库分类' }).click()
  await expect(page.locator('.album-list')).toBeVisible()
  await expect(page.locator('.album')).toHaveCount(2)
  await sidebar.locator('.path-label', { hasText: 'Photos' }).click()
  await expect(page.locator('.library-view .folder-meta')).toContainText('2 个项目')

  await sidebar.locator('[data-category="audio"]').click()
  await expect(page.locator('.view-switch')).toBeVisible()
  await page.getByRole('button', { name: '列表' }).click()
  await expect(page.locator('.file-rows .file-row')).toHaveCount(2)
  await expect(page.locator('.file-rows')).toContainText('2:05')

  await sidebar.locator('[data-category="video"]').click()
  await expect(page.locator('.gallery-switch')).toBeVisible()
  await expect(page.locator('.library-view .file-card')).toHaveCount(1)
  await expect(page.locator('.library-view .file-card').first().locator('.card-preview')).toHaveAttribute('title', '播放视频')

  await sidebar.locator('[data-category="file"]').click()
  await expect(page.locator('.content-head h1')).toHaveText('我的文件')
  await page.getByRole('button', { name: '列表' }).click()
  await expect(page.locator('.file-rows .file-row')).toHaveCount(4)
})

test('分类栏收起、路径树手风琴和移动端抽屉行为与 reference 一致', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 950 })
  await mockLibrary(page)
  await openApp(page)

  await page.locator('.sidebar-collapse').click()
  await expect(page.locator('.app-shell')).toHaveClass(/sidebar-collapsed/)
  await expect(page.locator('.app-sidebar')).toHaveClass(/collapsed/)
  await page.locator('.sidebar-collapse').click()
  await expect(page.locator('.app-shell')).not.toHaveClass(/sidebar-collapsed/)

  await page.locator('.app-sidebar [data-category="image"]').click()
  await expect(page.locator('.app-sidebar .path-label', { hasText: 'Photos' })).toBeVisible()
  await expect(page.locator('.app-sidebar .path-label', { hasText: 'Photos' }).locator('em')).toHaveText('2')
  await page.locator('.app-sidebar [data-category="book"]').click()
  await expect(page.locator('.app-sidebar .category-paths')).toHaveCount(1)
  await expect(page.locator('.app-sidebar .path-label', { hasText: '书籍' })).toBeVisible()
  await expect(page.locator('.app-sidebar .path-label', { hasText: 'Photos' })).toHaveCount(0)
  await page.locator('.app-sidebar [data-category="video"]').click()
  await expect(page.locator('.app-sidebar .category-paths')).toHaveCount(1)
  await expect(page.locator('.app-sidebar .path-label', { hasText: 'Videos' })).toBeVisible()

  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/')
  await expect(page.locator('.app-sidebar')).toBeHidden()
  const handle = page.locator('.sidebar-handle')
  await expect(handle).toHaveAttribute('aria-label', '展开分类栏')
  const closedContent = await page.locator('.content').boundingBox()
  await handle.click()
  const sidebar = page.locator('.app-sidebar')
  await expect(sidebar).toHaveClass(/mobile-open/)
  await expect(handle).toHaveClass(/open/)
  await expect(handle).toHaveAttribute('aria-label', '收起分类栏')
  await expect(sidebar.locator('.category-label').first()).toBeVisible()
  await expect(sidebar.locator('.category-expand')).toHaveCount(0)
  await expect(sidebar.locator('.category-count')).toHaveCount(0)
  await expect(sidebar.locator('.category-paths')).toHaveCount(0)
  await page.waitForTimeout(360)
  const drawer = await sidebar.boundingBox()
  const handleBox = await handle.boundingBox()
  expect(handleBox!.width).toBeCloseTo(32, 0)
  expect(handleBox!.height).toBeCloseTo(52, 0)
  expect(handleBox!.x + handleBox!.width).toBeGreaterThanOrEqual(drawer!.x + drawer!.width - 1)
  expect(handleBox!.x + handleBox!.width).toBeLessThanOrEqual(drawer!.x + drawer!.width + 14)
  const openContent = await page.locator('.content').boundingBox()
  expect(openContent!.x).toBeCloseTo(closedContent!.x, 1)
  await page.locator('.sidebar-backdrop').click({ position: { x: 360, y: 700 } })
  await expect(sidebar).not.toHaveClass(/mobile-open/)
  await handle.click()
  await page.keyboard.press('Escape')
  await expect(sidebar).not.toHaveClass(/mobile-open/)
  await handle.click()
  await sidebar.locator('[data-category="image"]').click()
  await expect(page.locator('.library-view')).toBeVisible()
  await expect(sidebar).not.toHaveClass(/mobile-open/)
})

test('同系列书籍数量增加时仍保持固定方形卡片', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 950 })
  const many = Array.from({ length: 12 }, (_, index) => base({
    id: `long-${index}`,
    name: `长夜行 第${index + 1}卷.epub`,
    mime_type: 'application/epub+zip',
    folder_path: [folder('books', '书籍')],
  }))
  await mockLibrary(page, many)
  await openApp(page)
  await page.locator('.app-sidebar [data-category="book"]').click()
  const series = page.locator('.series-card')
  await expect(series.locator('.series-cover-main')).toHaveCount(1)
  await expect(series.locator('.series-cover-fan')).toHaveCount(11)
  const stage = await series.locator('.series-stage').boundingBox()
  expect(stage!.width).toBeCloseTo(stage!.height, 0)
  const boxes = await Promise.all((await series.locator('.series-cover').all()).map(cover => cover.boundingBox()))
  expect(Math.min(...boxes.map(box => box!.x))).toBeLessThanOrEqual(stage!.x + 1)
  expect(Math.max(...boxes.map(box => box!.x + box!.width))).toBeGreaterThanOrEqual(stage!.x + stage!.width - 1)
  expect(Math.min(...boxes.map(box => box!.y))).toBeLessThanOrEqual(stage!.y + 1)
  expect(Math.max(...boxes.map(box => box!.y + box!.height))).toBeGreaterThanOrEqual(stage!.y + stage!.height - 1)
})

test('媒体库方块卡和音频列表行按 reference 阻止 Space 默认滚动', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await mockLibrary(page)
  await openApp(page)
  await page.evaluate(() => { document.body.style.minHeight = '2000px' })

  await page.locator('.sidebar-handle').click()
  await page.locator('.app-sidebar [data-category="image"]').click()
  const imageCard = page.locator('.library-view .file-card').first()
  await expect(imageCard).toBeVisible()
  await imageCard.focus()
  await page.evaluate(() => window.scrollTo(0, 150))
  const imageBefore = await page.evaluate(() => window.scrollY)
  await page.keyboard.press('Space')
  await expect.poll(() => page.evaluate(() => window.scrollY)).toBe(imageBefore)

  await page.locator('.sidebar-handle').click()
  await page.locator('.app-sidebar [data-category="audio"]').click()
  await page.getByRole('button', { name: '列表' }).click()
  const audioRow = page.locator('.library-view .file-row').first()
  await expect(audioRow).toBeVisible()
  await audioRow.focus()
  await page.evaluate(() => window.scrollTo(0, 150))
  const audioBefore = await page.evaluate(() => window.scrollY)
  await page.keyboard.press('Space')
  await expect.poll(() => page.evaluate(() => window.scrollY)).toBe(audioBefore)
})

test('分类路径树保持 reference 的计数、展开、过滤与导航', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 950 })
  await mockNestedLibrary(page)
  await openApp(page)

  const sidebar = page.locator('.app-sidebar')
  await sidebar.locator('[data-category="image"]').click()
  const libraryPaths = sidebar.locator('.category-paths')
  await expect(libraryPaths.locator('.path-label')).toHaveCount(2)
  await expect(libraryPaths.locator('.path-label').first()).toContainText('我的文件')
  await expect(libraryPaths.locator('.path-label').first().locator('em')).toHaveText('4')
  const archive = libraryPaths.locator('.path-label', { hasText: '归档' })
  await expect(archive.locator('em')).toHaveText('3')
  await expect(libraryPaths.locator('.path-label', { hasText: '旅行' })).toHaveCount(0)
  await expect(libraryPaths.locator('.path-row').first()).toHaveClass(/active/)

  await archive.click()
  await expect(page.locator('.library-view .folder-meta')).toContainText('归档')
  await expect(page.locator('.library-view .file-card')).toHaveCount(3)
  await expect(archive.locator('..')).toHaveClass(/active/)
  await archive.locator('..').locator('.path-toggle').click()
  await expect(libraryPaths.locator('.path-label', { hasText: '旅行' })).toBeVisible()
  await expect(libraryPaths.locator('.path-label', { hasText: '旅行' }).locator('em')).toHaveText('2')
  await expect(libraryPaths.locator('.path-label', { hasText: '工作' }).locator('em')).toHaveText('1')

  const travel = libraryPaths.locator('.path-label', { hasText: '旅行' })
  await travel.click()
  await expect(page.locator('.library-view .folder-meta')).toContainText('归档 / 旅行')
  await expect(page.locator('.library-view .file-card')).toHaveCount(2)
  await expect(travel.locator('..')).toHaveClass(/active/)
  await libraryPaths.locator('.path-label').first().click()
  await expect(page.locator('.library-view .folder-meta')).toContainText('全部位置')
  await expect(page.locator('.library-view .file-card')).toHaveCount(4)
  await expect(libraryPaths.locator('.path-row').first()).toHaveClass(/active/)

})

test('分类没有内容时保留 reference 的路径提示、空态文案与上传入口', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 950 })
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 0 } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 0 })
    if (path === `/api/files/${ROOT}`) {
      return json({ file: base({ id: ROOT, name: '我的文件', kind: 'directory', parent_id: null, size: 0, mime_type: '' }), breadcrumbs: [] })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    return json({ items: [] })
  })

  await openApp(page)
  const sidebar = page.locator('.app-sidebar')
  await sidebar.locator('[data-category="image"]').click()
  await expect(page.getByRole('heading', { name: '图片', exact: true })).toBeVisible()
  await expect(sidebar.locator('.path-loading')).toHaveText('还没有图片内容')
  const empty = page.locator('.library-view .state.empty')
  await expect(empty.locator('.empty-icon')).toHaveText('⌁')
  await expect(empty.locator('h3')).toHaveText('这里还没有图片内容')
  await expect(empty.locator('p')).toHaveText('上传后会自动归类到这里。')
  await expect(empty.getByRole('button', { name: '上传文件' })).toBeVisible()
  await expect(sidebar.locator('[data-category="image"] .category-count')).toHaveCount(0)
})

test('分类读取失败、刷新 loading 和 RefreshCw 图标保持 reference 行为', async ({ page }) => {
  let libraryRequests = 0
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      libraryRequests += 1
      if (libraryRequests === 1) {
        return route.fulfill({ status: 503, json: { error: { status: 503, message: '分类读取失败' } } })
      }
      await new Promise(resolve => setTimeout(resolve, 250))
      return json({
        items: { book: [], image: [base({ id: 'retry-image', name: '重试图片.png', mime_type: 'image/png', folder_path: [] })], video: [], audio: [] },
        counts: { book: 0, image: 1, video: 0, audio: 0, file: 0 },
      })
    }
    if (path === `/api/files/${ROOT}`) return json({ file: base({ id: ROOT, name: '我的文件', kind: 'directory', parent_id: null, mime_type: '' }), breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    if (path.endsWith('/thumbnail')) return route.fulfill({ contentType: 'image/svg+xml', body: cover })
    return json({ items: [] })
  })

  await page.goto('/')
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.locator('[data-category="image"]').click()
  await expect(page.getByRole('heading', { name: '图片', exact: true })).toBeVisible()
  await expect(page.locator('.library-view .state')).toContainText('分类读取失败')
  const refreshPaths = await page.locator('.library-head .secondary svg path').evaluateAll(paths => paths.map(path => path.getAttribute('d')))
  expect(refreshPaths).toEqual([
    'M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8',
    'M21 3v5h-5',
    'M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16',
    'M8 16H3v5',
  ])

  await page.locator('.library-head').getByRole('button', { name: '刷新', exact: true }).click()
  await expect(page.locator('.library-view .state')).toContainText('正在整理图片…')
  await expect(page.locator('.library-view .file-card').filter({ hasText: '重试图片.png' })).toBeVisible()
})
