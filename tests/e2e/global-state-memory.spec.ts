import { expect, test } from '@playwright/test'
import { login, navigate } from './helpers'
const root = '00000000-0000-0000-0000-000000000000'

for (const viewport of [{ width: 1280, height: 720 }, { width: 390, height: 844 }]) {
  test(`independent shelves, directories, sort and scroll survive navigation and reload at ${viewport.width}px`, async ({ page }) => {
    test.setTimeout(90_000)
    await page.setViewportSize(viewport)
    await login(page)
    const headers = { origin: new URL(page.url()).origin }
    const name = `state-${viewport.width}-${Date.now()}`
    const folderResponse = await page.request.post('/api/directories', { headers, data: { parent_id: root, name } })
    expect(folderResponse.status()).toBe(201)
    const folder = await folderResponse.json()
    let shelf: any
    try {
      const files: any[] = []
      for (let i = 0; i < 140; i += 4) {
        const batch = await Promise.all(Array.from({ length: 4 }, async (_, j) => {
          const response = await page.request.post('/api/documents', { headers, data: { parent_id: folder.id, name: `${name}-${String(i + j).padStart(3, '0')}.txt`, content: '第一章\n页面记忆与缓存验证。\n'.repeat(5) } })
          expect(response.status()).toBe(201)
          return response.json()
        }))
        files.push(...batch)
      }
      const created = await page.request.post('/api/library/collections', { headers, data: { name: `轻小说-${name}`, kind: 'book', file_ids: files.map(f => f.id) } })
      expect(created.status()).toBe(200)
      shelf = await created.json()
      expect(shelf.item_count).toBe(140)
      await navigate(page, '书籍')
      await page.getByLabel('选择集合', { exact: true }).click()
      await page.getByRole('button', { name: `${shelf.name} · 140`, exact: true }).click()
      await page.getByLabel('内容排序', { exact: true }).click()
      await page.getByRole('button', { name: '名称降序', exact: true }).click()
      await expect(page.locator('.library-card')).toHaveCount(60)
      const shelfScroll = await page.evaluate(() => Math.min(1900, document.documentElement.scrollHeight - innerHeight - 80))
      expect(shelfScroll).toBeGreaterThan(400)
      await page.evaluate(y => window.scrollTo(0, y), shelfScroll)
      await expect.poll(() => page.evaluate(y => Math.abs(window.scrollY - y), shelfScroll)).toBeLessThan(10)
      await page.waitForTimeout(250)
      await navigate(page, '音乐', true)
      await expect(page.locator('.library-loading')).toHaveCount(0)
      await navigate(page, '书籍', true)
      await expect(page.locator('.collection-caption')).toBeVisible()
      await expect.poll(() => page.evaluate(y => Math.abs(window.scrollY - y), shelfScroll)).toBeLessThan(10)
      await page.reload()
      await expect(page.locator('.collection-caption')).toBeVisible()
      await expect.poll(() => page.evaluate(y => Math.abs(window.scrollY - y), shelfScroll)).toBeLessThan(10)
      await page.getByLabel('内容排序', { exact: true }).click()
      await expect(page.getByRole('button', { name: '名称降序', exact: true })).toHaveAttribute('aria-pressed', 'true')
      await page.keyboard.press('Escape')
      await page.goto(`/f/${folder.id}`)
      await expect(page.getByRole('navigation', { name: '当前路径', exact: true })).toContainText(name)
      await page.getByRole('button', { name: '选择排序字段', exact: true }).click()
      await page.getByRole('group', { name: '排序字段', exact: true }).getByRole('button', { name: '时间', exact: true }).click()
      const fileScroll = await page.evaluate(() => Math.min(2800, document.documentElement.scrollHeight - innerHeight - 80))
      expect(fileScroll).toBeGreaterThan(400)
      await page.evaluate(y => window.scrollTo(0, y), fileScroll)
      await expect.poll(() => page.evaluate(y => Math.abs(window.scrollY - y), fileScroll)).toBeLessThan(10)
      await page.waitForTimeout(250)
      await navigate(page, '图片', true)
      await navigate(page, '文件', true)
      await expect(page).toHaveURL(new RegExp(`/f/${folder.id}$`))
      await expect.poll(() => page.evaluate(y => Math.abs(window.scrollY - y), fileScroll)).toBeLessThan(10)
      await page.reload()
      await expect(page.getByRole('navigation', { name: '当前路径', exact: true })).toContainText(name)
      await expect.poll(() => page.evaluate(y => Math.abs(window.scrollY - y), fileScroll)).toBeLessThan(10)
      await page.getByRole('button', { name: '选择排序字段', exact: true }).click()
      await expect(page.getByRole('group', { name: '排序字段', exact: true }).getByRole('button', { name: '时间', exact: true })).toHaveAttribute('aria-pressed', 'true')
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    } finally {
      if (shelf) await page.request.delete(`/api/library/collections/${shelf.id}`, { headers })
      await page.request.delete(`/api/files/${folder.id}`, { headers })
    }
  })
}

test('batch shelf mutation publishes fresh cached listings and keeps invalid batches atomic', async ({ page }) => {
  await login(page)
  const headers = { origin: new URL(page.url()).origin }
  const name = `atomic-shelf-${Date.now()}`
  const response = await page.request.post('/api/documents', { headers, data: { parent_id: root, name: `${name}.txt`, content: '批量书架。' } })
  const file = await response.json()
  const created = await page.request.post('/api/library/collections', { headers, data: { name, kind: 'book', file_ids: [file.id] } })
  expect(created.status()).toBe(200)
  const shelf = await created.json()
  const path = `/api/library/items?kind=book&collection=${shelf.id}`
  try {
    expect((await (await page.request.get(path)).json()).total).toBe(1)
    expect((await page.request.get(path)).headers()['x-revaro-cache']).toBe('hit')
    const invalid = await page.request.put(`/api/library/collections/${shelf.id}/items`, { headers, data: { file_ids: [file.id, 'missing'] } })
    expect(invalid.status()).toBe(400)
    expect((await (await page.request.get(path)).json()).total).toBe(1)
    expect((await page.request.delete(`/api/library/collections/${shelf.id}/items`, { headers, data: { file_ids: [file.id] } })).status()).toBe(204)
    expect((await (await page.request.get(path)).json()).total).toBe(0)
  } finally {
    await page.request.delete(`/api/library/collections/${shelf.id}`, { headers })
    await page.request.delete(`/api/files/${file.id}`, { headers })
  }
})

test('same-folder refresh updates renamed cards and preserves unchanged DOM nodes', async ({ page }) => {
  await login(page)
  const headers = { origin: new URL(page.url()).origin }
  const name = `refresh-${Date.now()}`
  const folder = await (await page.request.post('/api/directories', { headers, data: { parent_id: root, name } })).json()
  const files = []
  for (const label of ['change', 'keep']) files.push(await (await page.request.post('/api/documents', { headers, data: { parent_id: folder.id, name: `${label}-${name}.txt`, content: '同目录刷新。' } })).json())
  try {
    await page.goto(`/f/${folder.id}`)
    const keep = page.locator('.file-grid article').filter({ hasText: files[1].name })
    await expect(keep).toBeVisible()
    await keep.evaluate(element => { (window as any).__keptFileNode = element })
    const renamed = `renamed-${name}.txt`
    expect((await page.request.patch(`/api/files/${files[0].id}`, { headers, data: { name: renamed } })).status()).toBe(200)
    await navigate(page, '文件')
    await expect(page.locator('.file-grid article').filter({ hasText: renamed })).toBeVisible()
    expect(await keep.evaluate(element => element === (window as any).__keptFileNode)).toBe(true)
  } finally {
    await page.request.delete(`/api/files/${folder.id}`, { headers })
  }
})
