import { expect, test } from '@playwright/test'
import { createHmac } from 'node:crypto'
import { login } from './helpers'

function totpCode(secret: string, now = Date.now()) {
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567'
  const bytes: number[] = []
  let buffer = 0
  let bits = 0
  for (const character of secret.replace(/\s+/g, '').toUpperCase()) {
    const value = alphabet.indexOf(character)
    if (value < 0) continue
    buffer = (buffer << 5) | value
    bits += 5
    if (bits >= 8) {
      bits -= 8
      bytes.push((buffer >>> bits) & 0xff)
    }
  }
  const counter = Buffer.alloc(8)
  counter.writeBigUInt64BE(BigInt(Math.floor(now / 30_000)))
  const digest = createHmac('sha1', Buffer.from(bytes)).update(counter).digest()
  const offset = digest[digest.length - 1] & 0x0f
  const value = ((digest[offset] & 0x7f) << 24)
    | (digest[offset + 1] << 16)
    | (digest[offset + 2] << 8)
    | digest[offset + 3]
  return String(value % 1_000_000).padStart(6, '0')
}

async function currentTotp(secret: string) {
  // Avoid submitting in the last instant of a 30-second window.
  const remaining = 30_000 - (Date.now() % 30_000)
  if (remaining < 2_000) await new Promise(resolve => setTimeout(resolve, remaining + 100))
  return totpCode(secret)
}

test('账户设置保留用户名、头像校验和安全子面板行为', async ({ page }) => {
  await login(page)
  await page.locator('button[title="打开账户设置"]').click()
  const account = page.locator('.account-modal')
  await expect(account).toBeVisible()
  await expect(account).toContainText('支持 JPG、PNG、GIF 和 WebP，最大 2 MiB。')
  await expect(account).toContainText('当前会话')

  await account.getByRole('button', { name: '编辑用户名' }).click()
  const username = account.locator('input[aria-label="用户名"]')
  await expect(username).toBeFocused()
  const selection = await username.evaluate(input => ({
    start: (input as HTMLInputElement).selectionStart,
    end: (input as HTMLInputElement).selectionEnd,
  }))
  expect(selection.start).toBe(0)
  expect(selection.end).toBe(selection.start! + 'admin'.length)
  await username.fill('')
  await username.press('Enter')
  await expect(account).toContainText('用户名不能为空')
  await username.press('Escape')
  await expect(account.locator('input[aria-label="用户名"]')).toHaveCount(0)
  await expect(account).toContainText('admin')

  await account.getByRole('button', { name: '修改密码' }).click()
  const password = page.locator('.password-dialog')
  await expect(password).toBeVisible()
  await password.getByLabel('当前密码', { exact: true }).fill(process.env.E2E_PASSWORD || 'revaro-e2e-password')
  await password.getByLabel('新密码', { exact: true }).fill('new-password-1')
  await password.getByLabel('确认新密码', { exact: true }).fill('new-password-2')
  await password.getByRole('button', { name: '修改密码' }).click()
  await expect(password).toContainText('两次输入的新密码不一致')
  await password.getByRole('button', { name: '取消' }).click()

  await account.locator('input[type=file]').setInputFiles({ name: 'not-an-avatar.pdf', mimeType: 'application/pdf', buffer: Buffer.from('invalid') })
  await expect(account).toContainText('请选择 JPG、PNG、GIF 或 WebP 图片')

  const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=', 'base64')
  await account.locator('input[type=file]').setInputFiles({ name: 'avatar.png', mimeType: 'image/png', buffer: png })
  await expect(account.locator('img[alt="个人头像"]')).toBeVisible()
  await expect(page.locator('.toast')).toHaveText('头像已更新')
  await account.getByRole('button', { name: '移除' }).click()
  await expect(account.locator('img[alt="个人头像"]')).toHaveCount(0)
  await expect(page.locator('.toast')).toHaveText('头像已移除')

  await account.getByRole('button', { name: '编辑用户名' }).click()
  const renamed = `parity-user-${crypto.randomUUID().slice(0, 8)}`
  await account.locator('input[aria-label="用户名"]').fill(renamed)
  await account.locator('input[aria-label="用户名"]').press('Enter')
  await expect(account).toContainText(renamed)
  await expect(page.locator('.toast')).toHaveText('用户名已保存')
  await account.getByRole('button', { name: '编辑用户名' }).click()
  await account.locator('input[aria-label="用户名"]').fill('admin')
  await account.locator('input[aria-label="用户名"]').press('Enter')
  await expect(account).toContainText('admin')

  await account.getByRole('button', { name: '设置' }).click()
  const totp = page.locator('.totp-dialog')
  await expect(totp).toBeVisible()
  await expect(totp).toContainText('两步验证')
  const totpPassword = totp.getByLabel('当前密码', { exact: true })
  await totpPassword.fill('not-the-password')
  await totp.getByRole('button', { name: '开始设置' }).click()
  await expect(totp).toContainText('current password is incorrect')
  await totp.getByRole('button', { name: '关闭' }).click()
  await expect(totp).toHaveCount(0)
  await account.locator('header button').click()
  await expect(account).toHaveCount(0)
})

test('两步验证可按 reference 完成启用、恢复码重生成和关闭', async ({ page }) => {
  await login(page)
  await page.locator('button[title="打开账户设置"]').click()
  const account = page.locator('.account-modal')
  await account.getByRole('button', { name: '设置', exact: true }).click()
  const totp = page.locator('.totp-dialog')
  await expect(totp).toBeVisible()

  await totp.getByLabel('当前密码', { exact: true }).fill(process.env.E2E_PASSWORD || 'revaro-e2e-password')
  await totp.getByRole('button', { name: '开始设置' }).click()
  await expect(totp.locator('.manual-secret code')).toBeVisible()
  const secret = await totp.locator('.manual-secret code').innerText()
  await totp.getByLabel('6 位验证码', { exact: true }).fill(await currentTotp(secret))
  await totp.getByRole('button', { name: '启用并生成恢复码' }).click()
  await expect(account).toContainText('已启用')
  await expect(totp.locator('.recovery-grid code')).toHaveCount(10)

  const firstCodes = await totp.locator('.recovery-grid code').allTextContents()
  await totp.getByLabel('当前密码', { exact: true }).fill(process.env.E2E_PASSWORD || 'revaro-e2e-password')
  // The reference server rejects reusing the same TOTP counter. Use one of
  // the freshly issued recovery codes for the next privileged operation.
  await totp.getByLabel('验证码或恢复码', { exact: true }).fill(firstCodes[0])
  await totp.getByRole('button', { name: '重新生成恢复码' }).click()
  const regenerate = page.getByRole('dialog').filter({ hasText: '重新生成恢复码？' })
  await regenerate.getByRole('button', { name: '重新生成', exact: true }).click()
  await expect.poll(() => totp.locator('.recovery-grid code').allTextContents()).not.toEqual(firstCodes)

  await totp.getByLabel('当前密码', { exact: true }).fill(process.env.E2E_PASSWORD || 'revaro-e2e-password')
  const regeneratedCodes = await totp.locator('.recovery-grid code').allTextContents()
  await totp.getByLabel('验证码或恢复码', { exact: true }).fill(regeneratedCodes[0])
  await totp.getByRole('button', { name: '关闭两步验证' }).click()
  const disable = page.getByRole('dialog').filter({ hasText: '关闭两步验证？' })
  await disable.getByRole('button', { name: '关闭验证' }).click()
  await expect(account).toContainText('未启用')
})
