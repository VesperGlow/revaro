import { expect, test, type Page } from '@playwright/test'
import { readFileSync } from 'node:fs'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'
const codes = ['ABCD-1234', 'EFGH-5678', 'JKLM-9012']

async function mockAccount(page: Page, nullableSetup = false) {
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
      return json(nullableSetup
        ? { secret: 'JBSWY3DPEHPK3PXP', uri: null, qr_data_url: null }
        : { secret: 'JBSWY3DPEHPK3PXP', uri: 'otpauth://totp/Revaro:admin?secret=JBSWY3DPEHPK3PXP', qr_data_url: 'data:image/png;base64,not-a-real-qr' })
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

async function installClipboardFailure(page: Page) {
  await page.addInitScript(() => {
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: {
        writeText: async () => {
          throw new Error('clipboard denied')
        },
      },
    })
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
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
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
    const oldLines = oldDownload.text.split('\n')
    const newLines = newDownload.text.split('\n')
    expect(newLines[0]).toBe(oldLines[0])
    expect(newLines[1].replace(/\d/g, '#')).toBe(oldLines[1].replace(/\d/g, '#'))
    expect(newLines.slice(2), 'Rust 恢复码下载结构与 reference 不一致').toEqual(oldLines.slice(2))
    expect(newLines[1]).not.toMatch(/T\d{2}:\d{2}:\d{2}/)
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('old/new TOTP setup 显式 null 元数据时仍进入设置阶段', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockAccount(page, true)
    await page.goto(`${baseUrl}/?account-totp-null-setup=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.locator('button[title="打开账户设置"]').click()
    const account = page.locator('.account-modal')
    await account.getByRole('button', { name: '设置', exact: true }).click()
    const totp = page.locator('.totp-dialog')
    await totp.getByLabel('当前密码', { exact: true }).fill('password')
    await totp.getByRole('button', { name: '开始设置', exact: true }).click()
    await expect(totp.locator('.totp-enroll')).toBeVisible()
    return {
      secret: await totp.locator('.manual-secret code').textContent(),
      error: (await totp.locator('.two-factor-error').count()) > 0 ? await totp.locator('.two-factor-error').textContent() : null,
    }
  }

  try {
    const [oldState, newState] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldState).toEqual({ secret: 'JBSWY3DPEHPK3PXP', error: null })
    expect(newState, 'Rust TOTP setup 不应因未消费的 null 元数据而停在初始阶段').toEqual(oldState)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

async function enableTotp(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?account-copy-failure=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.locator('button[title="打开账户设置"]').click()
  const account = page.locator('.account-modal')
  await account.getByRole('button', { name: '设置', exact: true }).click()
  const totp = page.locator('.totp-dialog')
  await expect(totp).toBeVisible()
  await totp.getByLabel('当前密码', { exact: true }).fill('password')
  await totp.getByRole('button', { name: '开始设置', exact: true }).click()
  await totp.getByLabel('6 位验证码', { exact: true }).fill('123456')
  await totp.getByRole('button', { name: '启用并生成恢复码', exact: true }).click()
  await expect(totp.locator('.recovery-grid code')).toHaveCount(codes.length)
  return { account, totp }
}

test('恢复码复制失败时保留 reference 的局部错误且不产生全局 toast', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([
      mockAccount(oldPage),
      mockAccount(newPage),
      installClipboardFailure(oldPage),
      installClipboardFailure(newPage),
    ])
    const [{ totp: oldTotp }, { totp: newTotp }] = await Promise.all([
      enableTotp(oldPage, oldUrl),
      enableTotp(newPage, newUrl),
    ])
    await Promise.all([
      oldTotp.getByRole('button', { name: '复制恢复码' }).click(),
      newTotp.getByRole('button', { name: '复制恢复码' }).click(),
    ])
    await expect(oldTotp.locator('.two-factor-error')).toHaveText('复制失败，请手动保存恢复码')
    await expect(newTotp.locator('.two-factor-error')).toHaveText('复制失败，请手动保存恢复码')
    await expect(oldTotp.getByRole('button', { name: '复制恢复码' })).toBeVisible()
    await expect(newTotp.getByRole('button', { name: '复制恢复码' })).toBeVisible()
    expect(await newPage.locator('.toast').count(), '恢复码复制失败不应产生 reference 没有的全局 toast').toBe(await oldPage.locator('.toast').count())
    expect(await newTotp.textContent()).toBe(await oldTotp.textContent())
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})
