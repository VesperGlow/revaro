import { expect, test } from '@playwright/test'
import { login } from './helpers'

test('登录后状态球通过 SSE 展开并关闭系统状态',async({page})=>{
  await login(page)
  await expect(page.locator('.system-status.ok summary')).toBeVisible()
  await page.locator('[aria-label="打开系统状态"]').click()
  const panel=page.locator('.status-panel')
  await expect(panel).toBeVisible()
  await expect(panel.getByText('数据库')).toBeVisible()
  await expect(panel.getByText('S3 / 数据平面')).toBeVisible()
  await expect(panel.getByText('本地磁盘')).toBeVisible()
  await expect(page.locator('.system-status summary i')).toHaveCount(1)
  await expect(panel.locator('i')).toHaveCount(0)
  await expect(panel.getByRole('button',{name:'刷新'})).toHaveCount(0)
  await page.keyboard.press('Escape')
  await expect(panel).toBeHidden()
  await page.locator('[aria-label="打开系统状态"]').click()
  await page.getByRole('heading',{name:'我的文件'}).click()
  await expect(panel).toBeHidden()
})

test('未登录用户不能读取系统状态',async({request})=>{
  const response=await request.get('/api/system/status')
  expect(response.status()).toBe(401)
  const stream=await request.get('/api/system/status/stream')
  expect(stream.status()).toBe(401)
})
