import { expect, test, type Locator } from '@playwright/test'
import { login } from './helpers'
import { mkdirSync } from 'node:fs'

const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=', 'base64')

for (const mobile of [false, true]) {
  test.describe(mobile ? 'mobile account settings' : 'desktop account settings', () => {
    test.use({ viewport: mobile ? { width: 390, height: 844 } : { width: 1280, height: 900 }, isMobile: mobile, hasTouch: mobile })
    let errors: string[] = []

    async function dismiss(backdrop: Locator) {
      const position = { x: 2, y: 2 }
      if (mobile) await backdrop.tap({ position })
      else await backdrop.click({ position })
    }

    test.beforeEach(async ({ page }) => {
      errors = []
      page.on('pageerror', error => errors.push(error.message))
      await page.route('**/api/auth/totp', route => route.fulfill({ json: { enabled: true, recovery_codes: 5 } }))
      await login(page)
      await page.getByLabel('打开账户设置', { exact: true }).click()
    })
    test.afterEach(() => expect(errors).toEqual([]))

    test('compact settings stay on one line and explain security only when opened', async ({ page }) => {
      const account = page.getByRole('dialog', { name: '账户设置', exact: true })
      const settings = account.getByRole('group', { name: '账户与安全设置', exact: true })
      await expect(settings.getByRole('button')).toHaveCount(3)
      await expect(settings.getByLabel('两步验证', { exact: true })).toContainText('已启用')
      await expect(account.getByRole('button', { name: '更换头像', exact: true })).toBeVisible()
      await expect(account).not.toContainText('MiB')
      await expect(account).not.toContainText('TOTP')
      await expect(account).not.toContainText('恢复码')

      for (const width of mobile ? [320, 390, 430] : [768, 1280]) {
        await page.setViewportSize({ width, height: mobile ? 844 : 900 })
        const bounds = (await account.boundingBox())!
        expect(bounds.height).toBeLessThan(400)
        expect(bounds.x).toBeGreaterThanOrEqual(0)
        expect(bounds.x + bounds.width).toBeLessThanOrEqual(width)
        for (const row of await settings.getByRole('button').all()) {
          const label = (await row.locator('.setting-label').boundingBox())!
          const status = (await row.locator('.setting-status').boundingBox())!
          const size = (await row.boundingBox())!
          expect(Math.abs(label.y + label.height / 2 - status.y - status.height / 2)).toBeLessThan(2)
          expect(size.height).toBeGreaterThanOrEqual(44)
          expect(size.height).toBeLessThanOrEqual(56)
          expect(status.x + status.width).toBeLessThan(size.x + size.width)
        }
        expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
      }
      mkdirSync('/tmp/revaro-account-preview', { recursive: true })
      await account.screenshot({ path: `/tmp/revaro-account-preview/account-${mobile ? 'mobile' : 'desktop'}.png` })

      await settings.getByLabel('两步验证', { exact: true }).click({ position: { x: 2, y: 26 } })
      const totp = page.locator('.totp-dialog')
      await expect(totp).toContainText('5 枚恢复码')
      await dismiss(page.locator('.account-subdialog-backdrop'))
      await expect(account).toBeVisible()
      await settings.getByRole('button', { name: /修改密码/ }).click({ position: { x: 2, y: 26 } })
      await expect(page.locator('.password-dialog')).toContainText('所有设备都需要使用新密码重新登录')
      await expect(page.locator('.password-dialog input')).toHaveCount(3)
      await dismiss(page.locator('.account-subdialog-backdrop'))
      await account.getByRole('button', { name: '退出登录', exact: true }).click()
      await expect(page.getByLabel('用户名', { exact: true })).toBeVisible()
    })

    test('username changes require saving and update both the profile and row', async ({ page }) => {
      const submitted: unknown[] = []
      await page.route('**/api/profile/username', route => {
        submitted.push(route.request().postDataJSON())
        return route.fulfill({ status: 204 })
      })
      const account = page.getByRole('dialog', { name: '账户设置', exact: true })
      const original = await account.locator('.account-profile-copy strong').innerText()
      const row = account.getByLabel('修改用户名', { exact: true })
      await row.press('Enter')
      const editor = page.getByRole('dialog', { name: '修改用户名', exact: true })
      await editor.getByLabel('用户名', { exact: true }).fill('cancelled-name')
      await editor.getByRole('button', { name: '取消', exact: true }).click()
      await expect(editor).toBeHidden()
      expect(submitted).toEqual([])
      await expect(row).toContainText(original)
      await row.click()
      await expect(editor.getByLabel('用户名', { exact: true })).toHaveValue(original)
      await editor.getByLabel('用户名', { exact: true }).fill('   ')
      await editor.getByRole('button', { name: '保存', exact: true }).click()
      await expect(editor.getByRole('alert')).toHaveText('用户名不能为空')
      const name = `reader_${'long_name_'.repeat(8)}`
      await editor.getByLabel('用户名', { exact: true }).fill(name)
      await editor.getByLabel('用户名', { exact: true }).press('Enter')
      await expect(editor).toBeHidden()
      expect(submitted).toEqual([{ username: name }])
      await expect(row).toContainText(name)
      await expect(account.locator('.account-profile-copy strong')).toHaveText(name)
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(page.viewportSize()!.width)
    })

    test('avatar requirements appear on demand and image changes keep the list intact', async ({ page }) => {
      const uploads: string[] = []
      await page.route('**/api/profile/avatar*', route => {
        if (route.request().method() === 'PUT') {
          uploads.push(route.request().postDataJSON().data_url)
          return route.fulfill({ status: 204 })
        }
        if (route.request().method() === 'DELETE') return route.fulfill({ status: 204 })
        return route.fulfill({ contentType: 'image/png', body: png })
      })
      const account = page.getByRole('dialog', { name: '账户设置', exact: true })
      await account.getByRole('button', { name: '更换头像', exact: true }).click()
      const avatar = page.getByRole('dialog', { name: '更换头像', exact: true })
      await expect(avatar).toContainText('支持 JPG、PNG、GIF 和 WebP，最大 2 MiB')
      const chooser = page.waitForEvent('filechooser')
      await avatar.getByRole('button', { name: '选择图片', exact: true }).click()
      await (await chooser).setFiles({ name: 'avatar.png', mimeType: 'image/png', buffer: png })
      await expect(account.getByRole('img', { name: '个人头像', exact: true })).toBeVisible()
      expect(uploads).toHaveLength(1)
      expect(uploads[0]).toMatch(/^data:image\/png;base64,/)
      await avatar.getByRole('button', { name: '移除头像', exact: true }).click()
      await expect(account.getByRole('img', { name: '个人头像', exact: true })).toBeHidden()
      await dismiss(page.locator('.account-subdialog-backdrop'))
      await expect(account).not.toContainText('MiB')
      await expect(account.getByRole('group', { name: '账户与安全设置' }).getByRole('button')).toHaveCount(3)
    })
  })
}
