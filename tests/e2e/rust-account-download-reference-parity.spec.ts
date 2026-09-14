import { expect, test, type Page } from '@playwright/test'
import { readFileSync } from 'node:fs'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'
const codes = ['ABCD-1234', 'EFGH-5678', 'JKLM-9012']

async function mockAccount(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/auth/totp') return json({ enabled: false, recovery_codes: 0 })
    if (path === '/api/auth/totp/setup') {
      return json({ secret: 'JBSWY3DPEHPK3PXP', uri: 'otpauth://totp/Revaro:admin?secret=JBSWY3DPEHPK3PXP', qr_data_url: 'data:image/png;base64,not-a-real-qr' })
    }
    if (path === '/api/auth/totp/enable') return json({ enabled: true, recovery_codes: codes })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 0 } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 0 })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    return json({ items: [] })
  })
}

async function enableTotpAndDownload(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?account-download-parity=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.locator('button[title="打开账户设置"]').click()
  const account = page.locator('.account-modal')
  await expect(account.getByRole('button', { name: '设置', exact: true })).toBeEnabled()
  await account.getByRole('button', { name: '设置', exact: true }).click()
  const totp = page.locator('.totp-dialog')
  await expect(totp).toBeVisible()
  await totp.getByLabel('当前密码', { exact: true }).fill('password')
  await totp.getByRole('button', { name: '开始设置', exact: true }).click()
  await expect(totp.locator('.manual-secret code')).toHaveText('JBSWY3DPEHPK3PXP')
  await totp.getByLabel('6 位验证码', { exact: true }).fill('123456')
  await totp.getByRole('button', { name: '启用并生成恢复码', exact: true }).click()
  await expect(totp.locator('.recovery-grid code')).toHaveCount(codes.length)
  const downloadPromise = page.waitForEvent('download')
  await totp.getByRole('button', { name: '下载文本', exact: true }).click()
  const download = await downloadPromise
  const path = await download.path()
  if (!path) throw new Error('恢复码下载没有临时文件')
  return { filename: download.suggestedFilename(), text: readFileSync(path, 'utf8') }
}

test('恢复码下载文本格式保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockAccount(oldPage), mockAccount(newPage)])
    const [oldDownload, newDownload] = await Promise.all([
      enableTotpAndDownload(oldPage, oldUrl),
      enableTotpAndDownload(newPage, newUrl),
    ])
    expect(newDownload.filename).toBe(oldDownload.filename)
    expect(newDownload.text, 'Rust 恢复码下载内容与 reference 不一致').toBe(oldDownload.text)
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})
