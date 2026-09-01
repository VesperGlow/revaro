import { expect, test } from '@playwright/test'
import { login } from './helpers'

test.use({viewport:{width:390,height:844},isMobile:true,hasTouch:true})

test('手机汉堡菜单包含任务中心、系统状态和回收站',async({page})=>{
  await login(page)
  await page.locator('[aria-label="打开任务与工具菜单"]').click()
  const menu=page.locator('.mobile-tool-menu section')
  await expect(menu.getByText('任务中心')).toBeVisible()
  await expect(menu.getByText('系统状态')).toBeVisible()
  await expect(menu.getByText('回收站')).toBeVisible()
  await menu.getByRole('button',{name:/系统状态/}).click()
  await expect(page.locator('.status-panel')).toBeVisible()
  await expect(page.locator('.status-panel').getByText('媒体会话')).toBeVisible()
  await page.keyboard.press('Escape')
  await page.locator('[aria-label="打开任务与工具菜单"]').click()
  await menu.getByRole('button',{name:/任务中心/}).click()
  await expect(page.locator('.task-panel')).toBeVisible()
  await page.keyboard.press('Escape')
})
