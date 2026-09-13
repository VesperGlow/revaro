import { expect, test } from '@playwright/test'
import { login } from './helpers'

test('文件列表/方块视图偏好在重新进入后保持', async ({ page }) => {
  await login(page)
  await page.getByRole('button', { name: '列表' }).click()
  await expect(page.locator('.file-rows')).toBeVisible()
  await page.reload()
  await expect(page.getByRole('heading', { name: '我的文件' })).toBeVisible()
  await expect(page.locator('.file-rows')).toBeVisible()
  await expect(page.locator('.file-grid')).toHaveCount(0)
})
