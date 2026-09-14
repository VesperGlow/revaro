import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const FILE_ID = 'share-dialog-file'
const STAMP = '2026-01-01T00:00:00Z'

const file = {
  id: FILE_ID,
  parent_id: ROOT,
  name: '分享状态.txt',
  kind: 'file',
  size: 123,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'text/plain',
  etag: 'share-dialog-etag',
}

function deferred() {
  let resolve!: () => void
  const promise = new Promise<void>(value => { resolve = value })
  return { promise, resolve }
}

async function mockShare(page: Page) {
  const readGate = deferred()
  const createGate = deferred()
  let firstRead = true
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
    if (path === `/api/files/${FILE_ID}/share` && request.method() === 'GET') {
      if (firstRead) {
        firstRead = false
        await readGate.promise
      }
      return json({ active: false })
    }
    if (path === `/api/files/${FILE_ID}/share` && request.method() === 'POST') {
      await createGate.promise
      return json({ active: true, url: 'http://127.0.0.1:18083/s/share-dialog-token', created_at: STAMP })
    }
    return json({ items: [] })
  })
  return { readGate, createGate }
}

async function openShare(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await expect(page.locator('.file-card')).toHaveCount(1)
  await page.getByTitle('列表视图').click()
  const row = page.locator('.file-row').filter({ hasText: file.name })
  await row.getByRole('button', { name: '选择项目' }).click()
  await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '分享' }).click()
  await expect(page.locator('.share-modal .state.small')).toBeVisible()
}

async function dialogSnapshot(page: Page) {
  return page.locator('.share-modal').evaluate(dialog => ({
    text: dialog.textContent?.replace(/\s+/g, ' ').trim(),
    buttons: Array.from(dialog.querySelectorAll('button')).map(button => ({
      text: button.textContent?.replace(/\s+/g, ' ').trim(),
      disabled: (button as HTMLButtonElement).disabled,
    })),
    input: (() => {
      const input = dialog.querySelector('input') as HTMLInputElement | null
      return input ? { value: input.value, type: input.type, readOnly: input.readOnly } : null
    })(),
    rect: (() => {
      const rect = dialog.getBoundingClientRect()
      return { width: rect.width, height: rect.height }
    })(),
  }))
}

test('分享弹窗 loading 与 active 状态保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    const [oldMock, newMock] = await Promise.all([mockShare(oldPage), mockShare(newPage)])
    await Promise.all([openShare(oldPage, oldUrl), openShare(newPage, newUrl)])

    const oldClose = oldPage.locator('.share-modal header button')
    const newClose = newPage.locator('.share-modal header button')
    expect(await newClose.isDisabled(), 'Rust loading 时关闭按钮应保持 reference 可用状态').toBe(await oldClose.isDisabled())
    expect(await oldClose.isDisabled()).toBe(false)

    await Promise.all([oldClose.click(), newClose.click()])
    await expect(oldPage.locator('.share-modal')).toHaveCount(0)
    await expect(newPage.locator('.share-modal')).toHaveCount(0)
    oldMock.readGate.resolve()
    newMock.readGate.resolve()

    await Promise.all([
      oldPage.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '分享' }).click(),
      newPage.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '分享' }).click(),
    ])
    await expect(oldPage.locator('.share-modal').getByRole('button', { name: '创建公开链接' })).toBeVisible()
    await expect(newPage.locator('.share-modal').getByRole('button', { name: '创建公开链接' })).toBeVisible()
    expect(await dialogSnapshot(newPage), 'Rust 分享弹窗初始状态与 reference 不一致').toEqual(await dialogSnapshot(oldPage))

    await Promise.all([
      oldPage.locator('.share-modal').getByRole('button', { name: '创建公开链接' }).click(),
      newPage.locator('.share-modal').getByRole('button', { name: '创建公开链接' }).click(),
    ])
    await expect(oldPage.locator('.share-modal .state.small')).toBeVisible()
    await expect(newPage.locator('.share-modal .state.small')).toBeVisible()
    expect(await newClose.isDisabled(), 'Rust 创建分享 loading 时关闭按钮应保持 reference 可用状态').toBe(await oldClose.isDisabled())
    expect(await oldClose.isDisabled()).toBe(false)

    oldMock.createGate.resolve()
    newMock.createGate.resolve()
    await expect(oldPage.locator('.share-modal input[aria-label="分享链接"]')).toHaveValue(/\/s\//)
    await expect(newPage.locator('.share-modal input[aria-label="分享链接"]')).toHaveValue(/\/s\//)
    expect(await dialogSnapshot(newPage), 'Rust 分享 active 状态与 reference 不一致').toEqual(await dialogSnapshot(oldPage))
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})
