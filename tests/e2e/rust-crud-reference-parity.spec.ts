import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'
const failedFile = {
  id: 'crud-delete-failure',
  parent_id: ROOT,
  name: '删除失败.txt',
  kind: 'file',
  size: 12,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'text/plain',
  etag: 'crud-delete-failure-etag',
}
const deletedFile = {
  id: 'crud-delete-success',
  parent_id: ROOT,
  name: '删除成功.txt',
  kind: 'file',
  size: 12,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'text/plain',
  etag: 'crud-delete-success-etag',
}

async function mockDelete(page: Page) {
  let visible = [failedFile, deletedFile]
  const deleteCalls: string[] = []
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
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: visible.length } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: visible.length })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) {
      return json({ items: visible, total_bytes: visible.reduce((total, file) => total + file.size, 0), file_count: visible.length })
    }
    const match = path.match(new RegExp(`^/api/files/(${failedFile.id}|${deletedFile.id})$`))
    if (match && request.method() === 'DELETE') {
      const id = match[1]
      deleteCalls.push(id)
      if (id === failedFile.id) {
        return route.fulfill({
          status: 500,
          contentType: 'application/json',
          body: JSON.stringify({ error: { status: 500, message: 'delete failed' } }),
        })
      }
      visible = visible.filter(file => file.id !== id)
      return json({})
    }
    return json({ items: [] })
  })
  return { deleteCalls }
}

async function openDeleteFixture(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?crud-delete-reference=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.getByTitle('列表视图').click()
  for (const file of [failedFile, deletedFile]) {
    const row = page.locator('.file-row').filter({ hasText: file.name })
    await expect(row).toBeVisible()
    await row.getByRole('button', { name: '选择项目' }).click()
  }
  await expect(page.getByRole('toolbar', { name: '所选项目操作' })).toBeVisible()
}

test('多选删除部分失败时继续处理并保留 reference 反馈', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    const [oldMock, newMock] = await Promise.all([mockDelete(oldPage), mockDelete(newPage)])
    await Promise.all([openDeleteFixture(oldPage, oldUrl), openDeleteFixture(newPage, newUrl)])
    await Promise.all([
      oldPage.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '删除' }).click(),
      newPage.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '删除' }).click(),
    ])
    await Promise.all([
      oldPage.getByRole('dialog').getByRole('button', { name: '移入回收站' }).click(),
      newPage.getByRole('dialog').getByRole('button', { name: '移入回收站' }).click(),
    ])
    await expect(oldPage.locator('.toast')).toHaveText('已移入 1 项，1 项失败：删除失败.txt：delete failed')
    await expect(newPage.locator('.toast')).toHaveText('已移入 1 项，1 项失败：删除失败.txt：delete failed')
    expect(newMock.deleteCalls, 'Rust 未继续处理部分失败后的后续项目').toEqual(oldMock.deleteCalls)
    await expect(oldPage.locator('.file-row').filter({ hasText: deletedFile.name })).toHaveCount(0)
    await expect(newPage.locator('.file-row').filter({ hasText: deletedFile.name })).toHaveCount(0)
    await expect(oldPage.getByRole('toolbar', { name: '所选项目操作' })).toHaveCount(0)
    await expect(newPage.getByRole('toolbar', { name: '所选项目操作' })).toHaveCount(0)
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

async function mockRename(page: Page, failure = false) {
  const file = {
    id: 'crud-rename-file',
    parent_id: ROOT,
    name: '重命名之前.txt',
    kind: 'file',
    size: 12,
    status: 'ready',
    created_at: STAMP,
    updated_at: STAMP,
    mime_type: 'text/plain',
    etag: 'crud-rename-etag',
  }
  let current = { ...file }
  const renameValues: string[] = []
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
    if (path === `/api/files/${ROOT}/children`) return json({ items: [current], total_bytes: current.size, file_count: 1 })
    if (path === `/api/files/${file.id}` && request.method() === 'PATCH') {
      const body = request.postDataJSON() as { name?: string }
      renameValues.push(body.name ?? '')
      if (failure) {
        return route.fulfill({
          status: 409,
          contentType: 'application/json',
          body: JSON.stringify({ error: { status: 409, message: 'name already exists' } }),
        })
      }
      current = { ...current, name: body.name ?? current.name }
      return json(current)
    }
    return json({ items: [] })
  })
  return { renameValues }
}

async function openRenameFixture(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?crud-rename-reference=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.getByTitle('列表视图').click()
  const row = page.locator('.file-row').filter({ hasText: '重命名之前.txt' })
  await row.getByRole('button', { name: '选择项目' }).click()
  await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '重命名' }).click()
  const dialog = page.locator('.modal-backdrop > .modal').filter({ hasText: '重命名' })
  await expect(dialog).toBeVisible()
  return dialog
}

test('重命名保留 reference 的原始空白输入', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    const [oldMock, newMock] = await Promise.all([mockRename(oldPage), mockRename(newPage)])
    const [oldDialog, newDialog] = await Promise.all([
      openRenameFixture(oldPage, oldUrl),
      openRenameFixture(newPage, newUrl),
    ])
    const renamed = '重命名之后.txt '
    await Promise.all([
      oldDialog.locator('input').fill(renamed),
      newDialog.locator('input').fill(renamed),
    ])
    await Promise.all([
      oldDialog.getByRole('button', { name: '保存' }).click(),
      newDialog.getByRole('button', { name: '保存' }).click(),
    ])
    await Promise.all([
      expect(oldPage.locator('.toast')).toHaveText('已重命名'),
      expect(newPage.locator('.toast')).toHaveText('已重命名'),
    ])
    expect(newMock.renameValues, 'Rust 重命名不应擅自 trim reference 输入').toEqual(oldMock.renameValues)
    expect(oldMock.renameValues).toEqual([renamed])
    await expect(oldPage.locator('.file-row').filter({ hasText: renamed })).toBeVisible()
    await expect(newPage.locator('.file-row').filter({ hasText: renamed })).toBeVisible()
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

async function mockCreateFolder(page: Page) {
  const createCalls: string[] = []
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
    if (path === '/api/directories' && request.method() === 'POST') {
      const body = request.postDataJSON() as { name?: string }
      createCalls.push(body.name ?? '')
      return route.fulfill({
        status: 409,
        contentType: 'application/json',
        body: JSON.stringify({ error: { status: 409, message: 'folder already exists' } }),
      })
    }
    return json({ items: [] })
  })
  return { createCalls }
}

async function openCreateFolderFixture(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?crud-create-reference=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  const trigger = page.getByRole('button', { name: '新建文件夹', exact: true }).first()
  await trigger.click()
  return page.locator('.app-dialog')
}

test('新建文件夹空输入、取消和冲突失败保持 reference 交互', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    const mock = await mockCreateFolder(page)
    const dialog = await openCreateFolderFixture(page, baseUrl)
    const input = dialog.locator('input')
    const confirm = dialog.getByRole('button', { name: '创建', exact: true })
    await input.fill('   ')
    const emptyDisabled = await confirm.isDisabled()
    expect(emptyDisabled).toBe(true)
    await input.press('Enter')
    await expect(dialog).toBeVisible()
    expect(mock.createCalls).toEqual([])

    await dialog.getByRole('button', { name: '取消', exact: true }).click()
    await expect(dialog).toHaveCount(0)

    const reopened = await openCreateFolderFixture(page, baseUrl)
    await reopened.locator('input').fill('冲突目录')
    await reopened.getByRole('button', { name: '创建', exact: true }).click()
    await expect(reopened).toHaveCount(0)
    await expect(page.locator('.toast')).toHaveText('folder already exists')
    return { disabled: emptyDisabled, calls: mock.createCalls }
  }

  try {
    const [oldState, newState] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(newState.calls, 'Rust 新建文件夹空输入/冲突请求时序与 reference 不一致').toEqual(oldState.calls)
    expect(newState.calls).toEqual(['冲突目录'])
    expect(newState.disabled).toBe(oldState.disabled)
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('重命名冲突保留输入弹窗并恢复可重试状态', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    const mock = await mockRename(page, true)
    const dialog = await openRenameFixture(page, baseUrl)
    const value = '重命名冲突.txt'
    await dialog.locator('input').fill(value)
    await dialog.getByRole('button', { name: '保存', exact: true }).click()
    await expect(page.locator('.toast')).toHaveText('name already exists')
    await expect(dialog).toBeVisible()
    await expect(dialog.locator('input')).toHaveValue(value)
    await expect(dialog.getByRole('button', { name: '保存', exact: true })).toBeEnabled()
    return mock.renameValues
  }

  try {
    const [oldValues, newValues] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(newValues, 'Rust 重命名冲突后的输入/请求与 reference 不一致').toEqual(oldValues)
    expect(newValues).toEqual(['重命名冲突.txt'])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})
