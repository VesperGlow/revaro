import { test, expect, type Page } from '@playwright/test'

// 分类栏与媒体库视图（书架/图片/视频/音乐/文件）的 UI 冒烟测试，全部走本地 mock，无需后端。

const ROOT = '00000000-0000-0000-0000-000000000000'
const folder = (id: string, name: string) => ({ id, name })

const library = {
  book: [
    { id: 'book-1', parent_id: 'books', name: '星海拾遗 第1卷.epub', kind: 'file', size: 120000, status: 'ready', created_at: '', updated_at: '', folder_path: [folder('books', '书籍')] },
    { id: 'book-2', parent_id: 'books', name: '星海拾遗 第2卷.epub', kind: 'file', size: 130000, status: 'ready', created_at: '', updated_at: '', folder_path: [folder('books', '书籍')] },
    { id: 'book-3', parent_id: 'books', name: '星海拾遗 第3卷.epub', kind: 'file', size: 140000, status: 'ready', created_at: '', updated_at: '', folder_path: [folder('books', '书籍')] },
    { id: 'book-4', parent_id: 'books', name: '独立短篇.epub', kind: 'file', size: 90000, status: 'ready', created_at: '', updated_at: '', folder_path: [folder('books', '书籍')] },
  ],
  image: [
    { id: 'image-1', parent_id: 'photos', name: '群山.png', kind: 'file', size: 200000, status: 'ready', created_at: '', updated_at: '', folder_path: [folder('photos', 'Photos')] },
    { id: 'image-2', parent_id: 'photos', name: '远山.png', kind: 'file', size: 210000, status: 'ready', created_at: '', updated_at: '', folder_path: [folder('photos', 'Photos')] },
    { id: 'image-3', parent_id: ROOT, name: '海边.png', kind: 'file', size: 220000, status: 'ready', created_at: '', updated_at: '', folder_path: [] },
  ],
  video: [
    { id: 'video-1', parent_id: 'videos', name: '山间漫步.webm', kind: 'file', size: 900000, status: 'ready', created_at: '', updated_at: '', folder_path: [folder('videos', 'Videos')] },
  ],
  audio: [
    { id: 'audio-1', parent_id: 'music', name: '山间来信.m4a', kind: 'file', size: 400000, status: 'ready', created_at: '', updated_at: '', duration_ms: 120000, folder_path: [folder('music', 'Music')] },
    { id: 'audio-2', parent_id: 'music', name: '夜航.mp3', kind: 'file', size: 420000, status: 'ready', created_at: '', updated_at: '', folder_path: [folder('music', 'Music')] },
  ],
}
const counts = { book: 4, image: 3, video: 1, audio: 2, file: 11 }

const rootItems = [
  { id: 'dir-1', parent_id: ROOT, name: 'Photos', kind: 'directory', size: 0, status: 'ready', created_at: '', updated_at: '' },
  { id: 'dir-2', parent_id: ROOT, name: 'Music', kind: 'directory', size: 0, status: 'ready', created_at: '', updated_at: '' },
  { id: 'file-1', parent_id: ROOT, name: '报告.pdf', kind: 'file', size: 5000, status: 'ready', created_at: '', updated_at: '' },
  { id: 'book-4', parent_id: ROOT, name: '独立短篇.epub', kind: 'file', size: 90000, status: 'ready', created_at: '', updated_at: '' },
]

const cover = `<svg xmlns="http://www.w3.org/2000/svg" width="600" height="900" viewBox="0 0 600 900"><rect width="600" height="900" fill="#3f6b8f"/><rect x="40" y="60" width="520" height="780" fill="#2b4c68"/><text x="300" y="470" fill="#e8f1f8" font-size="64" text-anchor="middle">BOOK</text></svg>`

async function mockLibrary(page: Page, books = library.book) {
  const payload = { ...library, book: books }
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/auth/login') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') return route.fulfill({ contentType: 'text/event-stream', body: '' })
    if (path === '/api/library/all') return json({ items: payload, counts: { ...counts, book: books.length } })
    if (path === '/api/library/counts') return json({ ...counts, book: books.length })
    if (path === `/api/files/${ROOT}/children`) return json({ items: rootItems, total_bytes: 900000, file_count: 4 })
    if (path === `/api/files/${ROOT}`) return json({ file: { id: ROOT, name: '我的文件', kind: 'directory' }, breadcrumbs: [] })
    if (path === '/api/files/photos/children') return json({ items: [], total_bytes: 0, file_count: 0 })
    if (path === '/api/files/music/children') return json({ items: [], total_bytes: 0, file_count: 0 })
    if (path === '/api/files/videos/children') return json({ items: [], total_bytes: 0, file_count: 0 })
    if (path.endsWith('/audio')) return json({ duration: 125, has_cover: false, chapters: [] })
    if (path.endsWith('/thumbnail') || path.endsWith('/preview')) return route.fulfill({ contentType: 'image/svg+xml', body: cover })
    return json({ items: [] })
  })
}

async function login(page: Page) {
  await page.goto('/')
  await expect(page.getByRole('heading', { name: '我的文件' })).toBeVisible()
}

test('分类栏在五个大类之间切换并展示对应视图', async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 1440, height: 950 })
  await mockLibrary(page)
  await login(page)

  const sidebar = page.locator('.app-sidebar')
  await expect(sidebar).toBeVisible()
  for (const type of ['book', 'image', 'video', 'audio', 'file'] as const) {
    await expect(sidebar.locator(`[data-category="${type}"]`)).toBeVisible()
  }

  // 书架：无文件夹，封面方块展示，同系列收成一张固定尺寸的错位卡牌。
  await sidebar.locator('[data-category="book"]').click()
  await expect(page.locator('.book-shelf')).toBeVisible()
  await expect(page.locator('.shelf-card')).toHaveCount(2)
  const series = page.locator('.series-card')
  await expect(series).toContainText('同系列 · 3 本')
  await expect(series.locator('.series-cover-main')).toHaveCount(1)
  await expect(series.locator('.series-cover-fan')).toHaveCount(2)

  // 系列卡片固定方形：与单本卡片同尺寸，不因数量变化。
  const single = page.locator('.shelf-card:not(.series-card)').first()
  const seriesBox = await series.boundingBox()
  const singleBox = await single.boundingBox()
  expect(seriesBox!.width).toBeCloseTo(singleBox!.width, 0)
  expect(seriesBox!.height).toBeCloseTo(singleBox!.height, 0)

  // 单本卡片：封面几乎铺满整个固定容器，没有额外的白色底卡。
  const singleCover = await single.locator('.shelf-cover').boundingBox()
  const singleImage = await single.locator('.book-cover').boundingBox()
  expect(singleImage!.width).toBeCloseTo(singleCover!.width, 0)
  expect(singleImage!.height).toBeCloseTo(singleCover!.height, 0)

  // 系列：多本封面共同铺满整个容器区域（允许边缘裁切）。
  const stage = await series.locator('.series-stage').boundingBox()
  const boxes = await Promise.all((await series.locator('.series-cover').all()).map(cover => cover.boundingBox()))
  const left = Math.min(...boxes.map(box => box!.x))
  const right = Math.max(...boxes.map(box => box!.x + box!.width))
  const top = Math.min(...boxes.map(box => box!.y))
  const bottom = Math.max(...boxes.map(box => box!.y + box!.height))
  expect(left).toBeLessThanOrEqual(stage!.x + 1)
  expect(right).toBeGreaterThanOrEqual(stage!.x + stage!.width - 1)
  expect(top).toBeLessThanOrEqual(stage!.y + 1)
  expect(bottom).toBeGreaterThanOrEqual(stage!.y + stage!.height - 1)

  // 第一本完整展示，不被容器裁掉。
  const mainBox = await series.locator('.series-cover-main').boundingBox()
  expect(mainBox!.x).toBeGreaterThanOrEqual(stage!.x - 1)
  expect(mainBox!.y).toBeGreaterThanOrEqual(stage!.y - 1)
  expect(mainBox!.y + mainBox!.height).toBeLessThanOrEqual(stage!.y + stage!.height + 1)
  await page.screenshot({ path: testInfo.outputPath('bookshelf.png') })

  // 图片：全部视图 + 图库分类，底部可切换。
  await sidebar.locator('[data-category="image"]').click()
  await expect(page.locator('.gallery-switch')).toBeVisible()
  await expect(page.locator('.library-view .file-card')).toHaveCount(3)
  await page.getByRole('button', { name: '图库分类' }).click()
  await expect(page.locator('.album-list')).toBeVisible()
  await expect(page.locator('.album')).toHaveCount(2)
  await page.screenshot({ path: testInfo.outputPath('gallery-albums.png') })

  // 点击路径过滤：只显示 Photos 里的两张图。
  await sidebar.locator('.path-label', { hasText: 'Photos' }).click()
  await expect(page.locator('.library-view .folder-meta')).toContainText('2 个项目')

  // 音乐：方块 / 列表切换，列表显示时长。
  await sidebar.locator('[data-category="audio"]').click()
  await expect(page.locator('.view-switch')).toBeVisible()
  await page.getByRole('button', { name: '列表' }).click()
  await expect(page.locator('.file-rows .file-row')).toHaveCount(2)
  await expect(page.locator('.file-rows')).toContainText('2:05')
  await page.screenshot({ path: testInfo.outputPath('music-list.png') })

  // 视频：与图片共享图库切换，但数据相互独立。
  await sidebar.locator('[data-category="video"]').click()
  await expect(page.locator('.gallery-switch')).toBeVisible()
  await expect(page.locator('.library-view .file-card')).toHaveCount(1)

  // 文件：普通目录浏览，保留方块并新增列表视图。
  await sidebar.locator('[data-category="file"]').click()
  await expect(page.locator('.content-head h1')).toHaveText('我的文件')
  await page.getByRole('button', { name: '列表' }).click()
  await expect(page.locator('.file-rows .file-row')).toHaveCount(4)
  await page.screenshot({ path: testInfo.outputPath('files-list.png') })
})

test('分类栏可收起并可展开路径', async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 1440, height: 950 })
  await mockLibrary(page)
  await login(page)

  await page.locator('.sidebar-collapse').click()
  await expect(page.locator('.app-shell')).toHaveClass(/sidebar-collapsed/)
  await expect(page.locator('.app-sidebar')).toHaveClass(/collapsed/)
  await page.screenshot({ path: testInfo.outputPath('sidebar-collapsed.png') })
  await page.locator('.sidebar-collapse').click()
  await expect(page.locator('.app-shell')).not.toHaveClass(/sidebar-collapsed/)

  // 图片大类的路径树包含 Photos 节点与其计数。
  await page.locator('.app-sidebar [data-category="image"]').click()
  const photos = page.locator('.app-sidebar .path-label', { hasText: 'Photos' })
  await expect(photos).toBeVisible()
  await expect(photos.locator('em')).toHaveText('2')

  // 手风琴：打开书架后，图片的路径树自动收起，同时只有一个路径树。
  await page.locator('.app-sidebar [data-category="book"]').click()
  await expect(page.locator('.app-sidebar .category-paths')).toHaveCount(1)
  await expect(page.locator('.app-sidebar .path-label', { hasText: '书籍' })).toBeVisible()
  await expect(page.locator('.app-sidebar .path-label', { hasText: 'Photos' })).toHaveCount(0)
  await page.locator('.app-sidebar [data-category="video"]').click()
  await expect(page.locator('.app-sidebar .category-paths')).toHaveCount(1)
  await expect(page.locator('.app-sidebar .path-label', { hasText: '书籍' })).toHaveCount(0)
  await expect(page.locator('.app-sidebar .path-label', { hasText: 'Videos' })).toBeVisible()
})

test('同系列书籍再多也收在同一张固定尺寸卡片内', async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 1440, height: 950 })
  const many = Array.from({ length: 12 }, (_, index) => ({
    id: `long-${index}`,
    parent_id: 'books',
    name: `长夜行 第${index + 1}卷.epub`,
    kind: 'file',
    size: 100000,
    status: 'ready',
    created_at: '',
    updated_at: '',
    folder_path: [folder('books', '书籍')],
  }))
  await mockLibrary(page, many)
  await login(page)
  await page.locator('.app-sidebar [data-category="book"]').click()

  const series = page.locator('.series-card')
  await expect(series.locator('.series-cover-main')).toHaveCount(1)
  await expect(series.locator('.series-cover-fan')).toHaveCount(11)

  // 卡片保持方形：高度由宽度决定，与数量无关。
  const stage = await series.locator('.series-stage').boundingBox()
  expect(stage!.width).toBeCloseTo(stage!.height, 0)

  // 12 本封面共同铺满整块区域：左缘贴左、右缘抵达（并裁切于）右边界。
  const boxes = await Promise.all((await series.locator('.series-cover').all()).map(cover => cover.boundingBox()))
  expect(Math.min(...boxes.map(box => box!.x))).toBeLessThanOrEqual(stage!.x + 1)
  expect(Math.max(...boxes.map(box => box!.x + box!.width))).toBeGreaterThanOrEqual(stage!.x + stage!.width - 1)
  expect(Math.min(...boxes.map(box => box!.y))).toBeLessThanOrEqual(stage!.y + 1)
  expect(Math.max(...boxes.map(box => box!.y + box!.height))).toBeGreaterThanOrEqual(stage!.y + stage!.height - 1)
  await page.screenshot({ path: testInfo.outputPath('bookshelf-many.png') })
})

test('移动端浮动抽屉：开关跟随、内容不位移、backdrop 与 Esc 关闭', async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await mockLibrary(page)
  await login(page)

  const sidebar = page.locator('.app-sidebar')
  const handle = page.locator('.sidebar-handle')
  const content = page.locator('.content')
  await expect(sidebar).toBeHidden()
  await expect(handle).toBeVisible()
  await expect(handle).toHaveAttribute('aria-label', '展开分类栏')
  const closedContent = await content.boundingBox()

  // 展开：抽屉滑入，按钮跟随到抽屉右边缘，图标切换为 <
  await handle.click()
  await expect(sidebar).toHaveClass(/mobile-open/)
  await expect(handle).toHaveClass(/open/)
  await expect(handle).toHaveAttribute('aria-label', '收起分类栏')
  // 抽屉里分类是图标+文字的横向完整布局，且只有一级入口
  await expect(sidebar.locator('.category-label').first()).toBeVisible()
  await expect(sidebar.locator('[data-category="image"]')).toBeVisible()
  await expect(sidebar.locator('.category-expand')).toHaveCount(0)
  await expect(sidebar.locator('.category-count')).toHaveCount(0)
  await expect(sidebar.locator('.category-paths')).toHaveCount(0)
  // 一级入口约 50px 高，选中项是一整行淡蓝底（宽度铺满抽屉）
  const activeRow = sidebar.locator('[data-category="file"]')
  const activeBox = await activeRow.boundingBox()
  expect(activeBox!.height).toBeGreaterThanOrEqual(48)
  expect(activeBox!.height).toBeLessThanOrEqual(52)
  // 等滑入动画（250ms）结束再量尺寸
  await page.waitForTimeout(360)
  const drawer = await sidebar.boundingBox()
  expect(Math.abs(activeBox!.width - drawer!.width)).toBeLessThanOrEqual(2)
  // 回收站与主分类之间有一条浅色分隔线
  const dividerWidth = await sidebar.locator('.sidebar-foot').evaluate(el => getComputedStyle(el).borderTopWidth)
  expect(dividerWidth).not.toBe('0px')
  // 主内容不被推动
  const openContent = await content.boundingBox()
  expect(openContent!.x).toBeCloseTo(closedContent!.x, 1)
  // 开关贴在抽屉右边缘（仅少量外露）并换成 <
  const handleBox = await handle.boundingBox()
  expect(handleBox!.width).toBeCloseTo(32, 0)
  expect(handleBox!.height).toBeCloseTo(52, 0)
  expect(handleBox!.x + handleBox!.width).toBeGreaterThanOrEqual(drawer!.x + drawer!.width - 1)
  expect(handleBox!.x + handleBox!.width).toBeLessThanOrEqual(drawer!.x + drawer!.width + 14)
  // 开关固定在屏幕左侧垂直居中
  expect(handleBox!.y + handleBox!.height / 2).toBeCloseTo(422, 0)
  await page.screenshot({ path: testInfo.outputPath('mobile-drawer.png') })

  // 点击 backdrop 空白处立即收起
  await page.locator('.sidebar-backdrop').click({ position: { x: 360, y: 700 } })
  await expect(sidebar).not.toHaveClass(/mobile-open/)

  // Esc 收起
  await handle.click()
  await expect(sidebar).toHaveClass(/mobile-open/)
  await page.keyboard.press('Escape')
  await expect(sidebar).not.toHaveClass(/mobile-open/)

  // 通过悬浮按钮再次展开后选择分类，抽屉自动收起
  await handle.click()
  await sidebar.locator('[data-category="image"]').click()
  await expect(page.locator('.library-view')).toBeVisible()
  await expect(sidebar).not.toHaveClass(/mobile-open/)
})
