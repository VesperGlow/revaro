import { expect, test } from '@playwright/test'
import { login } from './helpers'

for (const width of [1280, 390, 320]) {
  test(`runtime status reserves its layout during loading, failure and recovery at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 })
    let release!: () => void
    let gate = new Promise<void>(resolve => { release = resolve })
    let failed = false
    await page.route('**/api/shares?**', route => route.fulfill({ json: [] }))
    await page.route('**/api/system/status', async route => {
      await gate
      if (failed) await route.fulfill({ status: 500, json: { message: 'unavailable' } })
      else await route.fulfill({ json: { disk_available_bytes: 4 * 1024 ** 3, cache: { memory_bytes: 2 * 1024 ** 2, disk_bytes: 3 * 1024 ** 2 }, active_tasks: 12 } })
    })
    await login(page)
    if (width <= 850) {
      await page.getByLabel('打开账户与工具菜单', { exact: true }).click()
      await page.getByRole('button', { name: '账户设置', exact: true }).click()
    } else await page.getByLabel('打开账户设置', { exact: true }).click()
    const management = page.locator('.management')
    const status = management.getByRole('status')
    const positions = () => page.evaluate(() => ['.management-status', '.management-actions', '.management > h3:last-of-type'].map(selector => {
      const rect = document.querySelector(selector)!.getBoundingClientRect()
      return { y: rect.y, height: rect.height }
    }))
    await expect(status).toHaveAttribute('aria-busy', 'true')
    await expect(status.locator('.status-skeleton')).toBeVisible()
    await expect(status).toHaveText('')
    const initial = await positions()
    release()
    await expect(status).toHaveAttribute('aria-busy', 'false')
    await expect(status.locator('.status-metrics > span')).toHaveText(['磁盘可用空间4.0 GiB', '内存缓存2.0 MiB', '磁盘缓存3.0 MiB', '活动任务12'])
    expect(await positions()).toEqual(initial)
    await expect(management.getByRole('button', { name: '刷新状态', exact: true })).toBeEnabled()
    failed = true
    gate = new Promise<void>(resolve => { release = resolve })
    await management.getByRole('button', { name: '刷新状态', exact: true }).click()
    await expect(status.locator('.status-skeleton')).toBeVisible()
    expect(await positions()).toEqual(initial)
    release()
    await expect(status).toHaveText('状态获取失败')
    expect(await positions()).toEqual(initial)
    await expect(management.getByRole('button', { name: '刷新状态', exact: true })).toBeEnabled()
    failed = false
    gate = Promise.resolve()
    await management.getByRole('button', { name: '刷新状态', exact: true }).click()
    await expect(status).toContainText('活动任务12')
    expect(await positions()).toEqual(initial)
  })
}

test('compact dashboards and shared SVG action menus preserve file operations and keyboard focus', async ({ page }) => {
  const failures: string[] = []
  page.on('pageerror', error => failures.push(error.message))
  await login(page)
  for (const width of [1280, 851, 850, 390, 320]) {
    await page.setViewportSize({ width, height: 900 })
    const nav = page.getByRole('navigation', { name: width <= 850 ? '移动端导航' : '主导航', exact: true })
    for (const label of ['首页', '阅读', '音乐', '图库']) {
      await nav.getByRole('link', { name: label, exact: true }).click()
      await expect(nav.locator('[aria-current=page]')).toHaveAttribute('aria-label', label)
      await expect(page.locator('.library-welcome, .library-eyebrow, .welcome-art, .library-heading, main h1')).toHaveCount(0)
      const content = await page.locator('main.library-main').boundingBox()
      const header = await page.locator('header.topbar').boundingBox()
      expect(content!.y).toBe(header!.height)
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
    }
    await nav.getByRole('link', { name: '文件', exact: true }).click()
    const create = page.locator('.topbar').getByLabel('新建', { exact: true })
    const upload = page.locator('.topbar').getByLabel('上传', { exact: true })
    for (const trigger of [create, upload]) {
      await expect(trigger).toHaveText('')
      await expect(trigger.locator('svg')).toHaveCount(1)
      const bounds = await trigger.boundingBox()
      expect(bounds!.height).toBe(44)
      expect(bounds!.width).toBe(44)
    }
    await expect(create.locator('svg circle')).toHaveAttribute('r', '9')
    await create.click()
    await expect(page.getByRole('button', { name: '新建文档', exact: true })).toBeVisible()
    const panel = page.locator('.topbar .action-menu[open] .action-menu-panel')
    const triggerBounds = await create.boundingBox()
    const panelBounds = await panel.boundingBox()
    expect(panelBounds!.y).toBeGreaterThan(triggerBounds!.y + triggerBounds!.height)
    expect(panelBounds!.x).toBeGreaterThanOrEqual(0)
    expect(panelBounds!.x + panelBounds!.width).toBeLessThanOrEqual(width)
    await upload.click()
    await expect(page.locator('.topbar .action-menu[open]')).toHaveCount(1)
    await expect(page.getByRole('button', { name: '新建文档', exact: true })).toBeHidden()
    await expect(page.getByRole('button', { name: '上传文件夹', exact: true })).toBeVisible()
    await page.keyboard.press('Tab')
    await expect(page.getByRole('button', { name: '上传文件', exact: true })).toBeFocused()
    await page.keyboard.press('Escape')
    await expect(upload).toBeFocused()
    await expect(page.locator('.topbar .action-menu[open]')).toHaveCount(0)
    await expect(upload).toHaveCSS('outline-style', 'none')
    await expect(upload).not.toHaveCSS('box-shadow', 'none')
    await create.press('Enter')
    await expect(page.getByRole('button', { name: '新建文档', exact: true })).toBeVisible()
    await page.getByRole('heading', { name: '我的文件', exact: true }).click()
    await expect(page.locator('.topbar .action-menu[open]')).toHaveCount(0)
  }
  await page.setViewportSize({ width: 1280, height: 900 })
  await page.locator('.topbar').getByLabel('新建', { exact: true }).click()
  const backgroundName = `background-menu-${Date.now()}.txt`
  await page.locator('input[type=file]').first().setInputFiles({ name: backgroundName, mimeType: 'text/plain', buffer: Buffer.from('后台上传完成时保持菜单打开') })
  await expect(page.locator('.file-card').filter({ hasText: backgroundName })).toBeVisible()
  await expect(page.getByRole('button', { name: '新建文档', exact: true })).toBeVisible()
  await page.keyboard.press('Escape')
  await page.locator('.topbar').getByLabel('上传', { exact: true }).click()
  const chooser = page.waitForEvent('filechooser')
  await page.getByRole('button', { name: '上传文件夹', exact: true }).click()
  expect(await (await chooser).element().getAttribute('webkitdirectory')).toBe('')
  await page.locator('.topbar').getByLabel('新建', { exact: true }).click()
  await page.getByRole('button', { name: '新建文件夹', exact: true }).click()
  const name = `compact-folder-${Date.now()}`
  const dialog = page.getByRole('dialog')
  await dialog.locator('input[type=text]').fill(name)
  await dialog.getByRole('button', { name: '创建', exact: true }).click()
  await expect(page.locator('.file-card').filter({ hasText: name })).toBeVisible()
  await page.locator('.file-card').filter({ hasText: name }).click()
  await expect(page.getByRole('heading', { name, exact: true })).toBeVisible()
  await page.getByRole('navigation', { name: '主导航', exact: true }).getByRole('link', { name: '首页', exact: true }).click()
  await page.locator('.topbar').getByLabel('新建', { exact: true }).click()
  await page.getByRole('button', { name: '新建文档', exact: true }).click()
  const editor = page.locator('.document-editor')
  const documentName = `compact-global-${Date.now()}.md`
  await editor.getByRole('textbox', { name: '文档文件名' }).fill(documentName)
  await editor.getByRole('button', { name: '保存', exact: true }).click()
  await expect(editor.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
  const listing = await (await page.request.get(`/api/files?q=${documentName}`)).json()
  expect(listing.items[0].parent_id).toBe('00000000-0000-0000-0000-000000000000')
  expect(failures).toEqual([])
})
