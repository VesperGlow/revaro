import { test, expect, type Page } from '@playwright/test'

// 分类栏与媒体库视图（书架/图片/视频/音乐/文件）的 UI 冒烟测试，全部走本地 mock，无需后端。

const ROOT = '00000000-0000-0000-0000-000000000000'
const folder = (id: string, name: string) => ({ id, name })

const library = {
  book: [
    { id: 'book-1', parent_id: ROOT, name: '星海拾遗 第1卷.epub', kind: 'file', size: 120000, status: 'ready', created_at: '', updated_at: '', folder_path: [] },
    { id: 'book-2', parent_id: ROOT, name: '星海拾遗 第2卷.epub', kind: 'file', size: 130000, status: 'ready', created_at: '', updated_at: '', folder_path: [] },
    { id: 'book-3', parent_id: ROOT, name: '星海拾遗 第3卷.epub', kind: 'file', size: 140000, status: 'ready', created_at: '', updated_at: '', folder_path: [] },
    { id: 'book-4', parent_id: ROOT, name: '独立短篇.epub', kind: 'file', size: 90000, status: 'ready', created_at: '', updated_at: '', folder_path: [] },
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

async function mockLibrary(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/auth/login') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') return route.fulfill({ contentType: 'text/event-stream', body: '' })
    if (path === '/api/library/all') return json({ items: library, counts })
    if (path === '/api/library/counts') return json(counts)
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

  // 书架：无文件夹，封面方块展示，同系列折叠成一张卡牌。
  await sidebar.locator('[data-category="book"]').click()
  await expect(page.locator('.book-shelf')).toBeVisible()
  await expect(page.locator('.shelf-card')).toHaveCount(2)
  const series = page.locator('.series-card')
  await expect(series).toContainText('同系列 · 3 本')
  await expect(page.locator('.series-fan .fan-card')).toHaveCount(2)
  await series.locator('.series-toggle').click()
  await expect(series).toHaveClass(/expanded/)
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
})

test('移动端通过抽屉打开分类栏', async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await mockLibrary(page)
  await login(page)

  const sidebar = page.locator('.app-sidebar')
  await expect(sidebar).toBeHidden()
  await page.getByRole('button', { name: '打开分类栏' }).click()
  await expect(sidebar).toHaveClass(/mobile-open/)
  await expect(sidebar.locator('[data-category="image"]')).toBeVisible()
  await page.waitForTimeout(320)
  await page.screenshot({ path: testInfo.outputPath('mobile-drawer.png') })
  await sidebar.locator('[data-category="image"]').click()
  await expect(page.locator('.library-view')).toBeVisible()
  await expect(sidebar).not.toHaveClass(/mobile-open/)
})
