import { expect, test } from '@playwright/test'
import { login, enterSelectionMode, openTopbarMenu } from './helpers'

const origin = (process.env.E2E_BASE_URL || 'http://localhost:18083').replace(/\/$/, '')
const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=', 'base64')

function wav() {
  const samples = 8000 * 90
  const data = Buffer.alloc(44 + samples * 2)
  data.write('RIFF', 0); data.writeUInt32LE(36 + samples * 2, 4); data.write('WAVEfmt ', 8)
  data.writeUInt32LE(16, 16); data.writeUInt16LE(1, 20); data.writeUInt16LE(1, 22)
  data.writeUInt32LE(8000, 24); data.writeUInt32LE(16000, 28); data.writeUInt16LE(2, 32)
  data.writeUInt16LE(16, 34); data.write('data', 36); data.writeUInt32LE(samples * 2, 40)
  return data
}

test('content libraries share real uploads, keep music alive during reading and restore deep links', async ({ page }) => {
  const failures: string[] = []
  page.on('pageerror', error => failures.push(error.message))
  const suffix = Date.now().toString(36)
  const book = `library-book-${suffix}.txt`
  const song = `library-song-${suffix}.wav`
  const photo = `library-photo-${suffix}.png`
  await login(page)
  await page.locator('input[type=file]').first().setInputFiles([
    { name: book, mimeType: 'text/plain', buffer: Buffer.from(('第一章 初见\n\n这是一段可以在阅读器里打开的文字，我们一边听音乐，一边翻开下一页。\n\n').repeat(70)) },
    { name: song, mimeType: 'audio/wav', buffer: wav() },
    { name: photo, mimeType: 'image/png', buffer: png },
  ])
  for (const name of [book, song, photo]) {
    await expect.poll(async () => (await (await page.request.get(`/api/library/items?q=${name}`)).json()).total).toBe(1)
  }
  const nav = page.getByRole('navigation', { name: '主导航', exact: true })
  await nav.getByRole('link', { name: '音乐', exact: true }).click()
  await page.getByRole('button', { name: `打开 ${song}`, exact: true }).click()
  await expect(page.getByRole('button', { name: '暂停音乐', exact: true })).toBeVisible()
  await page.evaluate(() => { (window as any).__audio = document.querySelector('audio') })
  await nav.getByRole('link', { name: '书籍', exact: true }).click()
  await page.getByRole('button', { name: `打开 ${book}`, exact: true }).click()
  await expect(page.locator('#reader-view')).toBeVisible()
  await expect(page.locator('#loading')).toBeHidden()
  await expect(page.locator('#flow')).toContainText('一边听音乐')
  expect(await page.evaluate(() => (window as any).__audio === document.querySelector('audio') && !(document.querySelector('audio') as HTMLAudioElement).paused)).toBeTruthy()
  for (const viewport of [{ width: 1280, height: 720 }, { width: 390, height: 844 }]) {
    await page.setViewportSize(viewport)
    const dock = await page.locator('.music-dock').boundingBox()
    const footer = await page.locator('.reader-footer').boundingBox()
    expect(dock!.y + dock!.height).toBeLessThan(footer!.y)
    await expect(page.getByRole('navigation', { name: '移动端导航' })).toBeHidden()
    await page.locator('#font-button').click()
    await expect(page.locator('.font-popover')).toBeVisible()
    await page.locator('#font-button').click()
  }
  await page.setViewportSize({ width: 1280, height: 720 })
  const deepLink = page.url()
  await page.locator('#reader-back').click()
  await expect(page.getByRole('navigation', { name: '主导航', exact: true }).locator('[aria-current=page]')).toHaveAttribute('aria-label', '书籍')
  await expect(page.getByRole('button', { name: '暂停音乐', exact: true })).toBeVisible()
  await nav.getByRole('link', { name: '图片', exact: true }).click()
  await page.getByRole('button', { name: `打开 ${photo}`, exact: true }).click()
  await expect(page.locator('.preview-image')).toBeVisible()
  await page.locator('.preview-close').click()
  await page.getByRole('button', { name: '停止音乐' }).click()
  await page.goto(deepLink)
  await expect(page.locator('#reader-view')).toBeVisible()
  await expect(page.locator('#loading')).toBeHidden()
  await page.locator('#reader-back').click()
  await expect(page.getByRole('navigation', { name: '主导航', exact: true }).locator('[aria-current=page]')).toHaveAttribute('aria-label', '书籍')
  expect(failures).toEqual([])
})

test('favorites and albums persist, and removing membership preserves the original image', async ({ page }) => {
  const suffix = Date.now().toString(36)
  const photo = `album-photo-${suffix}.png`
  const name = `album-${suffix}`
  await login(page)
  await page.locator('input[type=file]').first().setInputFiles({ name: photo, mimeType: 'image/png', buffer: png })
  await page.locator('.file-card').filter({ hasText: photo }).waitFor()
  await page.getByRole('navigation', { name: '主导航', exact: true }).getByRole('link', { name: '图片', exact: true }).click()
  const image = page.locator('.library-card').filter({ has: page.getByRole('button', { name: `打开 ${photo}`, exact: true }) })
  const toolbar = page.getByRole('toolbar', { name: '所选项目操作', exact: true })
  await enterSelectionMode(page)
  await image.getByRole('checkbox').press('Space')
  await toolbar.getByRole('button', { name: '收藏', exact: true }).click()
  await expect.poll(async () => (await (await page.request.get(`/api/library/items?q=${photo}`)).json()).items[0].favorite).toBe(true)
  await openTopbarMenu(page)
  await page.getByRole('button', { name: '退出选择模式', exact: true }).click()
  await page.getByRole('button', { name: '新建相册', exact: true }).click()
  await page.getByLabel('集合名称').fill(name)
  await page.getByRole('dialog', { name: '管理集合' }).getByRole('button', { name: '创建', exact: true }).click()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await page.getByRole('button', { name: /^全部/ }).click()
  await enterSelectionMode(page)
  await image.getByRole('checkbox').press('Space')
  await toolbar.getByLabel('更多管理操作', { exact: true }).click()
  await page.locator('.selection-management-panel').getByRole('button', { name: '加入相册', exact: true }).click()
  await page.getByRole('dialog').getByRole('button', { name: new RegExp(name) }).click()
  await page.getByLabel('选择集合', { exact: true }).click()
  await page.locator('.collection-menu .action-menu-panel').getByRole('button', { name: `${name} · 1`, exact: true }).click()
  await expect(page.locator('.library-card')).toHaveCount(1)
  await page.reload()
  await page.getByRole('button', { name: '我的收藏', exact: true }).click()
  await expect(page.getByRole('button', { name: `打开 ${photo}`, exact: true })).toBeVisible()
  await page.getByLabel('选择集合', { exact: true }).click()
  await page.locator('.collection-menu .action-menu-panel').getByRole('button', { name: `${name} · 1`, exact: true }).click()
  await enterSelectionMode(page)
  await image.getByRole('checkbox').press('Space')
  await toolbar.getByLabel('更多管理操作', { exact: true }).click()
  await page.locator('.selection-management-panel').getByRole('button', { name: '移出当前集合', exact: true }).click()
  await expect(page.locator('.library-card')).toHaveCount(0)
  const files = await (await page.request.get(`/api/library/items?q=${photo}`)).json()
  expect(files.total).toBe(1)
  expect(files.items[0].favorite).toBe(true)
})

test('gallery viewer prefetches beyond the first page and mobile layout stays within the viewport', async ({ page }) => {
  await login(page)
  const suffix = Date.now().toString(36)
  for (let start = 0; start < 65; start += 8) {
    await Promise.all(Array.from({ length: Math.min(8, 65 - start) }, async (_, i) => {
      const created = await page.request.post('/api/uploads', { headers: { Origin: origin }, data: { parent_id: '00000000-0000-0000-0000-000000000000', name: `gallery-batch-${suffix}-${start + i}.png`, size: png.length, mime_type: 'image/png' } })
      expect(created.ok()).toBeTruthy()
      const session = await created.json()
      expect((await page.request.put(session.url, { headers: { Origin: origin, 'Content-Type': 'image/png' }, data: png })).ok()).toBeTruthy()
      expect((await page.request.post(`/api/uploads/${session.upload_id}/complete`, { headers: { Origin: origin }, data: {} })).ok()).toBeTruthy()
    }))
  }
  await page.getByRole('navigation', { name: '主导航', exact: true }).getByRole('link', { name: '图片', exact: true }).click()
  await expect(page.locator('.library-card')).toHaveCount(60)
  let failedBatches = 0
  await page.route('**/api/library/items?**', async route => {
    if (new URL(route.request().url()).searchParams.get('offset') === '60') {
      failedBatches++
      await route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: 'temporary outage' }) })
    } else {
      await route.continue()
    }
  })
  await page.locator('.library-card-open').nth(59).click()
  await expect.poll(() => failedBatches).toBe(1)
  await page.waitForTimeout(500)
  expect(failedBatches).toBe(1)
  await page.locator('.preview-close').click()
  await page.unroute('**/api/library/items?**')
  await page.getByRole('alert').getByRole('button', { name: '重试', exact: true }).click()
  await expect(page.locator('.library-card')).toHaveCount(60)
  await page.locator('.library-card-open').nth(59).click()
  await page.getByRole('button', { name: '缩略图', exact: true }).click()
  await expect.poll(() => page.locator('.preview-filmstrip button').count()).toBeGreaterThan(60)
  await page.locator('.preview-close').click()
  await page.setViewportSize({ width: 320, height: 740 })
  await expect(page.getByRole('navigation', { name: '移动端导航' })).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBeTruthy()
  await page.getByRole('navigation', { name: '移动端导航' }).getByRole('link', { name: '首页', exact: true }).click()
  await expect(page.getByRole('heading', { name: '继续阅读', exact: true })).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBeTruthy()
})

test('home import and gallery transfer use the existing file tools', async ({ page }) => {
  await login(page)
  const suffix = Date.now().toString(36)
  const target = `gallery-transfer-${suffix}`
  const photo = `home-import-${suffix}.png`
  await openTopbarMenu(page)
  await page.locator('.topbar').getByLabel('新建', { exact: true }).click()
  await page.getByRole('button', { name: '新建文件夹', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.locator('input[type=text]').fill(target)
  await dialog.getByRole('button', { name: '创建', exact: true }).click()
  await page.locator('.file-card').filter({ hasText: target }).waitFor()
  const nav = page.getByRole('navigation', { name: '主导航', exact: true })
  await nav.getByRole('link', { name: '首页', exact: true }).click()
  const chooserPromise = page.waitForEvent('filechooser')
  await openTopbarMenu(page)
  await page.locator('.topbar').getByLabel('上传', { exact: true }).click()
  await page.getByRole('button', { name: '上传文件', exact: true }).click()
  await (await chooserPromise).setFiles({ name: photo, mimeType: 'image/png', buffer: png })
  await expect.poll(async () => (await (await page.request.get(`/api/library/items?q=${photo}`)).json()).total).toBe(1)
  await nav.getByRole('link', { name: '图片', exact: true }).click()
  await page.getByRole('button', { name: `打开 ${photo}`, exact: true }).click()
  await page.locator('.preview-commandbar summary').click()
  const downloadPromise = page.waitForEvent('download')
  await page.getByRole('button', { name: '下载', exact: true }).click()
  expect((await downloadPromise).suggestedFilename()).toBe(photo)
  await page.locator('.preview-commandbar summary').click()
  await page.getByRole('button', { name: '移动', exact: true }).click()
  await expect(page.locator('.move-copy-dialog')).toBeVisible()
  await page.locator('.directory-trigger').click()
  await page.getByRole('region', { name: '选择目标目录' }).getByRole('button', { name: target, exact: true }).click()
  await expect(page.locator('.directory-trigger')).toContainText(target)
  await page.locator('.directory-trigger').click()
  await page.getByRole('dialog').getByRole('button', { name: '移动', exact: true }).click()
  await expect(page.locator('.move-copy-dialog')).toHaveCount(0)
  await expect(nav.locator('[aria-current=page]')).toHaveAttribute('aria-label', '图片')
  const listing = await (await page.request.get(`/api/library/items?q=${photo}`)).json()
  expect(listing.total).toBe(1)
  const folder = await (await page.request.get(`/api/files?q=${target}`)).json()
  expect(listing.items[0].file.parent_id).toBe(folder.items.find((f: any) => f.name === target).id)
})
