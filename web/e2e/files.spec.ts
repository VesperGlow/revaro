import { expect, test } from '@playwright/test'
import { login, selectCard } from './helpers'

test('创建目录、真实上传、移动、删除、回收站和恢复',async({page})=>{
  await login(page)
  await page.getByRole('button',{name:'新建文件夹'}).click()
  await page.getByRole('dialog').getByPlaceholder('文件夹名称').fill('e2e-target')
  await page.getByRole('dialog').getByRole('button',{name:'创建'}).click()
  await expect(page.locator('.file-card').filter({hasText:'e2e-target'})).toBeVisible()

  await page.locator('input[type=file]').first().setInputFiles({name:'hello-e2e.txt',mimeType:'text/plain',buffer:Buffer.from('real revaro upload\n')})
  const uploaded=page.locator('.file-card').filter({hasText:'hello-e2e.txt'})
  await expect(uploaded).toBeVisible({timeout:20_000})

  await selectCard(page,'hello-e2e.txt')
  await page.getByRole('toolbar',{name:'所选项目操作'}).getByRole('button',{name:'移动'}).click()
  await page.locator('.directory-trigger').click()
  await page.getByRole('region',{name:'选择目标目录'}).getByRole('button',{name:'e2e-target'}).click()
  await page.locator('.directory-trigger').click()
  await page.locator('.move-copy-dialog').getByRole('button',{name:'移动'}).click()
  await expect(uploaded).toBeHidden()

  await page.locator('.file-card').filter({hasText:'e2e-target'}).click()
  await selectCard(page,'hello-e2e.txt')
  await page.getByRole('toolbar',{name:'所选项目操作'}).getByRole('button',{name:'删除'}).click()
  await page.getByRole('dialog').getByRole('button',{name:'移入回收站'}).click()
  await expect(page.locator('.file-card').filter({hasText:'hello-e2e.txt'})).toBeHidden()

  await page.getByRole('button',{name:'回到我的文件'}).click()
  await page.getByRole('button',{name:'打开回收站'}).click()
  await selectCard(page,'hello-e2e.txt')
  await page.getByRole('toolbar',{name:'所选项目操作'}).getByRole('button',{name:'恢复'}).click()
  await expect(page.getByText('回收站是空的')).toBeVisible()
})

test('多选文件通过一次 ZIP 下载且 CSP 保持禁止 frame',async({page})=>{
  let contentSecurityPolicy=''
  page.on('response',response=>{
    if(response.request().resourceType()==='document'&&new URL(response.url()).pathname==='/') contentSecurityPolicy=response.headers()['content-security-policy']||''
  })
  await login(page)
  expect(contentSecurityPolicy).toContain("frame-src 'none'")

  await page.locator('input[type=file]').first().setInputFiles([
    {name:'batch-one.txt',mimeType:'text/plain',buffer:Buffer.from('batch one\n')},
    {name:'batch-two.txt',mimeType:'text/plain',buffer:Buffer.from('batch two\n')},
  ])
  await expect(page.locator('.file-card').filter({hasText:'batch-one.txt'})).toBeVisible({timeout:20_000})
  await expect(page.locator('.file-card').filter({hasText:'batch-two.txt'})).toBeVisible({timeout:20_000})
  await selectCard(page,'batch-one.txt')
  await selectCard(page,'batch-two.txt')
  await expect(page.locator('iframe')).toHaveCount(0)

  const downloadPromise=page.waitForEvent('download')
  await page.getByRole('toolbar',{name:'所选项目操作'}).getByRole('button',{name:'下载 (2)'}).click()
  const download=await downloadPromise
  expect(download.suggestedFilename()).toBe('revaro-download.zip')
  await expect(page.locator('iframe')).toHaveCount(0)
})

test('桌面任务中心可展开和关闭',async({page})=>{
  await login(page)
  await page.getByTitle('任务中心').click()
  await expect(page.locator('.task-panel')).toBeVisible()
  await expect(page.locator('.task-panel').getByRole('button',{name:/新建下载/})).toBeVisible()
  await expect(page.locator('.task-panel')).toHaveCSS('overflow','hidden')
  await page.keyboard.press('Escape')
  await expect(page.locator('.task-panel')).toBeHidden()
})
