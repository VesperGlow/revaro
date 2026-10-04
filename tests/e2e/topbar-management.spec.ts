import { expect, test, type Page } from '@playwright/test'
import { login, openTopbarMenu } from './helpers'
import { readFileSync } from 'node:fs'

async function attached(page: Page, trigger: string, panel: string) {
  await expect(page.locator(panel)).toBeVisible()
  if (panel === '.public-links-popover') {
    const a = (await page.getByLabel(trigger, { exact: true }).boundingBox())!
    const p = (await page.locator(panel).boundingBox())!
    const menu = (await page.locator('.topbar-menu-panel').boundingBox())!
    expect(p.y).toBeGreaterThanOrEqual(a.y + a.height)
    expect(p.x).toBeGreaterThanOrEqual(menu.x)
    expect(p.x + p.width).toBeLessThanOrEqual(menu.x + menu.width)
    return
  }

  await expect.poll(async () => {
    const a = (await page.getByLabel(trigger, { exact: true }).boundingBox())!
    const p = (await page.locator(panel).boundingBox())!
    const centered = a.x + (a.width - p.width) / 2
    const required = Math.max(8, Math.min(centered, page.viewportSize()!.width - 8 - p.width))
    return Math.abs(p.x - required) + Math.abs(p.y - a.y - a.height - 8)
  }).toBeLessThanOrEqual(1)
}

test('links use the menu and storage stays anchored', async ({ page, context }) => {
  await context.grantPermissions(['clipboard-read', 'clipboard-write'])
  let revoked = false
  await page.route('**/api/shares?**', route => route.fulfill({ json: revoked ? [] : [{ file_id: 'link-test', name: '公开文件', active: true, url: 'http://localhost/s/link-test', created_at: '2026-10-03T00:00:00Z' }] }))
  await page.route('**/api/files/link-test/share', route => { revoked = true; return route.fulfill({ status: 204 }) })
  await login(page)
  await openTopbarMenu(page)
  await page.getByLabel('公开链接', { exact: true }).click()
  await attached(page, '公开链接', '.public-links-popover')
  const links = page.locator('.public-links-popover')
  await expect(links).toContainText('有效')
  await links.getByRole('button', { name: '复制链接', exact: true }).click()
  await expect(links.getByRole('button', { name: '已复制', exact: true })).toBeVisible()
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe('http://localhost/s/link-test')
  await links.getByRole('button', { name: '撤销链接', exact: true }).click()
  await expect(links.getByText('暂无公开链接', { exact: true })).toBeVisible()
  await page.getByLabel('系统状态', { exact: true }).click()
  await expect(links).toBeHidden()
  await attached(page, '系统状态', '.system-status-popover')
  const status = page.locator('.system-status-popover')
  await expect(status.locator('.system-metric')).toHaveCount(4)
  await expect(status).toContainText('已用 / 总存储空间')
  await expect(status.getByRole('button', { name: '任务中心', exact: true })).toHaveCount(0)
  await status.getByRole('button', { name: '刷新状态', exact: true }).click()
  await expect(status.getByRole('button', { name: '刷新状态', exact: true })).toBeEnabled()
  await page.evaluate(() => { localStorage.setItem('revaro-reader-manifest:test', 'cached'); localStorage.setItem('keep-ui', 'preserve') })
  await status.getByRole('button', { name: '清理阅读缓存', exact: true }).click()
  expect(await page.evaluate(() => localStorage.getItem('revaro-reader-manifest:test'))).toBeNull()
  expect(await page.evaluate(() => localStorage.getItem('keep-ui'))).toBe('preserve')
  await page.keyboard.press('Escape')
  await expect(status).toBeHidden()
  await expect(page.getByLabel('系统状态', { exact: true })).toBeFocused()
  await page.getByLabel('系统状态', { exact: true }).click()
  await openTopbarMenu(page)
  await page.getByLabel('公开链接', { exact: true }).click()
  await expect(status).toBeHidden()
  await page.locator('.breadcrumbs').click({ position: { x: 1, y: 1 } })
  await expect(links).toBeHidden()
  await page.getByLabel('打开账户设置', { exact: true }).click()
  await expect(page.locator('.account-modal')).toBeVisible()
  await expect(page.locator('.account-modal')).not.toContainText('公开链接')
  await expect(page.locator('.account-modal')).not.toContainText('运行状态')
  await expect(page.locator('.management')).toHaveCount(0)
})

test('status ball and numeric metrics reserve their geometry during loading and expose real storage ratio', async ({ page }) => {
  let release!: () => void
  const gate = new Promise<void>(resolve => { release = resolve })
  await page.route('**/api/system/status', async route => { await gate; await route.fulfill({ json: { disk_total_bytes: 100 * 1024 ** 3, disk_used_bytes: 42 * 1024 ** 3, disk_available_bytes: 58 * 1024 ** 3, cache: { memory_bytes: 2 * 1024 ** 2, disk_bytes: 3 * 1024 ** 2 } } }) })
  await login(page)
  const ball = page.locator('.system-status-ball')
  await expect(ball).toHaveClass(/pending/)
  const before = await ball.boundingBox()
  await page.getByLabel('系统状态', { exact: true }).click()
  const metrics = page.locator('.system-metrics')
  await expect(metrics.locator('.metric-skeleton')).toHaveCount(4)
  const positions = () => metrics.locator('.system-metric').evaluateAll(nodes => nodes.map(node => { const r = node.getBoundingClientRect(); return [r.x, r.y, r.height] }))
  const pending = await positions()
  release()
  await expect(ball).toHaveText('42%')
  expect(await ball.boundingBox()).toEqual(before)
  expect(await positions()).toEqual(pending)
  await expect(page.locator('.storage-ring-progress')).toHaveAttribute('stroke-dasharray', '42 100')
  await expect(metrics).toContainText('42.0 GB / 100.0 GB')
  for (const width of [1600, 1280, 851, 390, 320]) {
    await page.setViewportSize({ width, height: 900 })
    await attached(page, '系统状态', '.system-status-popover')
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
  }
})

test('public link pagination returns to an existing page after its last link is revoked', async ({ page }) => {
  let count = 201
  const links = Array.from({ length: 201 }, (_, index) => ({ file_id: `test-link-${index}`, name: `公开文件 ${index}`, active: true, url: `http://localhost/s/test-link-${index}`, created_at: '2026-10-03T00:00:00Z' }))
  await page.route('**/api/shares?**', route => { const offset = Number(new URL(route.request().url()).searchParams.get('offset')); return route.fulfill({ json: links.slice(0, count).slice(offset, offset + 200) }) })
  await page.route('**/api/files/test-link-200/share', route => { count = 200; return route.fulfill({ status: 204 }) })
  await login(page)
  await openTopbarMenu(page)
  await page.getByLabel('公开链接', { exact: true }).click()
  const panel = page.locator('.public-links-popover')
  await expect(panel.locator('.public-link-row')).toHaveCount(200)
  await panel.getByRole('button', { name: '下一页', exact: true }).click()
  await expect(panel.locator('.public-link-row')).toHaveCount(1)
  await panel.getByRole('button', { name: '撤销链接', exact: true }).click()
  await expect(panel.locator('.public-link-row')).toHaveCount(200)
  await expect(panel.getByRole('button', { name: '下一页', exact: true })).toHaveCount(0)
})

test('video and image libraries use separate queries, shared cards and the existing video player', async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', e => errors.push(e.message))
  await login(page)
  const name = `video-library-${Date.now()}.webm`
  const chooser = page.waitForEvent('filechooser')
  await openTopbarMenu(page)
  await page.getByLabel('上传', { exact: true }).click()
  await page.getByRole('button', { name: '上传文件', exact: true }).click()
  await (await chooser).setFiles({ name, mimeType: 'video/webm', buffer: readFileSync('fixtures/preview.webm') })
  await expect.poll(async () => (await (await page.request.get(`/api/library/items?kind=video&q=${name}`)).json()).total).toBeGreaterThan(0)
  const nav = page.getByRole('navigation', { name: '主导航', exact: true })
  expect(await nav.getByRole('link').evaluateAll(nodes => nodes.map(n => n.getAttribute('aria-label')))).toEqual(['首页', '书籍', '音乐', '图片', '视频', '文件', '回收站'])
  await nav.getByRole('link', { name: '图片', exact: true }).click()
  await expect(page.getByRole('button', { name: `打开 ${name}`, exact: true })).toHaveCount(0)
  await nav.getByRole('link', { name: '视频', exact: true }).click()
  await page.getByLabel('打开搜索', { exact: true }).click()
  await page.getByLabel('搜索视频', { exact: true }).fill(name)
  await page.getByLabel('搜索视频', { exact: true }).press('Enter')
  await expect(page.locator('.library-card')).toHaveCount(1)
  await expect(page.locator('.video-duration')).not.toHaveText('—:—')
  await page.getByRole('button', { name: `打开 ${name}`, exact: true }).click()
  await expect(page.locator('.video-player-shell')).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(page.locator('.video-player-shell')).toBeHidden()
  await nav.getByRole('link', { name: '音乐', exact: true }).click()
  const centers = await page.locator('.library-tabs > button, .library-tabs summary, .library-toolbar-actions > button').evaluateAll(nodes => nodes.map(node => { const r = node.getBoundingClientRect(); return r.y + r.height / 2 }))
  expect(Math.max(...centers) - Math.min(...centers)).toBeLessThan(1)
  await nav.getByRole('link', { name: '回收站', exact: true }).click()
  await expect(nav.locator('[aria-current=page]')).toHaveAttribute('aria-label', '回收站')
  await expect(page.getByRole('heading', { name: '回收站', exact: true })).toHaveCount(0)
  await expect(page.getByRole('button', { name: '返回我的文件', exact: true })).toHaveCount(0)
  await expect(page.locator('.trash-toolbar')).toContainText('30 天')
  await page.reload()
  await expect(nav.locator('[aria-current=page]')).toHaveAttribute('aria-label', '回收站')
  await expect(page.locator('.trash-toolbar')).toBeVisible()
  await nav.getByRole('link', { name: '文件', exact: true }).click()
  await expect(page.getByRole('navigation', { name: '当前路径', exact: true }).getByRole('button', { name: '我的文件', exact: true })).toBeVisible()
  expect(errors).toEqual([])
})


test('file path, totals and sorting align on desktop without a duplicate heading', async ({ page }) => {
  await login(page)
  await expect(page.locator('main h1')).toHaveCount(0)
  for (const width of [1600, 1280, 851, 650, 390, 320]) {
    await page.setViewportSize({ width, height: 900 })
    const geometry = await page.locator('.content-head .breadcrumbs, .content-head .folder-meta, .content-head .file-sort').evaluateAll(nodes => nodes.map(node => {
      const rect = node.getBoundingClientRect()
      return { center: rect.y + rect.height / 2, right: rect.right }
    }))
    expect(geometry).toHaveLength(3)
    if (width > 650) expect(Math.max(...geometry.map(g => g.center)) - Math.min(...geometry.map(g => g.center))).toBeLessThanOrEqual(1)
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
    await page.getByLabel('选择排序字段', { exact: true }).click()
    await expect(page.getByRole('group', { name: '排序字段', exact: true })).toBeVisible()
    await page.getByRole('group', { name: '排序字段', exact: true }).getByRole('button', { name: '大小', exact: true }).click()
    await expect(page.getByLabel('选择排序字段', { exact: true })).toHaveText('大小')
  }
})
