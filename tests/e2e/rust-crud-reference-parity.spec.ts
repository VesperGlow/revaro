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
const deletedDirectory = {
  id: 'crud-delete-directory',
  parent_id: ROOT,
  name: '删除目录',
  kind: 'directory',
  size: 0,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: '',
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
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
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

async function mockDirectoryDelete(page: Page) {
  let visible = true
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
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: visible ? 1 : 0 } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: visible ? 1 : 0 })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) {
      return json({ items: visible ? [deletedDirectory] : [], total_bytes: 0, file_count: visible ? 1 : 0 })
    }
    if (path === `/api/files/${deletedDirectory.id}` && request.method() === 'DELETE') {
      deleteCalls.push(deletedDirectory.id)
      visible = false
      return route.fulfill({ status: 204, body: '' })
    }
    return json({ items: [] })
  })
  return { deleteCalls }
}

test('目录删除的取消、确认文案和成功刷新保持 reference 行为', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    const mock = await mockDirectoryDelete(page)
    await page.goto(`${baseUrl}/?crud-directory-delete-reference=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.getByRole('button', { name: '列表', exact: true }).click()
    const row = page.locator('.file-row').filter({ hasText: deletedDirectory.name })
    await expect(row).toBeVisible()
    await row.getByRole('button', { name: '选择项目' }).click()

    const toolbar = page.getByRole('toolbar', { name: '所选项目操作' })
    await toolbar.getByRole('button', { name: '删除', exact: true }).click()
    const firstDialog = page.getByRole('dialog').filter({ hasText: '移入回收站？' })
    await expect(firstDialog).toContainText('选中的 1 项会移入回收站，文件夹中的内容也会一起保留。')
    await firstDialog.getByRole('button', { name: '取消', exact: true }).click()
    await expect(firstDialog).toHaveCount(0)
    await expect(row).toBeVisible()
    expect(mock.deleteCalls).toEqual([])

    await toolbar.getByRole('button', { name: '删除', exact: true }).click()
    await page.getByRole('dialog').filter({ hasText: '移入回收站？' }).getByRole('button', { name: '移入回收站', exact: true }).click()
    await expect(page.locator('.file-row').filter({ hasText: deletedDirectory.name })).toHaveCount(0)
    await expect(page.locator('.toast')).toHaveText('已将 1 项移入回收站')
    return mock.deleteCalls
  }

  try {
    const [oldCalls, newCalls] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldCalls).toEqual([deletedDirectory.id])
    expect(newCalls, 'Rust 目录删除的取消/确认/刷新行为与 reference 不一致').toEqual(oldCalls)
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

async function mockRename(page: Page, failure = false, delayMs = 0) {
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
      if (delayMs > 0) {
        await new Promise(resolve => setTimeout(resolve, delayMs))
      }
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

async function openRenameFixture(page: Page, baseUrl: string, fileName = '重命名之前.txt') {
  await page.goto(`${baseUrl}/?crud-rename-reference=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.getByTitle('列表视图').click()
  const row = page.locator('.file-row').filter({ hasText: fileName })
  await row.getByRole('button', { name: '选择项目' }).click()
  await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '重命名' }).click()
  const dialog = page.locator('.modal-backdrop > .modal').filter({ hasText: '重命名' })
  await expect(dialog).toBeVisible()
  return dialog
}

test('重命名保留 reference 的原始空白输入', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
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

async function mockRenameBoundaries(page: Page) {
  const file = {
    id: 'crud-rename-boundary-file',
    parent_id: ROOT,
    name: '重命名边界.txt',
    kind: 'file',
    size: 12,
    status: 'ready',
    created_at: STAMP,
    updated_at: STAMP,
    mime_type: 'text/plain',
    etag: 'crud-rename-boundary-etag',
  }
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
    if (path === `/api/files/${ROOT}`) return json({ file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' }, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: [file], total_bytes: file.size, file_count: 1 })
    if (path === `/api/files/${file.id}` && request.method() === 'PATCH') {
      const body = request.postDataJSON() as { name?: string }
      renameValues.push(body.name ?? '')
      return json(file)
    }
    return json({ items: [] })
  })
  return renameValues
}

test('重命名空名、Enter、遮罩和 Escape 按 reference 保持边界语义', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    const renameValues = await mockRenameBoundaries(page)
    const first = await openRenameFixture(page, baseUrl, '重命名边界.txt')
    await first.locator('input').fill('')
    await expect(first.getByRole('button', { name: '保存', exact: true })).toBeEnabled()
    await first.getByRole('button', { name: '保存', exact: true }).click()
    await expect(first).toHaveCount(0)

    const second = await openRenameFixture(page, baseUrl, '重命名边界.txt')
    await second.locator('input').fill('Enter 重命名.txt')
    await second.locator('input').press('Enter')
    await expect(second).toHaveCount(0)

    const third = await openRenameFixture(page, baseUrl, '重命名边界.txt')
    await page.locator('.modal-backdrop').click({ position: { x: 8, y: 8 } })
    await expect(third).toHaveCount(0)

    const fourth = await openRenameFixture(page, baseUrl, '重命名边界.txt')
    await page.keyboard.press('Escape')
    const escapeCount = await page.locator('.modal-backdrop > .modal').filter({ hasText: '重命名' }).count()
    return { renameValues, escapeCount }
  }

  try {
    const [oldState, newState] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(newState, 'Rust 重命名空名/Enter/遮罩/Escape 边界与 reference 不一致').toEqual(oldState)
    expect(oldState).toEqual({ renameValues: ['', 'Enter 重命名.txt'], escapeCount: 1 })
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('重命名保存进行中仍允许按 reference 关闭弹窗', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockRename(page, false, 800)
    const dialog = await openRenameFixture(page, baseUrl)
    await dialog.locator('input').fill('重命名进行中.txt')
    const requestStarted = page.waitForRequest(request => {
      const path = new URL(request.url()).pathname
      return path === `/api/files/crud-rename-file` && request.method() === 'PATCH'
    })
    await dialog.getByRole('button', { name: '保存', exact: true }).click()
    await requestStarted
    await expect(dialog.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
    await dialog.locator('header button').click()
    await page.waitForTimeout(50)
    return page.locator('.modal-backdrop > .modal').filter({ hasText: '重命名' }).count()
  }

  try {
    const [oldCount, newCount] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(newCount, 'Rust 重命名请求进行中不应阻止 reference 的关闭操作').toBe(oldCount)
    expect(oldCount).toBe(0)
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
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    const mock = await mockCreateFolder(page)
    const dialog = await openCreateFolderFixture(page, baseUrl)
    const input = dialog.locator('input')
    const confirm = dialog.getByRole('button', { name: '创建', exact: true })
    await expect(input).toBeFocused()
    const inputFocused = await input.evaluate(element => document.activeElement === element)
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
    return { disabled: emptyDisabled, inputFocused, calls: mock.createCalls }
  }

  try {
    const [oldState, newState] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(newState.calls, 'Rust 新建文件夹空输入/冲突请求时序与 reference 不一致').toEqual(oldState.calls)
    expect(newState.calls).toEqual(['冲突目录'])
    expect(newState.disabled).toBe(oldState.disabled)
    expect(oldState.inputFocused).toBe(true)
    expect(newState.inputFocused, 'Rust 新建文件夹打开后输入框焦点与 reference 不一致').toBe(oldState.inputFocused)
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

async function mockCreateFolderSuccess(page: Page) {
  let created: { id: string; parent_id: string; name: string; kind: string; size: number; status: string; created_at: string; updated_at: string; mime_type: string } | null = null
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
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: created ? 1 : 0 } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: created ? 1 : 0 })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) {
      return json({ items: created ? [created] : [], total_bytes: 0, file_count: created ? 1 : 0 })
    }
    if (path === '/api/directories' && request.method() === 'POST') {
      const body = request.postDataJSON() as { name?: string; parent_id?: string }
      createCalls.push(body.name ?? '')
      created = {
        id: 'crud-dialog-enter-folder',
        parent_id: body.parent_id ?? ROOT,
        name: body.name ?? '',
        kind: 'directory',
        size: 0,
        status: 'ready',
        created_at: STAMP,
        updated_at: STAMP,
        mime_type: '',
      }
      return route.fulfill({ status: 201, json: created })
    }
    return json({ items: [] })
  })
  return { createCalls }
}

test('新建文件夹有效输入支持 Enter，遮罩与 Escape 取消保持 reference 行为', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    const mock = await mockCreateFolderSuccess(page)
    await page.goto(`${baseUrl}/?crud-create-keyboard-reference=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()

    await page.getByRole('button', { name: '新建文件夹', exact: true }).first().click()
    const dialog = page.locator('.app-dialog')
    await dialog.locator('input').fill('Enter 创建目录')
    await dialog.locator('input').press('Enter')
    await expect(dialog).toHaveCount(0)
    await expect(page.locator('.file-card, .file-row').filter({ hasText: 'Enter 创建目录' })).toBeVisible()

    await page.getByRole('button', { name: '新建文件夹', exact: true }).first().click()
    await expect(page.locator('.app-dialog')).toBeVisible()
    await page.locator('.dialog-backdrop').click({ position: { x: 8, y: 8 } })
    await expect(page.locator('.app-dialog')).toHaveCount(0)

    await page.getByRole('button', { name: '新建文件夹', exact: true }).first().click()
    await expect(page.locator('.app-dialog')).toBeVisible()
    await page.keyboard.press('Escape')
    await expect(page.locator('.app-dialog')).toHaveCount(0)
    return mock.createCalls
  }

  try {
    const [oldCalls, newCalls] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(newCalls, 'Rust 新建文件夹 Enter/遮罩/Escape 行为与 reference 不一致').toEqual(oldCalls)
    expect(oldCalls).toEqual(['Enter 创建目录'])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('重命名冲突保留输入弹窗并恢复可重试状态', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
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

const trashFile = {
  id: 'crud-trash-failure',
  parent_id: ROOT,
  name: '回收站失败.txt',
  kind: 'file',
  size: 12,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  deleted_at: STAMP,
  mime_type: 'text/plain',
  etag: 'crud-trash-failure-etag',
}

async function mockTrashFailure(page: Page, action: 'restore' | 'purge') {
  const calls: string[] = []
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
    if (path === '/api/trash') return json({ items: [trashFile], total_bytes: trashFile.size, file_count: 1 })
    if (path === `/api/trash/${trashFile.id}/restore` && request.method() === 'POST' && action === 'restore') {
      calls.push('restore')
      return route.fulfill({
        status: 409,
        contentType: 'application/json',
        body: JSON.stringify({ error: { status: 409, message: 'restore conflict' } }),
      })
    }
    if (path === `/api/trash/${trashFile.id}` && request.method() === 'DELETE' && action === 'purge') {
      calls.push('purge')
      return route.fulfill({
        status: 409,
        contentType: 'application/json',
        body: JSON.stringify({ error: { status: 409, message: 'purge conflict' } }),
      })
    }
    return json({ items: [] })
  })
  return { calls }
}

async function openTrashFailureFixture(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?crud-trash-reference=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.getByTitle('列表视图').click()
  await page.getByTitle('回收站').first().click()
  await expect(page.getByRole('heading', { name: '回收站', exact: true })).toBeVisible()
  const row = page.locator('.file-row').filter({ hasText: trashFile.name })
  await expect(row).toBeVisible()
  await row.getByRole('button', { name: '选择项目' }).click()
  return page.getByRole('toolbar', { name: '所选项目操作' })
}

test('回收站恢复冲突保留项目与选择状态', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    const mock = await mockTrashFailure(page, 'restore')
    const toolbar = await openTrashFailureFixture(page, baseUrl)
    await toolbar.getByRole('button', { name: '恢复', exact: true }).click()
    await expect(page.locator('.toast')).toHaveText('回收站失败.txt：restore conflict')
    await expect(page.locator('.file-row').filter({ hasText: trashFile.name })).toBeVisible()
    await expect(page.getByRole('toolbar', { name: '所选项目操作' })).toBeVisible()
    return mock.calls
  }

  try {
    const [oldCalls, newCalls] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(newCalls, 'Rust 恢复冲突后的项目/选择状态与 reference 不一致').toEqual(oldCalls)
    expect(newCalls).toEqual(['restore'])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('回收站永久删除冲突关闭确认框并保留项目', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    const mock = await mockTrashFailure(page, 'purge')
    const toolbar = await openTrashFailureFixture(page, baseUrl)
    await toolbar.getByRole('button', { name: '永久删除', exact: true }).click()
    const dialog = page.locator('.app-dialog')
    await expect(dialog).toBeVisible()
    await dialog.getByRole('button', { name: '永久删除', exact: true }).click()
    await expect(dialog).toHaveCount(0)
    await expect(page.locator('.toast')).toHaveText('回收站失败.txt：purge conflict')
    await expect(page.locator('.file-row').filter({ hasText: trashFile.name })).toBeVisible()
    await expect(page.getByRole('toolbar', { name: '所选项目操作' })).toBeVisible()
    return mock.calls
  }

  try {
    const [oldCalls, newCalls] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(newCalls, 'Rust 永久删除冲突后的确认框/项目状态与 reference 不一致').toEqual(oldCalls)
    expect(newCalls).toEqual(['purge'])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('回收站清空的取消与失败结果保持 reference 交互', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const item = {
    id: 'crud-empty-trash-failure',
    parent_id: ROOT,
    name: '清空失败.txt',
    kind: 'file',
    size: 12,
    status: 'ready',
    created_at: STAMP,
    updated_at: STAMP,
    deleted_at: STAMP,
    mime_type: 'text/plain',
    etag: 'crud-empty-trash-failure-etag',
  }

  async function mock(page: Page) {
    const calls: string[] = []
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
      if (path === '/api/trash' && request.method() === 'GET') return json({ items: [item], total_bytes: item.size, file_count: 1 })
      if (path === '/api/trash' && request.method() === 'DELETE') {
        calls.push('empty')
        return route.fulfill({
          status: 500,
          contentType: 'application/json',
          body: JSON.stringify({ error: { status: 500, message: 'empty trash failed' } }),
        })
      }
      return json({ items: [] })
    })
    return { calls }
  }

  async function exercise(page: Page, baseUrl: string) {
    const mockState = await mock(page)
    await page.goto(`${baseUrl}/?crud-empty-trash-reference=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.getByTitle('回收站').first().click()
    await expect(page.getByRole('heading', { name: '回收站', exact: true })).toBeVisible()
    const empty = page.getByRole('button', { name: '清空回收站', exact: true })
    await expect(empty).toBeEnabled()

    await empty.click()
    const dialog = page.getByRole('dialog')
    await expect(dialog).toBeVisible()
    await expect(dialog).toContainText('回收站中的 1 项及其内容都会永久删除，无法恢复。')
    await dialog.getByRole('button', { name: '取消', exact: true }).click()
    await expect(dialog).toHaveCount(0)
    expect(mockState.calls).toEqual([])

    await empty.click()
    await page.getByRole('dialog').getByRole('button', { name: '清空回收站', exact: true }).click()
    await expect(page.getByRole('dialog')).toHaveCount(0)
    await expect(page.locator('.toast')).toHaveText('empty trash failed')
    await expect(page.locator('.file-card, .file-row').filter({ hasText: item.name })).toBeVisible()
    await expect(empty).toBeEnabled()
    return mockState.calls
  }

  try {
    const [oldCalls, newCalls] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldCalls).toEqual(['empty'])
    expect(newCalls, 'Rust 清空回收站失败后的确认框/列表/反馈与 reference 不一致').toEqual(oldCalls)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new CRUD、文档和上传入口保留 reference 的非法名称校验', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const invalidNames = [
    '',
    '.',
    '..',
    ' leading',
    'trailing ',
    'a/b',
    'a\\b',
    'a\u0001b',
    'a\u007fb',
    'a'.repeat(256),
  ]

  async function exercise(page: Page, baseUrl: string) {
    const folderName = `crud-validation-${crypto.randomUUID()}`
    await page.goto(`${baseUrl}/?crud-validation-reference=${Date.now()}`)
    await page.evaluate(async () => {
      const response = await fetch('/api/auth/login', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username: 'admin', password: 'revaro-e2e-password' }),
      })
      if (!response.ok) throw new Error(`login failed: ${response.status}`)
    })
    const valid = await page.evaluate(async ({ folderName, root }) => {
      const response = await fetch('/api/directories', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ parent_id: root, name: folderName }),
      })
      return { status: response.status, body: await response.json() as { id?: string } }
    }, { folderName, root: ROOT })
    expect(valid.status).toBe(201)
    const folderId = valid.body.id
    if (!folderId) throw new Error('validation fixture folder was not created')

    const results = await page.evaluate(async ({ folderId: id, invalidNames: names, root }) => {
      async function request(path: string, method: string, body: unknown) {
        const response = await fetch(path, {
          method,
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(body),
        })
        const text = await response.text()
        let parsed: unknown = text
        try {
          parsed = JSON.parse(text)
        } catch {
          // Preserve a non-JSON body as text for diagnostics.
        }
        return { status: response.status, body: parsed }
      }
      const rows: Array<{ name: string; directory: { status: number; body: unknown }; rename: { status: number; body: unknown }; document: { status: number; body: unknown }; upload: { status: number; body: unknown } }> = []
      for (const name of names) {
        rows.push({
          name,
          directory: await request('/api/directories', 'POST', { parent_id: root, name }),
          rename: await request(`/api/files/${id}`, 'PATCH', { name }),
          document: await request('/api/documents', 'POST', { parent_id: root, name, content: 'validation' }),
          upload: await request('/api/uploads', 'POST', { parent_id: root, name, size: 0, mime_type: 'text/plain' }),
        })
      }
      return rows
    }, { folderId, invalidNames, root: ROOT })
    await page.evaluate(async id => {
      const headers = { 'Content-Type': 'application/json' }
      await fetch(`/api/files/${id}`, { method: 'DELETE', headers })
      await fetch(`/api/trash/${id}`, { method: 'DELETE', headers })
    }, folderId)
    return results
  }

  try {
    const [oldResults, newResults] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(newResults, 'Rust CRUD/文档/上传非法名称校验与 reference 不一致').toEqual(oldResults)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})
