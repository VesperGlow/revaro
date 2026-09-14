import { expect, test } from '@playwright/test'
import { login } from './helpers'

test('文件列表/方块视图偏好在重新进入后保持', async ({ page }) => {
  const name = `view-preference-${crypto.randomUUID()}.txt`
  await login(page)
  try {
    await page.locator('input[type=file]').first().setInputFiles({
      name,
      mimeType: 'text/plain',
      buffer: Buffer.from('view preference parity\n'),
    })
    await page.locator('.file-card').filter({ hasText: name }).waitFor({ timeout: 20_000 })
    await page.getByRole('button', { name: '列表' }).click()
    await expect(page.locator('.file-rows')).toBeVisible()
    await page.reload()
    await expect(page.getByRole('heading', { name: '我的文件' })).toBeVisible()
    await expect(page.locator('.file-rows')).toBeVisible()
    await expect(page.locator('.file-grid')).toHaveCount(0)
  } finally {
    await page.evaluate(async wanted => {
      const headers = { 'Content-Type': 'application/json' }
      const children = await fetch('/api/files/00000000-0000-0000-0000-000000000000/children')
      if (children.ok) {
        const data = await children.json() as { items?: Array<{ id: string; name: string }> }
        const item = (data.items ?? []).find(entry => entry.name === wanted)
        if (item) await fetch(`/api/files/${item.id}`, { method: 'DELETE', headers })
      }
      const trash = await fetch('/api/trash')
      if (trash.ok) {
        const data = await trash.json() as { items?: Array<{ id: string; name: string }> }
        const item = (data.items ?? []).find(entry => entry.name === wanted)
        if (item) await fetch(`/api/trash/${item.id}`, { method: 'DELETE', headers })
      }
    }, name)
  }
})
