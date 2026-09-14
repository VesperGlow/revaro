import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const FILE_ID = 'transfer-dialog-file'
const STAMP = '2026-01-01T00:00:00Z'

const file = {
  id: FILE_ID,
  parent_id: ROOT,
  name: '传输中的文件.txt',
  kind: 'file',
  size: 12,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'text/plain',
}

function deferred() {
  let resolve!: () => void
  const promise = new Promise<void>(value => { resolve = value })
  return { promise, resolve }
}

async function mockTransfer(page: Page) {
  const transferGate = deferred()
  await page.route('**/api/**', async route => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 1 } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 1 })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: [file], total_bytes: file.size, file_count: 1 })
    if (path === `/api/files/${FILE_ID}` && request.method() === 'PATCH') {
      await transferGate.promise
      return json({ ...file })
    }
    return json({ items: [] })
  })
  return { transferGate }
}

async function startTransfer(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?transfer-reference=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.getByTitle('列表视图').click()
  const row = page.locator('.file-row').filter({ hasText: file.name })
  await row.getByRole('button', { name: '选择项目' }).click()
  await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '移动' }).click()
  await expect(page.locator('.move-copy-dialog')).toBeVisible()
  await expect(page.locator('.directory-trigger')).toBeVisible()
  await page.locator('.move-copy-dialog').getByRole('button', { name: '移动', exact: true }).click()
  await expect(page.locator('.move-copy-dialog .primary')).toBeDisabled()
}

test('移动请求进行中点击遮罩仍关闭传输弹窗', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    const [oldMock, newMock] = await Promise.all([mockTransfer(oldPage), mockTransfer(newPage)])
    await Promise.all([startTransfer(oldPage, oldUrl), startTransfer(newPage, newUrl)])
    await Promise.all([
      oldPage.locator('.modal-backdrop').filter({ has: oldPage.locator('.move-copy-dialog') }).click({ position: { x: 8, y: 8 } }),
      newPage.locator('.transfer-backdrop').click({ position: { x: 8, y: 8 } }),
    ])
    await expect(oldPage.locator('.move-copy-dialog')).toHaveCount(0)
    await expect(newPage.locator('.move-copy-dialog'), 'Rust 传输请求中遮罩关闭与 reference 不一致').toHaveCount(0)
    oldMock.transferGate.resolve()
    newMock.transferGate.resolve()
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})
