import { expect, type Page } from '@playwright/test'

export async function enterSelectionMode(page: Page) {
  await openTopbarMenu(page)
  const toggle = page.locator('.selection-toggle')
  await expect(toggle).toBeVisible()
  if (await toggle.getAttribute('aria-pressed') === 'false') await toggle.click()
  else await page.getByLabel('关闭菜单', { exact: true }).click()
  await expect(toggle).toHaveAttribute('aria-pressed', 'true')
}

export async function login(page: Page) {
  await page.goto('/')
  await page.getByLabel('用户名').fill(process.env.E2E_USERNAME || 'admin')
  await page.getByLabel('密码').fill(process.env.E2E_PASSWORD || 'revaro-e2e-password')
  await page.getByRole('button', { name: '进入我的内容库' }).click()
  if ((page.viewportSize()?.width || 1280) <= 850) {
    await page.getByRole('navigation', { name: '移动端导航', exact: true }).getByRole('link', { name: '文件', exact: true }).click()
  } else {
    await page.getByRole('navigation', { name: '主导航', exact: true }).getByRole('link', { name: '文件', exact: true }).click()
  }
  await expect(page.getByRole('navigation', { name: '当前路径', exact: true }).getByRole('button', { name: '我的文件', exact: true })).toBeVisible()
}

export async function openTopbarMenu(page: Page) {
  const menu = page.locator('.topbar-menu')
  if (!await menu.evaluate(element => (element as HTMLDetailsElement).open)) {
    await page.getByLabel('更多操作', { exact: true }).click()
  }
  await expect(page.locator('.topbar-menu-panel')).toBeVisible()
}
