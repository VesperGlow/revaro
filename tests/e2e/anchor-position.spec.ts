import { expect, test, type Locator, type Page } from '@playwright/test'
import { login } from './helpers'

async function attached(page: Page, trigger: Locator, panel: Locator) {
  await expect(panel).toBeVisible()
  await expect.poll(async () => {
    const anchor = (await trigger.boundingBox())!
    const popup = (await panel.boundingBox())!
    const width = page.viewportSize()!.width
    const centered = anchor.x + (anchor.width - popup.width) / 2
    const required = Math.max(8, Math.min(centered, width - 8 - popup.width))
    return Math.abs(popup.x - required) + Math.abs(popup.y - anchor.y - anchor.height - 8)
  }).toBeLessThanOrEqual(1)
  const popup = (await panel.boundingBox())!
  expect(popup.x).toBeGreaterThanOrEqual(8)
  expect(popup.x + popup.width).toBeLessThanOrEqual(page.viewportSize()!.width - 8)
}

test('open popovers remain centered or minimally clamped through resize, scroll and short viewports', async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await page.setViewportSize({ width: 390, height: 900 })
  await login(page)
  const upload = page.locator('.topbar').getByLabel('上传', { exact: true })
  const panel = page.locator('.topbar .action-menu[open] .action-menu-panel')
  await upload.click()
  for (const width of [390, 1280, 851, 320, 1600]) {
    await page.setViewportSize({ width, height: 900 })
    await attached(page, upload, panel)
    await expect(panel).toHaveCSS('border-radius', '10px')
    await page.evaluate(() => window.scrollTo(0, 100))
    await attached(page, upload, panel)
  }
  await page.keyboard.press('Escape')
  await expect(upload).toBeFocused()
  const create = page.locator('.topbar').getByLabel('新建', { exact: true })
  await create.click()
  for (const width of [1600, 1280, 320, 850]) {
    await page.setViewportSize({ width, height: 900 })
    await attached(page, create, panel)
  }
  await page.keyboard.press('Escape')
  await page.setViewportSize({ width: 1280, height: 150 })
  await page.getByLabel('打开任务中心', { exact: true }).click()
  const tasks = page.locator('.task-panel')
  await attached(page, page.getByLabel('打开任务中心', { exact: true }), tasks)
  const bounds = (await tasks.boundingBox())!
  expect(bounds.y + bounds.height).toBeLessThanOrEqual(142)
  await page.waitForTimeout(150)
  await page.keyboard.press('Escape')
  await expect(tasks).toBeHidden()
  expect(errors).toEqual([])
})
