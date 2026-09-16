import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const FILE_ID = 'modal-history-file'
const STAMP = '2026-01-01T00:00:00Z'

const file = {
  id: FILE_ID,
  parent_id: ROOT,
  name: '历史弹层.txt',
  kind: 'file',
  size: 12,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'text/plain',
  etag: 'modal-history-etag',
}

async function mockShare(page: Page) {
  await page.route('**/api/**', async route => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/auth/totp') return json({ enabled: false, recovery_codes: 0 })
    if (path === '/api/auth/totp/setup') return json({
      secret: 'JBSWY3DPEHPK3PXP',
      uri: 'otpauth://totp/Revaro:admin?secret=JBSWY3DPEHPK3PXP',
      qr_data_url: 'data:image/png;base64,aGVsbG8=',
    })
    if (path === '/api/library/all') return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 1 } })
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 1 })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: [file], total_bytes: file.size, file_count: 1 })
    if (path === `/api/files/${FILE_ID}` && request.method() === 'PATCH') {
      const input = request.postDataJSON() as { name?: string }
      return json({ ...file, name: input.name || file.name })
    }
    if (path === `/api/files/${FILE_ID}/share` && request.method() === 'GET') {
      return json({ active: true, url: 'http://127.0.0.1:18084/s/modal-history-token', created_at: STAMP })
    }
    return json({ items: [] })
  })
}

async function openShare(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?modal-history-reference=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.getByTitle('列表视图').click()
  const row = page.locator('.file-row').filter({ hasText: file.name })
  await row.getByRole('button', { name: '选择项目' }).click()
  await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '分享' }).click()
  await expect(page.locator('.share-modal input[aria-label="分享链接"]')).toHaveValue(/modal-history-token/)
}

async function openAccount(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?account-history-reference=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.locator('button[title="打开账户设置"]').click()
  await expect(page.locator('.account-modal')).toBeVisible()
  await expect(page.locator('.account-modal').getByRole('button', { name: '设置', exact: true })).toBeEnabled()
}

async function layerSnapshot(page: Page) {
  return page.evaluate(() => ({
    path: location.pathname,
    share: document.querySelector('.share-modal')?.className ?? null,
    dialog: document.querySelector('.app-dialog')?.textContent?.replace(/\s+/g, ' ').trim() ?? null,
  }))
}

test('分享确认弹窗叠加时浏览器后退保留 reference 的弹层清理语义', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockShare(oldPage), mockShare(newPage)])
    await Promise.all([openShare(oldPage, oldUrl), openShare(newPage, newUrl)])
    await Promise.all([
      oldPage.locator('.share-modal').getByRole('button', { name: '停止分享' }).click(),
      newPage.locator('.share-modal').getByRole('button', { name: '停止分享' }).click(),
    ])
    await expect(oldPage.locator('.app-dialog')).toBeVisible()
    await expect(newPage.locator('.app-dialog')).toBeVisible()

    await Promise.all([oldPage.goBack(), newPage.goBack()])
    await expect(oldPage.locator('.share-modal')).toHaveCount(0)
    await expect(newPage.locator('.share-modal')).toHaveCount(0)
    // AppDialog is mounted outside the reference modal. Browser back closes
    // the share modal but leaves this confirmation open; preserve that
    // observed behavior even though it is an unusual navigation edge case.
    await expect(oldPage.locator('.app-dialog')).toBeVisible()
    await expect(newPage.locator('.app-dialog')).toBeVisible()
    expect(await layerSnapshot(newPage), 'Rust 嵌套确认弹层后退清理与 reference 不一致').toEqual(await layerSnapshot(oldPage))
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('TOTP 设置子弹窗打开时浏览器后退关闭账户层并在重开后重置', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockShare(page)
    await openAccount(page, baseUrl)
    const account = page.locator('.account-modal')
    await account.getByRole('button', { name: '设置', exact: true }).click()
    const totp = page.locator('.totp-dialog')
    await expect(totp.locator('.two-factor-idle')).toBeVisible()
    await totp.getByLabel('当前密码', { exact: true }).fill('test-current-password')
    await totp.getByRole('button', { name: '开始设置', exact: true }).click()
    await expect(totp.locator('.totp-enroll')).toBeVisible()

    await page.goBack()
    await expect(account).toHaveCount(0)
    await expect(totp).toHaveCount(0)
    const afterBack = new URL(page.url()).pathname

    await page.locator('button[title="打开账户设置"]').click()
    await expect(account).toBeVisible()
    await account.getByRole('button', { name: '设置', exact: true }).click()
    await expect(page.locator('.totp-dialog .two-factor-idle')).toBeVisible()
    await expect(page.locator('.totp-dialog .totp-enroll')).toHaveCount(0)

    return {
      afterBack,
      accountReopened: await account.isVisible(),
      setupReset: await page.locator('.totp-dialog .two-factor-idle').isVisible(),
      enrollmentCleared: await page.locator('.totp-dialog .totp-enroll').count() === 0,
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({
      afterBack: '/',
      accountReopened: true,
      setupReset: true,
      enrollmentCleared: true,
    })
    expect(newResult, 'Rust TOTP 子弹窗后的账户历史状态与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('重命名与移动弹窗打开时浏览器后退关闭弹窗并保留选择', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockShare(page)
    await page.goto(`${baseUrl}/?file-action-history-reference=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.getByTitle('列表视图').click()
    const row = page.locator('.file-row').filter({ hasText: file.name })
    await row.getByRole('button', { name: '选择项目' }).click()
    const toolbar = page.getByRole('toolbar', { name: '所选项目操作' })

    await toolbar.getByRole('button', { name: '重命名' }).click()
    const rename = page.locator('.modal-backdrop > .modal').filter({ hasText: '重命名' })
    await expect(rename).toBeVisible()
    await page.goBack()
    await expect(rename).toHaveCount(0)
    await expect(toolbar).toBeVisible()

    await toolbar.getByRole('button', { name: '移动' }).click()
    const move = page.locator('.move-copy-dialog')
    await expect(move).toBeVisible()
    await page.goBack()
    await expect(move).toHaveCount(0)
    await expect(toolbar).toBeVisible()

    await toolbar.getByRole('button', { name: '重命名' }).click()
    const cancelRename = page.locator('.modal-backdrop > .modal').filter({ hasText: '重命名' })
    await page.evaluate(() => {
      const target = window as Window & { __historyPopCount?: number }
      target.__historyPopCount = 0
      window.addEventListener('popstate', () => { target.__historyPopCount = (target.__historyPopCount || 0) + 1 })
    })
    await cancelRename.getByRole('button', { name: '取消', exact: true }).click()
    await expect(cancelRename).toHaveCount(0)
    await expect.poll(() => page.evaluate(() => (window as Window & { __historyPopCount?: number }).__historyPopCount)).toBe(1)
    await expect(toolbar).toBeVisible()

    await toolbar.getByRole('button', { name: '重命名' }).click()
    const saveRename = page.locator('.modal-backdrop > .modal').filter({ hasText: '重命名' })
    await saveRename.locator('input').fill('历史重命名成功.txt')
    await page.evaluate(() => { (window as Window & { __historyPopCount?: number }).__historyPopCount = 0 })
    await saveRename.getByRole('button', { name: '保存', exact: true }).click()
    await expect(saveRename).toHaveCount(0)
    await expect(page.locator('.toast')).toHaveText('已重命名')
    await expect.poll(() => page.evaluate(() => (window as Window & { __historyPopCount?: number }).__historyPopCount)).toBe(1)

    return {
      path: new URL(page.url()).pathname,
      selectedRows: await page.locator('.file-row.selected').count(),
      renameClosed: await rename.count() === 0,
      moveClosed: await move.count() === 0,
      cancelConsumedHistory: true,
      saveConsumedHistory: true,
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({
      path: '/',
      selectedRows: 0,
      renameClosed: true,
      moveClosed: true,
      cancelConsumedHistory: true,
      saveConsumedHistory: true,
    })
    expect(newResult, 'Rust 重命名/移动弹窗的浏览器后退结果与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})
