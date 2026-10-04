import { expect, test, type Page } from '@playwright/test'
import { login, openTopbarMenu } from './helpers'

async function attached(page: Page) {
  const panel = page.locator('.topbar-menu-panel')
  await expect(panel).toBeVisible()
  const anchor = (await page.getByLabel('更多操作', { exact: true }).boundingBox())!
  const popup = (await panel.boundingBox())!
  const { width, height } = page.viewportSize()!
  expect(popup.x).toBeGreaterThanOrEqual(0)
  expect(popup.x + popup.width).toBeLessThanOrEqual(width)
  expect(popup.y + popup.height).toBeLessThanOrEqual(height - (width > 850 ? 8 : 0))
  if (width > 850) {
    expect(popup.x + popup.width).toBe(anchor.x + anchor.width)
    expect(popup.y).toBe(anchor.y + anchor.height + 8)
  } else {
    expect(popup.x).toBe(0)
    expect(popup.width).toBe(width)
    expect(popup.y + popup.height).toBe(height)
  }
}

test('actions menu stays attached through resize, scroll and short viewports', async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await page.setViewportSize({ width: 390, height: 900 })
  await login(page)
  await openTopbarMenu(page)
  await page.getByLabel('上传', { exact: true }).click()
  for (const width of [390, 1280, 851, 320, 1600]) {
    await page.setViewportSize({ width, height: 900 })
    await attached(page)
    await expect(page.getByRole('button', { name: '上传文件夹', exact: true })).toBeVisible()
    await page.evaluate(() => window.scrollTo(0, 100))
    await attached(page)
  }
  await page.keyboard.press('Escape')
  await expect(page.getByLabel('上传', { exact: true })).toBeFocused()
  await openTopbarMenu(page)
  await page.getByLabel('新建', { exact: true }).click()
  for (const width of [1600, 1280, 320, 850]) {
    await page.setViewportSize({ width, height: 900 })
    await attached(page)
  }
  await page.keyboard.press('Escape')
  await page.keyboard.press('Escape')
  expect(errors).toEqual([])
})
