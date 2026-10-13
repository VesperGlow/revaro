import { expect, test, type Page } from '@playwright/test'
import { launchApp, login, uploadFixture } from './helpers'
const root = '00000000-0000-0000-0000-000000000000'
async function homeLogin(page: Page) {
  await page.goto('/')
  await page.getByLabel('用户名').fill(process.env.E2E_USERNAME || 'admin')
  await page.getByLabel('密码').fill(process.env.E2E_PASSWORD || 'revaro-e2e-password')
  await page.getByRole('button', { name: '进入我的内容库' }).click()
  await expect(page.locator('.revaro-os')).toBeVisible()
}
function app(page: Page, path: string) { return page.locator(`.os-app-window[data-app="${path}"]`) }

test('desktop boots to an empty desktop and opens independent draggable resizable tasks', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 960 })
  const errors: string[] = []; page.on('pageerror', e => errors.push(e.message))
  await homeLogin(page)
  await expect(page.getByLabel('桌面主屏幕')).toBeVisible()
  await expect(page.locator('.os-app-window')).toHaveCount(0)
  await expect(page.locator('.os-desktop-apps,.library-navigation-menu')).toHaveCount(0)
  await launchApp(page, '文件')
  const files = app(page, '/files')
  const initial = (await files.boundingBox())!
  expect(initial.width).toBeLessThan(1000)
  const chrome = files.locator('.os-window-chrome')
  const drag = (await chrome.boundingBox())!
  await page.mouse.move(drag.x + drag.width / 2, drag.y + 18)
  await page.mouse.down(); await page.mouse.move(drag.x + drag.width / 2 + 85, drag.y + 70, { steps: 6 }); await page.mouse.up()
  expect((await files.boundingBox())!.x).toBeCloseTo(initial.x + 85, 0)
  const resizedBefore = (await files.boundingBox())!
  const resize = (await files.getByLabel('调整窗口大小').boundingBox())!
  await page.mouse.move(resize.x + 10, resize.y + 10); await page.mouse.down()
  await page.mouse.move(resize.x + 80, resize.y + 55, { steps: 5 }); await page.mouse.up()
  expect((await files.boundingBox())!.width).toBeGreaterThan(resizedBefore.width + 60)
  await launchApp(page, '音乐')
  await expect(files).toBeVisible(); await expect(app(page, '/music')).toBeVisible()
  await expect(page.locator('.os-app-window')).toHaveCount(2)
  await app(page, '/music').getByLabel('最小化窗口').click()
  await expect(app(page, '/music')).toBeHidden(); await expect(files).toHaveClass(/focused/)
  await launchApp(page, '音乐')
  await app(page, '/music').getByLabel('最大化窗口').click()
  expect((await app(page, '/music').boundingBox())!.width).toBe(1416)
  await app(page, '/music').getByLabel('还原窗口').click()
  await page.getByLabel('显示桌面', { exact: true }).first().click()
  await expect(page.locator('.os-app-window:visible')).toHaveCount(0)
  await page.getByLabel('切换任务', { exact: true }).click()
  await page.getByRole('button', { name: '切换到文件', exact: true }).click()
  await expect(files).toBeVisible()
  expect(errors).toEqual([])
})

test('desktop apps keep separate search state and the same mounted workspaces', async ({ page }) => {
  await login(page)
  await app(page, '/files').getByLabel('打开搜索', { exact: true }).click()
  await app(page, '/files').getByLabel('搜索文件名').fill('files-state')
  await page.evaluate(() => { (window as any).__filesWorkspace = document.querySelector('.os-app-window[data-app="/files"] .app-shell') })
  await launchApp(page, '阅读')
  await app(page, '/library').getByLabel('打开搜索', { exact: true }).click()
  await app(page, '/library').getByLabel('搜索书籍').fill('books-state')
  await launchApp(page, '文件')
  await expect(app(page, '/files').getByLabel('搜索文件名')).toHaveValue('files-state')
  expect(await page.evaluate(() => (window as any).__filesWorkspace === document.querySelector('.os-app-window[data-app="/files"] .app-shell'))).toBe(true)
  await launchApp(page, '阅读')
  await expect(app(page, '/library').getByLabel('搜索书籍')).toHaveValue('books-state')
})

test('mobile boots to MobileShell, opens fullscreen apps and switches recent tasks', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  const errors: string[] = []; page.on('pageerror', e => errors.push(e.message))
  await homeLogin(page)
  await expect(page.locator('[data-shell=mobile]')).toBeVisible()
  await expect(page.locator('[data-shell=desktop]')).toHaveCount(0)
  await expect(page.getByLabel('手机主屏幕')).toBeVisible()
  await expect(page.locator('.os-app-window')).toHaveCount(0)
  await launchApp(page, '文件')
  expect((await app(page, '/files').boundingBox())!.width).toBe(390)
  await app(page, '/files').getByLabel('打开搜索', { exact: true }).click()
  await app(page, '/files').getByLabel('搜索文件名').fill('mobile-state')
  await page.getByLabel('返回主屏幕', { exact: true }).click()
  await expect(page.getByLabel('手机主屏幕')).toBeVisible()
  await launchApp(page, '音乐')
  await expect(app(page, '/files')).toBeHidden()
  await page.getByLabel('最近任务', { exact: true }).click()
  await page.getByRole('button', { name: '切换到文件', exact: true }).click()
  await expect(app(page, '/files').getByLabel('搜索文件名')).toHaveValue('mobile-state')
  await page.getByLabel('返回上一级', { exact: true }).click()
  await expect(page.getByLabel('手机主屏幕')).toBeVisible()
  expect(errors).toEqual([])
})

test('mobile folder back navigation preserves original API and returns to the home screen', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 }); await login(page)
  const created = await page.request.post('/api/directories', { headers: { origin: new URL(page.url()).origin }, data: { parent_id: root, name: `mobile-folder-${Date.now()}` } })
  expect(created.ok()).toBe(true)
  const folder = await created.json()
  try {
    await page.reload()
    await app(page, '/files').getByLabel('打开搜索', { exact: true }).click()
    await app(page, '/files').getByLabel('搜索文件名').fill(folder.name)
    await app(page, '/files').getByLabel('搜索文件名').press('Enter')
    await app(page, '/files').locator('.file-card').filter({ hasText: folder.name }).click()
    await expect(page).toHaveURL(new RegExp(`/f/${folder.id}$`))
    await page.getByLabel('返回上一级', { exact: true }).click()
    await expect(page).toHaveURL(/\/files$/)
    await page.getByLabel('返回主屏幕', { exact: true }).click()
    await expect(page).toHaveURL(/\/$/)
    await expect(page.getByLabel('手机主屏幕')).toBeVisible()
  } finally { await page.request.delete(`/api/files/${folder.id}`, { headers: { origin: new URL(page.url()).origin } }); await page.request.delete(`/api/trash/${folder.id}`, { headers: { origin: new URL(page.url()).origin } }) }
})

test('deep links and responsive boundary select the correct shell without remounting apps', async ({ page }) => {
  await login(page); await page.goto('/gallery')
  await expect(app(page, '/gallery')).toBeVisible()
  await page.evaluate(() => { (window as any).__galleryWorkspace = document.querySelector('.os-app-window[data-app="/gallery"] .app-workspace') })
  for (const width of [851, 850, 390, 320, 1280]) {
    await page.setViewportSize({ width, height: 900 })
    await expect(page.locator(`[data-shell=${width <= 850 ? 'mobile' : 'desktop'}]`)).toBeVisible()
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
    expect(await page.evaluate(() => (window as any).__galleryWorkspace === document.querySelector('.os-app-window[data-app="/gallery"] .app-workspace'))).toBe(true)
  }
  await page.goto('/')
  await expect(page.locator('.os-app-window')).toHaveCount(0)
})

test('shared playback survives desktop minimize, mobile home, app and task changes', async ({ page }) => {
  await login(page)
  const data = Buffer.alloc(44 + 8000 * 60 * 2)
  data.write('RIFF'); data.writeUInt32LE(data.length - 8, 4); data.write('WAVEfmt ', 8)
  data.writeUInt32LE(16, 16); data.writeUInt16LE(1, 20); data.writeUInt16LE(1, 22)
  data.writeUInt32LE(8000, 24); data.writeUInt32LE(16000, 28); data.writeUInt16LE(2, 32); data.writeUInt16LE(16, 34); data.write('data', 36); data.writeUInt32LE(data.length - 44, 40)
  const name = `os-audio-${Date.now()}.wav`; const file = await uploadFixture(page, root, name, 'audio/wav', data)
  try {
    await launchApp(page, '音乐')
    await page.getByRole('button', { name: `打开 ${name}`, exact: true }).click()
    await expect.poll(() => page.locator('audio:not([data-revaro-next])').evaluate((audio: HTMLAudioElement) => !audio.paused)).toBe(true)
    const start = await page.locator('audio:not([data-revaro-next])').evaluate((audio: HTMLAudioElement) => { (window as any).__audio = audio; return audio.currentTime })
    await app(page, '/music').getByLabel('最小化窗口').click()
    await page.setViewportSize({ width: 390, height: 844 })
    await page.getByLabel('返回主屏幕', { exact: true }).click()
    await launchApp(page, '阅读')
    await page.getByLabel('最近任务', { exact: true }).click()
    await page.getByRole('button', { name: '切换到音乐', exact: true }).click()
    await expect.poll(() => page.locator('audio:not([data-revaro-next])').evaluate((audio: HTMLAudioElement) => audio.currentTime)).toBeGreaterThan(start)
    expect(await page.locator('audio:not([data-revaro-next])').evaluate((audio: HTMLAudioElement) => audio === (window as any).__audio && !audio.paused)).toBe(true)
    await page.getByLabel('展开播放器', { exact: true }).click()
    await expect(page.getByRole('dialog', { name: '详细音频播放器', exact: true })).toBeVisible()
    await page.getByLabel('返回主屏幕', { exact: true }).click()
    await expect(page.getByLabel('手机主屏幕', { exact: true })).toBeVisible()
    await expect(page.getByRole('dialog', { name: '详细音频播放器', exact: true })).toHaveCount(0)
    expect(await page.locator('audio:not([data-revaro-next])').evaluate((audio: HTMLAudioElement) => !audio.paused)).toBe(true)
  } finally { await page.locator('audio:not([data-revaro-next])').evaluate((audio: HTMLAudioElement) => audio.pause()); await page.request.delete(`/api/files/${file.id}`, { headers: { origin: new URL(page.url()).origin } }); await page.request.delete(`/api/trash/${file.id}`, { headers: { origin: new URL(page.url()).origin } }) }
})

for (const mobile of [false, true]) {
  test(`original upload, reader and gallery work in the ${mobile ? 'mobile' : 'desktop'} application shell`, async ({ page }) => {
    await page.setViewportSize({ width: mobile ? 390 : 1440, height: mobile ? 844 : 960 })
    const errors: string[] = []; page.on('pageerror', e => errors.push(e.message))
    await login(page)
    const suffix = Date.now()
    const book = `os-reader-${suffix}.txt`, image = `os-image-${suffix}.png`
    const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=', 'base64')
    await page.locator('input[type=file]').first().setInputFiles([
      { name: book, mimeType: 'text/plain', buffer: Buffer.from(('第一章\n\n应用外壳继续共享原来的阅读器。\n\n').repeat(100)) },
      { name: image, mimeType: 'image/png', buffer: png },
    ])
    const ids: string[] = []
    try {
      for (const name of [book, image]) {
        await expect.poll(async () => (await (await page.request.get(`/api/library/items?q=${name}`)).json()).total).toBe(1)
        ids.push((await (await page.request.get(`/api/library/items?q=${name}`)).json()).items[0].file.id)
      }
      await launchApp(page, '阅读')
      await page.getByRole('button', { name: `打开 ${book}`, exact: true }).click()
      await expect(page.locator('#reader-view:visible')).toBeVisible()
      await expect(page.locator('#loading:visible')).toHaveCount(0)
      await expect(page.locator('#flow:visible')).toContainText('共享原来的阅读器')
      if (!mobile) {
        const frame = (await app(page, '/library').boundingBox())!
        const reader = (await page.locator('#reader-view:visible').boundingBox())!
        expect(reader.width).toBe(frame.width)
        expect(reader.y).toBe(frame.y + 39)
        const resize = (await app(page, '/library').getByLabel('调整窗口大小').boundingBox())!
        await page.mouse.move(resize.x + 10, resize.y + 10); await page.mouse.down()
        await page.mouse.move(resize.x + 55, resize.y + 30, { steps: 4 }); await page.mouse.up()
        await expect.poll(async () => (await page.locator('#reader-view:visible').boundingBox())!.width).toBeGreaterThan(reader.width + 40)
        await expect(page.locator('#loading:visible')).toHaveCount(0)
      }
      if (mobile) {
        await page.getByLabel('返回主屏幕', { exact: true }).click()
        await expect(page.locator('#reader-view:visible')).toHaveCount(0)
        await page.setViewportSize({ width: 320, height: 844 })
        await page.keyboard.press('Escape')
        await page.getByLabel('最近任务', { exact: true }).click()
        await page.getByRole('button', { name: '切换到阅读', exact: true }).click()
        await expect(page.locator('#reader-view:visible')).toBeVisible()
      }
      await page.locator('#reader-back:visible').click()
      await expect(page).toHaveURL(/\/library$/)
      await launchApp(page, '图库')
      await page.getByRole('button', { name: `打开 ${image}`, exact: true }).click()
      await expect(page.locator('.preview-image:visible')).toBeVisible()
      await page.locator('.preview-close:visible').click()
      expect(errors).toEqual([])
    } finally {
      for (const id of ids) {
        await page.request.delete(`/api/files/${id}`, { headers: { origin: new URL(page.url()).origin } })
        await page.request.delete(`/api/trash/${id}`, { headers: { origin: new URL(page.url()).origin } })
      }
    }
  })
}

test('mobile Back keeps unsaved editor confirmation after returning from the task switcher', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 }); await login(page)
  const name = `os-draft-${Date.now()}.md`
  const created = await page.request.post('/api/documents', { headers: { origin: new URL(page.url()).origin }, data: { parent_id: root, name, content: '# Original' } })
  expect(created.ok()).toBe(true)
  const file = await created.json()
  try {
    await page.reload()
    await app(page, '/files').getByLabel('打开搜索', { exact: true }).click()
    await app(page, '/files').getByLabel('搜索文件名').fill(name)
    await app(page, '/files').getByLabel('搜索文件名').press('Enter')
    await app(page, '/files').locator('.file-card').filter({ hasText: name }).click()
    await page.getByLabel('文档内容', { exact: true }).fill('# Unsaved draft')
    await page.getByLabel('返回主屏幕', { exact: true }).click()
    await expect(page.locator('.document-editor:visible')).toHaveCount(0)
    await page.getByLabel('最近任务', { exact: true }).click()
    await page.getByRole('button', { name: '切换到文件', exact: true }).click()
    await expect(page.getByLabel('文档内容', { exact: true })).toHaveValue('# Unsaved draft')
    await page.getByLabel('返回上一级', { exact: true }).click()
    await expect(page.locator('.app-dialog:visible')).toContainText('放弃未保存的修改')
    await page.getByLabel('返回上一级', { exact: true }).click()
    await expect(page.locator('.app-dialog:visible')).toHaveCount(0)
    await expect(page.getByLabel('文档内容', { exact: true })).toHaveValue('# Unsaved draft')
    await page.getByLabel('返回上一级', { exact: true }).click()
    await page.getByRole('button', { name: '放弃修改', exact: true }).click()
    await expect(page.locator('.document-editor:visible')).toHaveCount(0)
    expect((await (await page.request.get(`/api/files/${file.id}/content`)).json()).content).toBe('# Original')
  } finally {
    await page.request.delete(`/api/files/${file.id}`, { headers: { origin: new URL(page.url()).origin } })
    await page.request.delete(`/api/trash/${file.id}`, { headers: { origin: new URL(page.url()).origin } })
  }
})
