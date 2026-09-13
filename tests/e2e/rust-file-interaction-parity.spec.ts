import { expect, test } from '@playwright/test'
import { login } from './helpers'

const ROOT = '00000000-0000-0000-0000-000000000000'

test('移动端列表在选择模式下轻触行只切换选择，不打开文件', async ({ browser }) => {
  const context = await browser.newContext({
    viewport: { width: 390, height: 844 },
    isMobile: true,
    hasTouch: true,
  })
  const page = await context.newPage()
  const name = `compat-touch-${crypto.randomUUID()}.txt`
  let id = ''

  try {
    await login(page)
    id = await page.evaluate(async ({ name, root }) => {
      const response = await fetch('/api/documents', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ parent_id: root, name, content: 'touch parity' }),
      })
      if (!response.ok) throw new Error(`创建文档失败：${response.status}`)
      const file = await response.json() as { id: string }
      return file.id
    }, { name, root: ROOT })

    await page.reload()
    await page.getByRole('button', { name: '列表' }).click()
    const row = page.locator('.file-row').filter({ hasText: name })
    await expect(row).toBeVisible()
    await row.getByRole('button', { name: '选择项目' }).click()
    await expect(page.locator('.selection-toolbar')).toContainText('1 项')

    await row.locator('.row-info').click()
    await expect(page.locator('.selection-toolbar')).toHaveCount(0)
    await expect(page.locator('.modal-backdrop.editing')).toHaveCount(0)

    await row.getByRole('button', { name: '选择项目' }).click()
    await expect(page.locator('.selection-toolbar')).toContainText('1 项')
  } finally {
    if (id) {
      await page.evaluate(async ({ id }) => {
        await fetch(`/api/files/${id}`, { method: 'DELETE' })
        await fetch(`/api/trash/${id}`, { method: 'DELETE' })
      }, { id })
    }
    await context.close()
  }
})

test('可编辑 TXT 在文件卡上使用旧版文档图标，而不是阅读器图标', async ({ page }) => {
  const name = `compat-icon-${crypto.randomUUID()}.txt`
  let id = ''

  try {
    await login(page)
    id = await page.evaluate(async ({ name, root }) => {
      const response = await fetch('/api/documents', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ parent_id: root, name, content: 'icon parity' }),
      })
      if (!response.ok) throw new Error(`创建文档失败：${response.status}`)
      const file = await response.json() as { id: string }
      return file.id
    }, { name, root: ROOT })

    await page.reload()
    const card = page.locator('.file-card').filter({ hasText: name })
    await expect(card).toBeVisible()
    await expect(card.locator('.document-type-icon')).toHaveCount(1)
    await expect(card.locator('.book-type-icon')).toHaveCount(0)
  } finally {
    if (id) {
      await page.evaluate(async ({ id }) => {
        await fetch(`/api/files/${id}`, { method: 'DELETE' })
        await fetch(`/api/trash/${id}`, { method: 'DELETE' })
      }, { id })
    }
  }
})
