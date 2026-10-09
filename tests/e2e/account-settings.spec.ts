import { expect, test, type Locator, type Page } from '@playwright/test'
import { login } from './helpers'
import { mkdirSync } from 'node:fs'

const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=', 'base64')

async function mockAccountRequests(page: Page) {
  // Worker network routing is only observable in Chromium. Mock application
  // fetches before they enter the worker so profile tests stay isolated.
  await page.addInitScript(() => {
    const fetch = window.fetch.bind(window)
    const state = (window as any).accountProbe = { uploads: 0, lastAvatar: '', usernames: [] }
    window.fetch = async (input, init) => {
      const path = new URL(typeof input === 'string' ? input : input instanceof Request ? input.url : input.toString(), location.href).pathname
      const method = (init?.method || (input instanceof Request ? input.method : 'GET')).toUpperCase()
      const body = async () => JSON.parse(typeof init?.body === 'string' ? init.body : input instanceof Request ? await input.clone().text() : '{}')
      if (path === '/api/auth/totp' && method === 'GET')
        return new Response(JSON.stringify({ enabled: true, recovery_codes: 5 }), { headers: { 'Content-Type': 'application/json' } })
      if (path === '/api/profile/username' && method === 'PATCH') {
        state.usernames.push(await body())
        return new Response(null, { status: 204 })
      }
      if (path === '/api/profile/avatar' && method === 'PUT') {
        state.uploads++
        state.lastAvatar = (await body()).data_url
        return new Response(null, { status: 204 })
      }
      if (path === '/api/profile/avatar' && method === 'DELETE') return new Response(null, { status: 204 })
      return fetch(input, init)
    }
  })
  await page.context().route('**/api/profile/avatar*', route => {
    if (route.request().method() === 'GET') return route.fulfill({ contentType: 'image/png', body: png })
    return route.continue()
  })
}

test('closing account settings tolerates a late security-status response', async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await mockAccountRequests(page)
  await login(page)
  await page.evaluate(() => {
    const fetch = window.fetch.bind(window)
    const state = (window as any).lateAccountStatus = { requested: false, release: () => {} }
    window.fetch = async (input, init) => {
      const path = new URL(typeof input === 'string' ? input : input instanceof Request ? input.url : input.toString(), location.href).pathname
      if (path === '/api/auth/totp') {
        state.requested = true
        await new Promise<void>(resolve => { state.release = resolve })
      }
      return fetch(input, init)
    }
  })
  await page.getByLabel('打开账户设置', { exact: true }).click()
  const account = page.getByRole('dialog', { name: '账户设置', exact: true })
  await expect.poll(() => page.evaluate(() => (window as any).lateAccountStatus.requested)).toBe(true)
  await account.getByRole('button', { name: '关闭', exact: true }).click()
  await page.evaluate(() => (window as any).lateAccountStatus.release())
  await page.waitForTimeout(250)
  expect(errors).toEqual([])
  await page.getByLabel('打开账户设置', { exact: true }).click()
  await page.evaluate(() => (window as any).lateAccountStatus.release())
  await expect(account.getByLabel('两步验证', { exact: true })).toContainText('已启用')
})

test('an unreadable avatar image preserves profile controls', async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await mockAccountRequests(page)
  await page.context().route('**/api/profile/avatar*', route => route.fulfill({ contentType: 'image/png', body: 'invalid image data' }))
  await login(page)
  await page.getByLabel('打开账户设置', { exact: true }).click()
  const account = page.getByRole('dialog', { name: '账户设置', exact: true })
  await account.getByRole('button', { name: '更换头像', exact: true }).click()
  const avatar = page.getByRole('dialog', { name: '更换头像', exact: true })
  await account.locator('input[type="file"]').setInputFiles({ name: 'avatar.png', mimeType: 'image/png', buffer: png })
  await expect.poll(() => page.evaluate(() => (window as any).accountProbe.uploads)).toBe(1)
  await expect(page.locator('.avatar-badge img')).toHaveCount(0)
  await expect(avatar.getByRole('button', { name: '移除头像', exact: true })).toBeEnabled()
  await avatar.getByRole('button', { name: '移除头像', exact: true }).click()
  await expect(account.getByRole('img', { name: '个人头像', exact: true })).toHaveCount(0)
  expect(errors).toEqual([])
})

test('avatar read failures can be retried and signing out aborts a pending read', async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await page.addInitScript(() => {
    const NativeReader = window.FileReader
    const state = (window as any).avatarReadProbe = { attempts: 0, pending: false, aborted: false }
    window.FileReader = class extends NativeReader {
      readAsDataURL(blob: Blob) {
        state.attempts++
        if (state.attempts === 1) throw new Error('unreadable test file')
        if (state.attempts === 3) { state.pending = true; return }
        return super.readAsDataURL(blob)
      }
      abort() {
        if (state.pending) { state.aborted = true; state.pending = false }
        return super.abort()
      }
    }
  })
  await mockAccountRequests(page)
  await login(page)
  await page.getByLabel('打开账户设置', { exact: true }).click()
  const account = page.getByRole('dialog', { name: '账户设置', exact: true })
  await account.getByRole('button', { name: '更换头像', exact: true }).click()
  const avatar = page.getByRole('dialog', { name: '更换头像', exact: true })
  const input = account.locator('input[type="file"]')
  const file = { name: 'avatar.png', mimeType: 'image/png', buffer: png }
  await input.setInputFiles(file)
  await expect(avatar.getByRole('alert')).toHaveText('无法读取图片')
  await expect(avatar.getByRole('button', { name: '选择图片', exact: true })).toBeEnabled()
  expect(await page.evaluate(() => (window as any).accountProbe.uploads)).toBe(0)
  await input.setInputFiles(file)
  await expect.poll(() => page.evaluate(() => (window as any).accountProbe.uploads)).toBe(1)
  await expect(avatar.getByRole('button', { name: '选择图片', exact: true })).toBeEnabled()
  await expect(avatar.getByRole('alert')).toHaveCount(0)
  await input.setInputFiles(file)
  await expect(avatar.getByRole('button', { name: '处理中…', exact: true })).toBeDisabled()
  await avatar.getByRole('button', { name: '关闭', exact: true }).click()
  await account.getByRole('button', { name: '退出登录', exact: true }).click()
  await expect(page.getByLabel('用户名', { exact: true })).toBeVisible()
  expect(await page.evaluate(() => (window as any).avatarReadProbe.aborted)).toBe(true)
  expect(await page.evaluate(() => (window as any).accountProbe.uploads)).toBe(1)
  expect(errors).toEqual([])
})

test('repeated avatar uploads release the completed file readers', async ({ page, browserName }) => {
  test.skip(browserName !== 'chromium', 'Garbage collection is measured through Chromium CDP')
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await page.addInitScript(() => {
    const NativeReader = window.FileReader
    ;(window as any).avatarReaders = []
    window.FileReader = class extends NativeReader {
      constructor() {
        super()
        ;(window as any).avatarReaders.push(new WeakRef(this))
      }
    }
  })
  await mockAccountRequests(page)
  await login(page)
  await page.getByLabel('打开账户设置', { exact: true }).click()
  const account = page.getByRole('dialog', { name: '账户设置', exact: true })
  await account.getByRole('button', { name: '更换头像', exact: true }).click()
  const avatar = page.getByRole('dialog', { name: '更换头像', exact: true })
  const bytes = Buffer.concat([png, Buffer.alloc(256 * 1024)])
  for (let index = 0; index < 30; index++) {
    await account.locator('input[type="file"]').setInputFiles({ name: `avatar-${index}.png`, mimeType: 'image/png', buffer: bytes })
    await expect.poll(() => page.evaluate(() => (window as any).accountProbe.uploads)).toBe(index + 1)
    await expect(avatar.getByRole('button', { name: '选择图片', exact: true })).toBeEnabled()
  }
  const cdp = await page.context().newCDPSession(page)
  for (let index = 0; index < 3; index++) {
    await cdp.send('HeapProfiler.collectGarbage')
    await page.waitForTimeout(100)
  }
  const retained = await page.evaluate(() => (window as any).avatarReaders.filter((reader: WeakRef<FileReader>) => reader.deref()).length)
  console.log(`Retained FileReaders after 30 uploads: ${retained}`)
  expect(retained, 'completed readers retain the selected image data').toBeLessThanOrEqual(2)
  expect(errors).toEqual([])
})

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
      await mockAccountRequests(page)
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
      const account = page.getByRole('dialog', { name: '账户设置', exact: true })
      const original = await account.locator('.account-profile-copy strong').innerText()
      const row = account.getByLabel('修改用户名', { exact: true })
      await row.press('Enter')
      const editor = page.getByRole('dialog', { name: '修改用户名', exact: true })
      await editor.getByLabel('用户名', { exact: true }).fill('cancelled-name')
      await editor.getByRole('button', { name: '取消', exact: true }).click()
      await expect(editor).toBeHidden()
      expect(await page.evaluate(() => (window as any).accountProbe.usernames)).toEqual([])
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
      expect(await page.evaluate(() => (window as any).accountProbe.usernames)).toEqual([{ username: name }])
      await expect(row).toContainText(name)
      await expect(account.locator('.account-profile-copy strong')).toHaveText(name)
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(page.viewportSize()!.width)
    })

    test('avatar requirements appear on demand and image changes keep the list intact', async ({ page }) => {
      const account = page.getByRole('dialog', { name: '账户设置', exact: true })
      await account.getByRole('button', { name: '更换头像', exact: true }).click()
      const avatar = page.getByRole('dialog', { name: '更换头像', exact: true })
      await expect(avatar).toContainText('支持 JPG、PNG、GIF 和 WebP，最大 2 MiB')
      const chooser = page.waitForEvent('filechooser')
      await avatar.getByRole('button', { name: '选择图片', exact: true }).click()
      await (await chooser).setFiles({ name: 'avatar.png', mimeType: 'image/png', buffer: png })
      await expect(account.getByRole('img', { name: '个人头像', exact: true })).toBeVisible()
      expect(await page.evaluate(() => (window as any).accountProbe.uploads)).toBe(1)
      expect(await page.evaluate(() => (window as any).accountProbe.lastAvatar)).toMatch(/^data:image\/png;base64,/)
      await avatar.getByRole('button', { name: '移除头像', exact: true }).click()
      await expect(account.getByRole('img', { name: '个人头像', exact: true })).toBeHidden()
      await dismiss(page.locator('.account-subdialog-backdrop'))
      await expect(account).not.toContainText('MiB')
      await expect(account.getByRole('group', { name: '账户与安全设置' }).getByRole('button')).toHaveCount(3)
    })
  })
}
