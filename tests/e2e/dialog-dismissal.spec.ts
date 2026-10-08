import { expect, test, type Locator, type Page } from '@playwright/test'
import { enterSelectionMode, login, navigate, openTopbarMenu } from './helpers'

const origin = (process.env.E2E_BASE_URL || 'http://127.0.0.1:18080').replace(/\/$/, '')
const root = '00000000-0000-0000-0000-000000000000'
const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=', 'base64')

for (const mobile of [false, true]) {
  test.describe(mobile ? 'touch dialogs' : 'desktop dialogs', () => {
    test.use({ viewport: mobile ? { width: 390, height: 844 } : { width: 1280, height: 900 }, isMobile: mobile, hasTouch: mobile })
    let created: string[] = []
    let trashed: Set<string> = new Set()
    let errors: string[] = []

    async function dismiss(backdrop: Locator) {
      const position = { x: 2, y: 2 }
      if (mobile) await backdrop.tap({ position })
      else await backdrop.click({ position })
    }

    test.beforeEach(async ({ page }) => {
      created = []
      trashed = new Set()
      errors = []
      page.on('pageerror', error => errors.push(error.message))
      await login(page)
    })
    test.afterEach(async ({ page }) => {
      for (const id of created) {
        if (!trashed.has(id)) {
          const removed = await page.request.delete(`/api/files/${id}`, { headers: { Origin: origin } })
          expect(removed.ok()).toBeTruthy()
        }
        const purged = await page.request.delete(`/api/trash/${id}`, { headers: { Origin: origin } })
        expect(purged.ok()).toBeTruthy()
      }
      expect(errors).toEqual([])
    })

    async function document(page: Page, extension = 'md') {
      const name = `dismissal-${Date.now()}-${mobile}.${extension}`
      const response = await page.request.post('/api/documents', {
        headers: { Origin: origin },
        data: { name, parent_id: root, content: extension === 'txt' ? ('第一章\n点击空白关闭排版设置，阅读内容保留。\n\n').repeat(100) : 'original' },
      })
      expect(response.ok(), await response.text()).toBeTruthy()
      const file = await response.json()
      created.push(file.id)
      await page.reload()
      await page.getByLabel('打开搜索', { exact: true }).click()
      await page.getByLabel('搜索文件名').fill(name)
      await page.getByLabel('搜索文件名').press('Enter')
      await expect(page.locator('.file-card')).toHaveCount(1)
      await page.getByLabel('搜索文件名').press('Escape')
      return file
    }

    test('tap highlights are transparent and keyboard focus remains visible', async ({ page }) => {
      const account = page.getByLabel('打开账户设置', { exact: true })
      await expect(account).toHaveCSS('-webkit-tap-highlight-color', 'rgba(0, 0, 0, 0)')
      await expect(page.getByLabel('打开搜索', { exact: true }).locator('svg')).toHaveCSS('-webkit-tap-highlight-color', 'rgba(0, 0, 0, 0)')
      await expect(page.getByRole('navigation', { name: mobile ? '移动端导航' : '主导航', exact: true }).getByRole('link', { name: '文件', exact: true })).toHaveCSS('-webkit-tap-highlight-color', 'rgba(0, 0, 0, 0)')
      await openTopbarMenu(page)
      await expect(page.getByLabel('新建', { exact: true })).toHaveCSS('-webkit-tap-highlight-color', 'rgba(0, 0, 0, 0)')
      await page.keyboard.press('Escape')
      await page.keyboard.press('Tab')
      await account.focus()
      await expect(account).toBeFocused()
      await expect(account).not.toHaveCSS('box-shadow', 'none')
    })

    test('popover menus dismiss outside and on Escape, preserve internal clicks and exclude other menus', async ({ page }) => {
      const sort = page.getByRole('button', { name: '选择排序字段', exact: true })
      const fields = page.getByRole('group', { name: '排序字段', exact: true })
      await sort.click()
      await expect(fields).toBeVisible()
      // Touch hit testing can promote a padding tap to a nearby row action.
      // Target the noninteractive surface explicitly to test internal dismissal.
      if (mobile) await fields.dispatchEvent('click')
      else await fields.click({ position: { x: 8, y: 3 } })
      await expect(fields).toBeVisible()
      // No pointer or focus change: native disclosure opening must still close sorting.
      const menu = page.locator('.topbar .topbar-menu')
      await menu.evaluate((details: HTMLDetailsElement) => { details.open = true })
      await expect(menu).toHaveAttribute('open', '')
      await expect(fields).toBeHidden()
      await expect(page.locator('.action-menu[open]:not(.embedded-menu)')).toHaveCount(1)
      await page.keyboard.press('Escape')
      await expect(menu).not.toHaveAttribute('open', '')
      await expect(menu.locator(':scope > summary')).toBeFocused()

      await sort.click()
      await fields.getByRole('button', { name: '大小', exact: true }).click()
      await expect(fields).toBeHidden()
      await expect(sort).toHaveText('大小')
      await sort.click()
      await page.locator('.folder-meta').click()
      await expect(fields).toBeHidden()

      await openTopbarMenu(page)
      await menu.locator('.topbar-menu-heading h2').click()
      await expect(menu).toHaveAttribute('open', '')
      await page.getByLabel('新建', { exact: true }).click()
      await page.getByRole('button', { name: '新建文件夹', exact: true }).click()
      await expect(menu).not.toHaveAttribute('open', '')
      await expect(page.locator('.app-dialog')).toBeVisible()
      await page.keyboard.press('Escape')
      await expect(page.locator('.app-dialog')).toBeHidden()
    })

    test('creation and every collection dialog close only on their backdrop', async ({ page }) => {
      await openTopbarMenu(page)
      await page.getByLabel('新建', { exact: true }).click()
      await page.getByRole('button', { name: '新建文件夹', exact: true }).click()
      const action = page.locator('.app-dialog')
      await action.locator('input').fill('keep until dismissed')
      await action.locator('h2').click()
      await expect(action).toBeVisible()
      await dismiss(page.locator('.dialog-backdrop'))
      await expect(action).toBeHidden()

      for (const [pageName, label] of [['书籍', '书架'], ['音乐', '歌单'], ['图片', '相册'], ['视频', '视频集']]) {
        await navigate(page, pageName)
        await page.getByRole('button', { name: `新建${label}`, exact: true }).click()
        const collection = page.getByRole('dialog', { name: '管理集合', exact: true })
        await collection.getByLabel('集合名称').fill('dismiss without creating')
        await collection.locator('h2').click()
        await expect(collection).toBeVisible()
        await dismiss(page.locator('.modal-backdrop'))
        await expect(collection).toBeHidden()
      }
    })

    test('nested account backdrops close their own dialog and a drag out stays open', async ({ page }) => {
      await page.getByLabel('打开账户设置', { exact: true }).click()
      const account = page.locator('.account-modal')
      await account.locator('h2').first().click()
      await expect(account).toBeVisible()
      for (const selector of ['.password-entry', '.security-row']) {
        await account.locator(selector).click()
        const panel = page.locator('.account-subdialog')
        await expect(panel).toBeVisible()
        await panel.locator('h2').click()
        await expect(panel).toBeVisible()
        await dismiss(page.locator('.account-subdialog-backdrop'))
        await expect(panel).toBeHidden()
        await expect(account).toBeVisible()
      }
      const heading = (await account.locator('h2').first().boundingBox())!
      await page.mouse.move(heading.x + 5, heading.y + 5)
      await page.mouse.down()
      await page.mouse.move(2, 2, { steps: 5 })
      await page.mouse.up()
      await expect(account).toBeVisible()
      await dismiss(page.locator('.accounting'))
      await expect(account).toBeHidden()
    })

    test('file operation backdrops preserve the file and the editor keeps unsaved-change confirmation', async ({ page }) => {
      const file = await document(page)
      await enterSelectionMode(page)
      await page.locator('.file-card').getByRole('checkbox').press('Space')
      const toolbar = page.locator('.selection-toolbar')
      for (const [button, dialog, backdrop] of [
        ['重命名', '.modal', '.modal-backdrop'],
        ['移动', '.move-copy-dialog', '.transfer-backdrop'],
        ['分享', '.share-modal', '.modal-backdrop'],
        ['删除', '.app-dialog', '.dialog-backdrop'],
      ]) {
        await toolbar.getByRole('button', { name: button, exact: true }).click()
        await expect(page.locator(dialog)).toBeVisible()
        await page.locator(dialog).locator('h2').click()
        await expect(page.locator(dialog)).toBeVisible()
        await dismiss(page.locator(backdrop))
        await expect(page.locator(dialog)).toBeHidden()
      }
      await toolbar.getByLabel('取消', { exact: true }).click()
      await page.locator('.file-card').click()
      const editor = page.locator('.document-editor')
      await expect(editor.getByLabel('文档内容')).toHaveValue('original')
      await editor.getByRole('button', { name: '版本历史', exact: true }).click()
      const history = editor.getByRole('dialog', { name: '版本历史', exact: true })
      await expect(history).toBeVisible()
      await history.locator('header > strong').click()
      await expect(history).toBeVisible()
      await editor.getByLabel('文档内容').click()
      await expect(history).toBeHidden()
      await editor.getByLabel('文档内容').fill('unsaved content')
      // The mobile editor occupies the viewport; dispatch a backdrop click
      // there to exercise the same close callback without an exposed margin.
      if (mobile) await page.locator('.editing').dispatchEvent('click')
      else await dismiss(page.locator('.editing'))
      await expect(page.locator('.app-dialog')).toContainText('放弃未保存的修改')
      await dismiss(page.locator('.dialog-backdrop'))
      await expect(editor.getByLabel('文档内容')).toHaveValue('unsaved content')
      expect((await (await page.request.get(`/api/files/${file.id}/content`)).json()).content).toBe('original')
    })

    test('reader typography and image surfaces dismiss without treating content clicks as outside', async ({ page }) => {
      const file = await document(page, 'txt')
      await navigate(page, '书籍')
      await page.getByRole('button', { name: `打开 ${file.name}`, exact: true }).click()
      await expect(page.locator('#loading')).toBeHidden()
      await page.locator('#font-button').click()
      await expect(page.locator('#font-popover')).toBeVisible()
      await page.locator('#font-larger').click()
      await expect(page.locator('#font-popover')).toBeVisible()
      await page.locator('#reader-view .reader-bar').click({ position: { x: 2, y: 2 } })
      await expect(page.locator('#font-popover')).toBeHidden()
      await expect(page.locator('#reader-view')).toBeVisible()
      await page.locator('#reader-back').click()
      await navigate(page, '文件')
      await page.getByLabel('打开搜索', { exact: true }).click()
      await page.getByLabel('搜索文件名').fill('')
      await page.getByLabel('搜索文件名').press('Enter')
      await page.getByLabel('搜索文件名').press('Escape')
      const name = `dismissal-image-${Date.now()}.png`
      await page.locator('input[type=file]').first().setInputFiles({ name, mimeType: 'image/png', buffer: png })
      await expect(page.locator('.file-card').filter({ hasText: name })).toBeVisible()
      const listing = await (await page.request.get(`/api/files?q=${name}`)).json()
      created.push(listing.items[0].id)
      await page.locator('.file-card').filter({ hasText: name }).click()
      await expect(page.locator('.preview-image')).toBeVisible()
      await page.getByRole('button', { name: '放大', exact: true }).click()
      await expect(page.locator('.preview-modal')).toBeVisible()
      await dismiss(page.locator('.preview-stage'))
      await expect(page.locator('.preview-modal')).toBeHidden()
    })

    test('audio controls stay open and empty player space closes the preview', async ({ page }) => {
      const samples = 8000
      const wav = Buffer.alloc(44 + samples * 2)
      wav.write('RIFF', 0); wav.writeUInt32LE(36 + samples * 2, 4); wav.write('WAVEfmt ', 8)
      wav.writeUInt32LE(16, 16); wav.writeUInt16LE(1, 20); wav.writeUInt16LE(1, 22)
      wav.writeUInt32LE(8000, 24); wav.writeUInt32LE(16000, 28); wav.writeUInt16LE(2, 32)
      wav.writeUInt16LE(16, 34); wav.write('data', 36); wav.writeUInt32LE(samples * 2, 40)
      const name = `dismissal-audio-${Date.now()}.wav`
      await page.locator('input[type=file]').first().setInputFiles({ name, mimeType: 'audio/wav', buffer: wav })
      await expect(page.locator('.file-card').filter({ hasText: name })).toBeVisible()
      const listing = await (await page.request.get(`/api/files?q=${name}`)).json()
      const id = listing.items[0].id
      created.push(id)
      const removed = await page.request.delete(`/api/files/${id}`, { headers: { Origin: origin } })
      expect(removed.ok()).toBeTruthy()
      trashed.add(id)
      // Live audio opens the persistent music dock; the trash uses the modal
      // audio preview so files can be inspected before restoring them.
      await navigate(page, '回收站')
      await page.locator('.file-card').filter({ hasText: name }).click()
      await expect(page.locator('.chapter-audio-player')).toBeVisible()
      await page.getByLabel('播放速度', { exact: true }).selectOption('1.5')
      await expect(page.locator('.preview-modal')).toBeVisible()
      await dismiss(page.locator('.audio-main'))
      await expect(page.locator('.preview-modal')).toBeHidden()
    })
  })
}
