import { expect, test } from '@playwright/test'
import { login } from './helpers'

test.use({viewport:{width:390,height:844},isMobile:true,hasTouch:true})

test('手机顶栏状态球和工具菜单交互',async({page})=>{
  await login(page)
  const statusTrigger=page.locator('.system-status summary')
  await expect(statusTrigger).toBeVisible()
  await statusTrigger.click()
  await expect(page.locator('.status-panel')).toBeVisible()
  await expect(page.locator('.status-panel').getByText('媒体会话')).toBeVisible()
  await page.keyboard.press('Escape')
  await page.locator('[aria-label="打开任务与工具菜单"]').click()
  const menu=page.locator('.mobile-tool-menu section')
  await expect(menu.getByText('任务中心')).toBeVisible()
  await expect(menu.getByText('系统状态')).toHaveCount(0)
  await expect(menu.getByText('回收站')).toBeVisible()
  await menu.getByRole('button',{name:/任务中心/}).click()
  await expect(page.locator('.task-panel')).toBeVisible()
  await page.keyboard.press('Escape')
})
