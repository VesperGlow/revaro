import { expect, test } from '@playwright/test'
import { login } from './helpers'

test('登录并展开、刷新和关闭桌面系统状态',async({page})=>{
  await login(page)
  await page.getByRole('button',{name:'打开系统状态'}).click()
  const panel=page.locator('.status-panel')
  await expect(panel).toBeVisible()
  await expect(panel.getByText('数据库')).toBeVisible()
  await expect(panel.getByText('S3 / 数据平面')).toBeVisible()
  await panel.getByRole('button',{name:'刷新'}).click()
  await page.keyboard.press('Escape')
  await expect(panel).toBeHidden()
  await page.getByRole('button',{name:'打开系统状态'}).click()
  await page.getByRole('heading',{name:'我的文件'}).click()
  await expect(panel).toBeHidden()
})

test('未登录用户不能读取系统状态',async({request})=>{
  const response=await request.get('/api/system/status')
  expect(response.status()).toBe(401)
})
