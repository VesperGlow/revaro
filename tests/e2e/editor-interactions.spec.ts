import { expect, test, type Locator, type Page } from '@playwright/test'
import { failApplicationRequest, holdApplicationResponse, login } from './helpers'

const root = '00000000-0000-0000-0000-000000000000'
const origin = (process.env.E2E_BASE_URL || 'http://127.0.0.1:18080').replace(/\/$/, '')
const headers = { Origin: origin }
const original = '# 原始版本\n\n' + '这是一行用于检查滚动位置的历史内容。\n\n'.repeat(450)
const current = '# 当前文档\n\n[预览链接](https://example.com)\n\n' + '这是一行用于检查当前文档滚动与选区的内容。\n\n'.repeat(450)

async function position(text: Locator) {
  return text.evaluate((text: HTMLTextAreaElement) => ({ top: text.scrollTop, left: text.scrollLeft, start: text.selectionStart, end: text.selectionEnd, direction: text.selectionDirection }))
}
async function settle(page: Page) {
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))))
}
async function textPointer(page: Page, text: Locator) {
  const bounds = (await text.boundingBox())!
  const point = { x: bounds.x + Math.min(40, bounds.width / 2), y: bounds.y + Math.min(70, bounds.height / 2) }
  await page.mouse.move(point.x, point.y)
  await expect(text).toHaveCSS('cursor', 'text')
  const style = await text.evaluate((element, point) => ({ hit: element.contains(document.elementFromPoint(point.x, point.y)), caret: getComputedStyle(element).caretColor, color: getComputedStyle(element).color, pointerEvents: getComputedStyle(element).pointerEvents, tag: element.tagName }), point)
  expect(style.hit).toBe(true)
  if (style.tag === 'TEXTAREA') {
    expect(style.caret).toBe(style.color)
    expect(style.caret).not.toBe('rgba(0, 0, 0, 0)')
  }
  expect(style.pointerEvents).toBe('auto')
}

for (const mobile of [false, true]) for (const colorScheme of ['light', 'dark'] as const) {
  test.describe(`editor ${mobile ? 'mobile' : 'desktop'} ${colorScheme}`, () => {
    test.use({ viewport: mobile ? { width: 390, height: 844 } : { width: 1280, height: 900 }, isMobile: mobile, hasTouch: mobile, colorScheme })
    let file: { id: string, etag: string, name: string }
    let errors: string[]
    test.beforeEach(async ({ page }) => {
      errors = []; page.on('pageerror', error => errors.push(error.message))
      await login(page)
      const created = await page.request.post('/api/documents', { headers, data: { parent_id: root, name: `editor-interaction-${Date.now()}-${mobile}-${colorScheme}.md`, content: original } })
      expect(created.ok()).toBeTruthy(); file = await created.json()
      const updated = await page.request.put(`/api/files/${file.id}/content`, { headers, data: { etag: file.etag, content: current } })
      expect(updated.ok()).toBeTruthy(); file = await updated.json()
      await page.goto(`/f/${root}`)
      await page.getByLabel('打开搜索', { exact: true }).click()
      await page.getByLabel('搜索文件名').fill(file.name)
      await page.getByLabel('搜索文件名').press('Enter')
      await page.locator('.file-card').click()
      await expect(page.getByLabel('文档内容', { exact: true })).toHaveValue(current)
    })
    test.afterEach(async ({ page }) => {
      await page.request.delete(`/api/files/${file.id}`, { headers })
      await page.request.delete(`/api/trash/${file.id}`, { headers })
      expect(errors).toEqual([])
    })

    test('sidebar and in-place history preserve the same draft node, selection, focus and scroll without extra overlays', async ({ page }) => {
      const editor = page.locator('.document-editor'), text = editor.getByLabel('文档内容', { exact: true })
      const node = await text.elementHandle()
      const draft = current + '\n未保存的修改'
      await text.fill(draft)
      await text.evaluate((text: HTMLTextAreaElement) => { text.setSelectionRange(40, 70, 'backward'); text.scrollTop = 1200 })
      await settle(page)
      const saved = await position(text)
      expect(saved.top).toBeGreaterThan(500)
      await editor.getByRole('button', { name: '版本历史', exact: true }).click()
      const history = editor.getByRole('complementary', { name: '版本历史', exact: true })
      await expect(history.locator('.history-row')).toHaveCount(1)
      const historyBounds = (await history.boundingBox())!
      const bodyHeight = (await editor.locator('.editor-body').boundingBox())!.height
      const navigationHeight = mobile ? (await editor.locator('.editor-mobile-views').boundingBox())!.height : 0
      expect(historyBounds.height).toBeCloseTo(bodyHeight - navigationHeight, 0)
      await expect(page.locator('.ui-dialog-backdrop')).toHaveCount(1)
      await expect(page.getByRole('dialog')).toHaveCount(1)
      if (mobile) {
        await expect(text).toBeHidden()
        expect((await history.boundingBox())!.width).toBeGreaterThan(350)
      } else {
        const width = (await history.boundingBox())!.width
        expect(width).toBeGreaterThanOrEqual(320); expect(width).toBeLessThanOrEqual(360)
        await expect(text).toBeEditable()
        await expect(editor.locator('.editor-body')).not.toHaveAttribute('inert', '')
      }
      await history.getByRole('button', { name: '查看', exact: true }).click()
      const past = editor.getByLabel('历史版本内容', { exact: true })
      await expect(past).toHaveValue(original)
      await expect(past).not.toBeEditable()
      await expect(past).toBeFocused()
      await expect(text).toBeHidden()
      await textPointer(page, past)
      await expect(editor.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
      await past.press('Control+s')
      expect((await (await page.request.get(`/api/files/${file.id}/content`)).json()).content).toBe(current)
      await editor.getByRole('button', { name: '返回当前文档', exact: true }).click()
      await settle(page)
      await expect(text).toHaveValue(draft)
      await expect(text).toBeFocused()
      expect(await position(text)).toEqual(saved)
      expect(await node!.evaluate(element => element.isConnected)).toBe(true)
      expect(await text.evaluate(element => element === document.querySelector('.editor-current textarea'))).toBe(true)
      if (mobile) await editor.getByRole('button', { name: '历史记录', exact: true }).click()
      await history.getByLabel('关闭版本历史').click()
      await settle(page)
      await expect(history).toBeHidden()
      await expect(text).toHaveValue(draft)
      expect(await position(text)).toEqual(saved)
      await expect(editor.locator('.unsaved-dot')).toHaveText('未保存')
      await textPointer(page, text)
      await text.press('Control+s')
      await expect(editor.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
      expect((await (await page.request.get(`/api/files/${file.id}/content`)).json()).content).toBe(draft)
      // An open sidebar follows ordinary saves while the current document stays editable.
      await editor.getByRole('button', { name: '版本历史', exact: true }).click()
      await expect(history.locator('.history-row')).toHaveCount(2)
      if (mobile) await editor.getByRole('button', { name: '当前文档', exact: true }).click()
      await text.fill(draft + '\n侧栏打开时保存')
      await text.press('Control+s')
      if (mobile) await editor.getByRole('button', { name: '历史记录', exact: true }).click()
      await expect(history.locator('.history-row')).toHaveCount(3)
    })

    test('all modes keep correct pointer hit targets and late history responses and restore conflicts cannot replace current content', async ({ page }) => {
      const editor = page.locator('.document-editor'), text = editor.getByLabel('文档内容', { exact: true })
      // A cursor hidden by an unrelated ancestor must never leak into the editor.
      await page.evaluate(() => { document.body.style.cursor = 'none' })
      for (const label of ['编辑', '分栏', '预览']) {
        await editor.getByRole('button', { name: label, exact: true }).click()
        await expect(editor.getByRole('button', { name: label, exact: true })).toHaveAttribute('aria-pressed', 'true')
        if (label === '编辑' || (label === '分栏' && !mobile)) await textPointer(page, text)
        if (label !== '编辑') {
          const markdown = editor.getByLabel('Markdown 预览', { exact: true })
          await textPointer(page, markdown)
          await expect(markdown.getByRole('link', { name: '预览链接' })).toHaveCSS('cursor', 'pointer')
        }
        await expect(editor.locator('.editor-header')).toHaveCSS('cursor', 'default')
        await expect(editor.getByRole('button', { name: '关闭编辑器' })).toHaveCSS('cursor', 'pointer')
      }
      await page.evaluate(() => { document.body.style.removeProperty('cursor') })
      await editor.getByRole('button', { name: '编辑', exact: true }).click()
      const trigger = editor.getByRole('button', { name: '版本历史', exact: true })
      const history = editor.getByRole('complementary', { name: '版本历史', exact: true })
      const failed = await failApplicationRequest(page, `/api/files/${file.id}/versions`, '模拟历史读取失败')
      await trigger.click()
      await expect(history.getByRole('alert')).toContainText('模拟历史读取失败')
      await failed.clear()
      await history.getByRole('button', { name: '重试', exact: true }).click()
      await expect(history.locator('.history-row')).toHaveCount(1)
      const held = await holdApplicationResponse(page, `^/api/files/${file.id}/versions/[^/]+/content$`, 'GET')
      try {
        await history.getByRole('button', { name: '查看', exact: true }).click()
        await expect.poll(held.captured).toBe(true)
        await history.getByLabel('关闭版本历史').click()
        await held.release()
        await settle(page)
        await expect(history).toBeHidden()
        await expect(editor.getByLabel('历史版本内容')).toBeHidden()
        await expect(text).toHaveValue(current)
      } finally { await held.release() }
      await trigger.click()
      await expect(history.locator('.history-row')).toHaveCount(1)
      const external = await page.request.put(`/api/files/${file.id}/content`, { headers, data: { etag: file.etag, content: '另一设备的新版本' } })
      expect(external.ok()).toBeTruthy()
      await history.getByRole('button', { name: '恢复此版本', exact: true }).click()
      const confirm = page.getByRole('dialog', { name: '恢复历史版本？', exact: true })
      await expect(editor.locator('.editor-body')).toHaveAttribute('inert', '')
      await confirm.getByRole('button', { name: '确认恢复', exact: true }).click()
      await expect(confirm.getByRole('alert')).toContainText(/changed|冲突|更改/)
      expect((await (await page.request.get(`/api/files/${file.id}/content`)).json()).content).toBe('另一设备的新版本')
      await confirm.getByRole('button', { name: '取消', exact: true }).click()
      await expect(confirm).toBeHidden()
      await expect(editor.locator('.editor-body')).not.toHaveAttribute('inert', '')
      if (mobile) await editor.getByRole('button', { name: '当前文档', exact: true }).click()
      await expect(text).toHaveValue(current)
      await textPointer(page, text)
    })
  })
}
