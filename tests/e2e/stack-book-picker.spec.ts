import { expect, test } from '@playwright/test'
import { login, failApplicationRequest } from './helpers'
import { zip } from './fixtures/epub'

const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=', 'base64')
const coveredBook = zip([
  ['META-INF/container.xml', Buffer.from('<container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>')],
  ['content.opf', Buffer.from('<package><metadata><title>封面测试</title></metadata><manifest><item id="cover" href="cover.png" media-type="image/png" properties="cover-image"/><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>')],
  ['cover.png', png],
  ['chapter.xhtml', Buffer.from('<html><body><p>书籍选择器中的真实封面测试。</p></body></html>')],
])

for (const touch of [false, true]) {
  test.describe(touch ? 'touch book picker' : 'desktop book picker', () => {
    test.use({ hasTouch: touch, viewport: touch ? { width: 390, height: 844 } : { width: 1280, height: 800 } })

    test('whole rows select across searches, keep actions visible and move only Stack membership', async ({ page }) => {
      test.setTimeout(90_000)
      const errors: string[] = []
      page.on('pageerror', error => errors.push(error.message))
      const prefix = `picker-${Date.now()}-${touch}`
      await login(page)
      const headers = { origin: new URL(page.url()).origin }
      const ownNames = [1, 2].map(n => `${prefix}-current-${n}.txt`)
      const otherNames = [1, 2].map(n => `${prefix}-source-${n}.epub`)
      const freeNames = Array.from({ length: 62 }, (_, n) => `${prefix}-free-${n.toString().padStart(2, '0')}.txt`)
      await page.locator('input[type=file]').first().setInputFiles([
        ...ownNames.map(name => ({ name, mimeType: 'text/plain', buffer: Buffer.from('当前堆叠中的书籍。') })),
        ...otherNames.map(name => ({ name, mimeType: 'application/epub+zip', buffer: coveredBook })),
        ...freeNames.map(name => ({ name, mimeType: 'text/plain', buffer: Buffer.from('选择器中可添加的独立书籍。') })),
      ])
      const listingUrl = `/api/library/items?kind=book&q=${prefix}&limit=200`
      await expect.poll(async () => (await (await page.request.get(listingUrl)).json()).total).toBe(66)
      const before = (await (await page.request.get(listingUrl)).json()).items
      const id = (name: string) => before.find((item: any) => item.file.name === name).file.id
      const currentResponse = await page.request.post('/api/library/stacks', { headers, data: { name: `Current ${prefix}`, file_ids: ownNames.map(id) } })
      expect(currentResponse.ok()).toBeTruthy()
      const current = await currentResponse.json()
      const sourceResponse = await page.request.post('/api/library/stacks', { headers, data: { name: `Source ${prefix}`, file_ids: otherNames.map(id) } })
      expect(sourceResponse.ok()).toBeTruthy()
      const source = await sourceResponse.json()
      const shelf = await (await page.request.post('/api/library/collections', { headers, data: { kind: 'book', name: `Shelf ${prefix}` } })).json()
      expect((await page.request.put(`/api/library/collections/${shelf.id}/items/${id(otherNames[0])}`, { headers })).ok()).toBeTruthy()
      await page.goto(`/library/stacks/${current.id}`)
      await expect(page.locator('.stack-header h1')).toHaveText(current.name)
      await page.getByRole('button', { name: '添加书籍', exact: true }).click()
      const picker = page.getByRole('dialog', { name: '添加书籍', exact: true })
      const search = picker.getByLabel('搜索待添加书籍')
      const footer = picker.locator('.stack-picker-footer')
      const add = footer.getByRole('button', { name: '添加', exact: true })
      await expect(picker.getByText('书籍只改变堆叠关系，不影响原文件和分类', { exact: true })).toBeVisible()
      await expect(add).toBeDisabled()
      await search.fill(prefix)
      await expect(picker.getByRole('checkbox')).toHaveCount(64)
      await expect(picker.locator('input[type=checkbox]')).toHaveCount(0)
      for (const name of ownNames) await expect(picker.getByRole('checkbox', { name: `添加 ${name}`, exact: true })).toHaveCount(0)
      expect(await picker.locator('.stack-picker-row').evaluateAll(rows => rows.every(row => row.tagName === 'BUTTON' && row.getBoundingClientRect().height < 110))).toBeTruthy()
      if (touch) {
        const bounds = (await picker.boundingBox())!
        expect(bounds.y).toBeLessThan(40)
        expect(bounds.height).toBeGreaterThan(844 * 0.9)
        expect(Math.abs(bounds.y + bounds.height - 844)).toBeLessThan(2)
      } else await expect(search).toBeFocused()
      const fromOther = picker.getByRole('checkbox', { name: `添加 ${otherNames[0]}`, exact: true })
      await search.fill(otherNames[0])
      await expect(fromOther).toContainText(`所属堆叠：${source.name}`)
      await expect.poll(() => fromOther.locator('img').evaluate((img: HTMLImageElement) => img.complete && img.naturalWidth > 0)).toBeTruthy()
      await fromOther.locator('.stack-picker-copy').click()
      await expect(fromOther).toHaveAttribute('aria-checked', 'true')
      await expect(fromOther).toHaveClass(/selected/)
      await expect(fromOther).toContainText('将移入当前堆叠')
      await expect(footer).toContainText('已选 1 本')
      await search.fill(freeNames[61])
      const standalone = picker.getByRole('checkbox', { name: `添加 ${freeNames[61]}`, exact: true })
      if (touch) await standalone.click()
      else await standalone.press('Space')
      await expect(footer).toContainText('已选 2 本')
      await search.fill('does-not-match-any-book')
      await expect(picker.getByText('没有找到相关书籍', { exact: true })).toBeVisible()
      await expect(footer).toContainText('已选 2 本')
      await expect(add).toBeEnabled()
      await search.fill(prefix)
      const searchBounds = (await search.boundingBox())!
      const footerBounds = (await footer.boundingBox())!
      const scroll = picker.locator('.stack-picker-scroll')
      if (touch) {
        const cdp = await page.context().newCDPSession(page)
        const area = (await scroll.boundingBox())!
        const start = { x: area.x + area.width / 2, y: area.y + area.height - 40 }
        await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [start] })
        for (let n = 1; n <= 6; n++) {
          await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: start.x, y: start.y - n * 35 }] })
          await page.waitForTimeout(20)
        }
        await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
        await cdp.detach()
        await expect.poll(() => scroll.evaluate(el => el.scrollTop)).toBeGreaterThan(40)
      } else await scroll.evaluate(el => { el.scrollTop = el.scrollHeight })
      expect(Math.abs((await footer.boundingBox())!.y - footerBounds.y)).toBeLessThan(1)
      expect(Math.abs((await search.boundingBox())!.y - searchBounds.y)).toBeLessThan(1)
      if (touch) {
        await page.setViewportSize({ width: 390, height: 500 })
        await expect.poll(async () => { const box = (await footer.boundingBox())!; return Math.abs(box.y + box.height - 500) }).toBeLessThan(2)
        await expect(footer).toContainText('已选 2 本')
        await page.setViewportSize({ width: 390, height: 844 })
        await expect.poll(async () => { const box = (await footer.boundingBox())!; return Math.abs(box.y + box.height - 844) }).toBeLessThan(2)
      }
      expect(await page.evaluate(() => document.body.style.overflow)).toBe('hidden')
      const fault = await failApplicationRequest(page, `/api/library/stacks/${current.id}/items`, '添加暂时失败，请重试')
      await add.click()
      await expect(picker.getByRole('alert')).toContainText('添加暂时失败，请重试')
      await expect(footer).toContainText('已选 2 本')
      await fault.clear()
      await add.click()
      await expect(picker).toHaveCount(0)
      await expect(page.locator('.library-card')).toHaveCount(4)
      const stacks = await (await page.request.get('/api/library/stacks')).json()
      expect(stacks.find((stack: any) => stack.id === current.id).files.map((file: any) => file.id)).toEqual(expect.arrayContaining([id(otherNames[0]), id(freeNames[61]), ...ownNames.map(id)]))
      expect(stacks.find((stack: any) => stack.id === source.id).files.map((file: any) => file.id)).toEqual([id(otherNames[1])])
      const after = (await (await page.request.get(listingUrl)).json()).items
      expect(after.map((item: any) => [item.file.id, item.kind, item.file.name, item.file.size, item.file.parent_id, item.file.content_hash]).sort()).toEqual(before.map((item: any) => [item.file.id, item.kind, item.file.name, item.file.size, item.file.parent_id, item.file.content_hash]).sort())
      const onShelf = await (await page.request.get(`/api/library/items?kind=book&collection=${shelf.id}`)).json()
      expect(onShelf.items.map((item: any) => item.file.id)).toEqual([id(otherNames[0])])
      await page.getByRole('button', { name: '添加书籍', exact: true }).click()
      await expect(search).toHaveValue('')
      await expect(footer).toContainText('已选 0 本')
      await expect(add).toBeDisabled()
      await picker.getByRole('button', { name: '关闭书籍选择器', exact: true }).press('Escape')
      await expect(picker).toHaveCount(0)
      expect(await page.evaluate(() => document.body.style.overflow)).toBe('')
      await expect(page.getByRole('button', { name: '添加书籍', exact: true })).toBeFocused()
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBeTruthy()
      expect(errors).toEqual([])
    })
  })
}
