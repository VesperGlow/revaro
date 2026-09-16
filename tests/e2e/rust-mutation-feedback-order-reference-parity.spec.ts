import { expect, test, type BrowserContext, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'
const folder = {
  id: 'mutation-order-folder',
  parent_id: ROOT,
  name: '时序目录',
  kind: 'directory',
  size: 0,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: '',
}
const root = {
  id: ROOT,
  parent_id: null,
  name: '我的文件',
  kind: 'directory',
  size: 0,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: '',
}

async function mockCreateFolder(page: Page, options: { emptyResponse?: boolean } = {}) {
  let created = false
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
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) {
      if (created) await new Promise(resolve => setTimeout(resolve, 700))
      return json({ items: created ? [folder] : [], total_bytes: 0, file_count: created ? 1 : 0 })
    }
    if (path === '/api/directories' && request.method() === 'POST') {
      created = true
      return route.fulfill({ status: 201, json: options.emptyResponse ? {} : folder })
    }
    return json({ items: [] })
  })
}

async function clearPreferences(context: BrowserContext) {
  await context.addInitScript(() => {
    localStorage.removeItem('revaro:sidebar:collapsed')
    localStorage.removeItem('revaro:sidebar:expanded')
    localStorage.removeItem('revaro:view-mode')
  })
}

async function exercise(page: Page, url: string) {
  await page.goto(`${url}/?mutation-feedback-order=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '新建文件夹', exact: true }).first().click()
  const dialog = page.locator('.app-dialog')
  await dialog.locator('input').fill(folder.name)
  await dialog.getByRole('button', { name: '创建', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  await page.waitForTimeout(180)
  const toast = page.locator('.toast')
  const toastCount = await toast.count()
  const beforeRefresh = {
    toast: toastCount ? await toast.textContent() : null,
    folderVisible: await page.locator('.file-card, .file-row').filter({ hasText: folder.name }).count(),
  }
  await expect(page.locator('.file-card, .file-row').filter({ hasText: folder.name })).toBeVisible()
  return { beforeRefresh, afterRefresh: await page.locator('.toast').textContent() }
}

test('新建目录成功反馈等待 reference 的目录刷新完成', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  await clearPreferences(oldContext)
  await clearPreferences(newContext)
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockCreateFolder(oldPage), mockCreateFolder(newPage)])
    const [oldState, newState] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldState.beforeRefresh, 'reference 应在目录刷新完成前不显示成功 Toast').toEqual({ toast: null, folderVisible: 0 })
    expect(newState, 'Rust 成功反馈的刷新时序与 reference 不一致').toEqual(oldState)
    expect(oldState.afterRefresh).toBe('文件夹已创建')
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 新建目录成功响应缺少文件字段时仍保留成功反馈', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  await clearPreferences(oldContext)
  await clearPreferences(newContext)
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([
      mockCreateFolder(oldPage, { emptyResponse: true }),
      mockCreateFolder(newPage, { emptyResponse: true }),
    ])
    const [oldState, newState] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldState.beforeRefresh, 'reference 应在目录刷新完成前不显示成功 Toast').toEqual({ toast: null, folderVisible: 0 })
    expect(newState, 'Rust 新建目录成功响应缺字段时未保留 reference 反馈').toEqual(oldState)
    expect(oldState.afterRefresh).toBe('文件夹已创建')
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

const mutableFile = {
  id: 'mutation-order-file',
  parent_id: ROOT,
  name: '待变更.txt',
  kind: 'file',
  size: 12,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'text/plain',
}

async function mockDelete(page: Page) {
  let deleted = false
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
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) {
      if (deleted) await new Promise(resolve => setTimeout(resolve, 700))
      return json({ items: deleted ? [] : [mutableFile], total_bytes: deleted ? 0 : mutableFile.size, file_count: deleted ? 0 : 1 })
    }
    if (path === `/api/files/${mutableFile.id}` && request.method() === 'DELETE') {
      deleted = true
      return json({})
    }
    return json({ items: [] })
  })
}

async function mockTransferOrder(page: Page, options: { emptyResponse?: boolean } = {}) {
  let moved = false
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
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) {
      if (moved) await new Promise(resolve => setTimeout(resolve, 700))
      return json({ items: moved ? [] : [mutableFile], total_bytes: moved ? 0 : mutableFile.size, file_count: moved ? 0 : 1 })
    }
    if (path === `/api/files/${mutableFile.id}` && request.method() === 'PATCH') {
      moved = true
      return json(options.emptyResponse ? {} : { ...mutableFile })
    }
    return json({ items: [] })
  })
}

async function mockTrashMutation(page: Page, action: 'restore' | 'purge' | 'empty') {
  let mutated = false
  const trashFile = { ...mutableFile, id: `mutation-order-trash-${action}`, name: `待${action}回收站.txt`, deleted_at: STAMP }
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
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    if (path === '/api/trash' && request.method() === 'GET') {
      if (mutated) await new Promise(resolve => setTimeout(resolve, 700))
      return json({ items: mutated ? [] : [trashFile], total_bytes: mutated ? 0 : trashFile.size, file_count: mutated ? 0 : 1 })
    }
    if (action === 'restore' && path === `/api/trash/${trashFile.id}/restore` && request.method() === 'POST') {
      mutated = true
      return json({})
    }
    if (action === 'purge' && path === `/api/trash/${trashFile.id}` && request.method() === 'DELETE') {
      mutated = true
      return json({})
    }
    if (action === 'empty' && path === '/api/trash' && request.method() === 'DELETE') {
      mutated = true
      return json({})
    }
    return json({ items: [] })
  })
  return trashFile
}

async function sampleMutationState(page: Page, name: string) {
  const toast = page.locator('.toast')
  const toastCount = await toast.count()
  return {
    toast: toastCount ? await toast.textContent() : null,
    visible: await page.locator('.file-card, .file-row').filter({ hasText: name }).count(),
  }
}

test('删除成功反馈等待 reference 的目录刷新完成', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  await clearPreferences(oldContext)
  await clearPreferences(newContext)
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, url: string) {
    await page.goto(`${url}/?mutation-delete-order=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.getByRole('button', { name: '列表', exact: true }).click()
    const row = page.locator('.file-row').filter({ hasText: mutableFile.name })
    await row.getByRole('button', { name: '选择项目' }).click()
    await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '删除', exact: true }).click()
    await page.getByRole('dialog').getByRole('button', { name: '移入回收站', exact: true }).click()
    await expect(page.getByRole('dialog')).toHaveCount(0)
    await page.waitForTimeout(180)
    const before = await sampleMutationState(page, mutableFile.name)
    await expect(page.locator('.toast')).toHaveText('已将 1 项移入回收站')
    return { before, after: await sampleMutationState(page, mutableFile.name) }
  }

  try {
    await Promise.all([mockDelete(oldPage), mockDelete(newPage)])
    const [oldState, newState] = await Promise.all([exercise(oldPage, oldUrl), exercise(newPage, newUrl)])
    expect(oldState.before.toast).toBeNull()
    expect(newState, 'Rust 删除成功反馈的刷新时序与 reference 不一致').toEqual(oldState)
    expect(oldState.after).toEqual({ toast: '已将 1 项移入回收站', visible: 0 })
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('移动成功反馈等待 reference 的目录刷新完成', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  await clearPreferences(oldContext)
  await clearPreferences(newContext)
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, url: string) {
    await mockTransferOrder(page)
    await page.goto(`${url}/?mutation-transfer-order=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.getByTitle('列表视图').click()
    const row = page.locator('.file-row').filter({ hasText: mutableFile.name })
    await row.getByRole('button', { name: '选择项目' }).click()
    await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '移动', exact: true }).click()
    const dialog = page.locator('.move-copy-dialog')
    await dialog.getByRole('button', { name: '移动', exact: true }).click()
    await expect(dialog).toHaveCount(0)
    await page.waitForTimeout(180)
    const before = await sampleMutationState(page, mutableFile.name)
    await expect(page.locator('.toast')).toHaveText('已移动 1 项')
    return { before, after: await sampleMutationState(page, mutableFile.name) }
  }

  try {
    const [oldState, newState] = await Promise.all([exercise(oldPage, oldUrl), exercise(newPage, newUrl)])
    expect(oldState.before.toast).toBeNull()
    expect(newState, 'Rust 移动成功反馈的刷新时序与 reference 不一致').toEqual(oldState)
    expect(oldState.after).toEqual({ toast: '已移动 1 项', visible: 0 })
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 移动成功响应缺少文件字段时仍保留成功反馈', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, url: string) {
    await mockTransferOrder(page, { emptyResponse: true })
    await page.goto(`${url}/?mutation-transfer-empty-response=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.getByTitle('列表视图').click()
    const row = page.locator('.file-row').filter({ hasText: mutableFile.name })
    await row.getByRole('button', { name: '选择项目' }).click()
    await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '移动', exact: true }).click()
    const dialog = page.locator('.move-copy-dialog')
    await dialog.getByRole('button', { name: '移动', exact: true }).click()
    await expect(dialog).toHaveCount(0)
    await expect(page.locator('.toast')).toHaveText('已移动 1 项')
    return {
      toast: await page.locator('.toast').textContent(),
      visible: await page.locator('.file-card, .file-row').filter({ hasText: mutableFile.name }).count(),
    }
  }

  try {
    const [oldState, newState] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldState).toEqual({ toast: '已移动 1 项', visible: 0 })
    expect(newState, 'Rust 移动成功响应缺字段时未保留 reference 反馈').toEqual(oldState)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

async function mockRename(page: Page, options: { emptyResponse?: boolean } = {}) {
  let renamed = false
  const nextName = '已重命名.txt'
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
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) {
      if (renamed) await new Promise(resolve => setTimeout(resolve, 700))
      return json({ items: [{ ...mutableFile, name: renamed ? nextName : mutableFile.name }], total_bytes: mutableFile.size, file_count: 1 })
    }
    if (path === `/api/files/${mutableFile.id}` && request.method() === 'PATCH') {
      renamed = true
      return json(options.emptyResponse ? {} : { ...mutableFile, name: nextName })
    }
    return json({ items: [] })
  })
  return nextName
}

test('重命名成功反馈等待 reference 的目录刷新完成', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  await clearPreferences(oldContext)
  await clearPreferences(newContext)
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, url: string) {
    const nextName = await mockRename(page)
    await page.goto(`${url}/?mutation-rename-order=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.getByTitle('列表视图').click()
    const row = page.locator('.file-row').filter({ hasText: mutableFile.name })
    await row.getByRole('button', { name: '选择项目' }).click()
    await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '重命名', exact: true }).click()
    const dialog = page.locator('.modal-backdrop > .modal').filter({ hasText: '重命名' })
    await dialog.locator('input').fill(nextName)
    await dialog.getByRole('button', { name: '保存', exact: true }).click()
    await expect(dialog).toHaveCount(0)
    await page.waitForTimeout(180)
    const before = await sampleMutationState(page, nextName)
    await expect(page.locator('.toast')).toHaveText('已重命名')
    return { before, after: await sampleMutationState(page, nextName) }
  }

  try {
    const [oldState, newState] = await Promise.all([exercise(oldPage, oldUrl), exercise(newPage, newUrl)])
    expect(oldState.before.toast).toBeNull()
    expect(newState, 'Rust 重命名成功反馈的刷新时序与 reference 不一致').toEqual(oldState)
    expect(oldState.after).toEqual({ toast: '已重命名', visible: 1 })
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 重命名成功响应缺少文件字段时仍保留成功反馈', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, url: string) {
    const nextName = await mockRename(page, { emptyResponse: true })
    await page.goto(`${url}/?mutation-rename-empty-response=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.getByTitle('列表视图').click()
    const row = page.locator('.file-row').filter({ hasText: mutableFile.name })
    await row.getByRole('button', { name: '选择项目' }).click()
    await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '重命名', exact: true }).click()
    const dialog = page.locator('.modal-backdrop > .modal').filter({ hasText: '重命名' })
    await dialog.locator('input').fill(nextName)
    await dialog.getByRole('button', { name: '保存', exact: true }).click()
    await expect(dialog).toHaveCount(0)
    await expect(page.locator('.toast')).toHaveText('已重命名')
    return {
      toast: await page.locator('.toast').textContent(),
      visible: await page.locator('.file-card, .file-row').filter({ hasText: nextName }).count(),
    }
  }

  try {
    const [oldState, newState] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldState).toEqual({ toast: '已重命名', visible: 1 })
    expect(newState, 'Rust 重命名成功响应缺字段时未保留 reference 反馈').toEqual(oldState)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

for (const action of ['restore', 'purge', 'empty'] as const) {
  test(`${action} 成功反馈等待 reference 的回收站刷新完成`, async ({ browser }) => {
    const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
    const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
    const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
    const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
    await clearPreferences(oldContext)
    await clearPreferences(newContext)
    const oldPage = await oldContext.newPage()
    const newPage = await newContext.newPage()

    async function exercise(page: Page, url: string) {
      const trashFile = await mockTrashMutation(page, action)
      await page.goto(`${url}/?mutation-${action}-order=${Date.now()}`)
      await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
      // Trash has no own view switch. The reference uses the persisted file
      // view, so enter list mode before opening the trash page.
      await page.getByTitle('列表视图').click()
      await page.locator('.app-sidebar .trash-entry').click()
      await expect(page.getByRole('heading', { name: '回收站', exact: true })).toBeVisible()
      if (action === 'empty') {
        await page.getByRole('button', { name: '清空回收站', exact: true }).click()
        await page.getByRole('dialog').getByRole('button', { name: '清空回收站', exact: true }).click()
      } else {
        const row = page.locator('.file-row').filter({ hasText: trashFile.name })
        await row.getByRole('button', { name: '选择项目' }).click()
        const toolbar = page.getByRole('toolbar', { name: '所选项目操作' })
        if (action === 'restore') {
          await toolbar.getByRole('button', { name: '恢复', exact: true }).click()
        } else {
          await toolbar.getByRole('button', { name: '永久删除', exact: true }).click()
          await page.getByRole('dialog').getByRole('button', { name: '永久删除', exact: true }).click()
        }
      }
      await expect(page.getByRole('dialog')).toHaveCount(0)
      await page.waitForTimeout(180)
      const before = await sampleMutationState(page, trashFile.name)
      const selectionToolbarDuringRefresh = await page.getByRole('toolbar', { name: '所选项目操作' }).count()
      const success = action === 'restore' ? '所选项目已恢复' : action === 'purge' ? '已永久删除所选项目' : '回收站已清空'
      await expect(page.locator('.toast')).toHaveText(success)
      return { before, selectionToolbarDuringRefresh, after: await sampleMutationState(page, trashFile.name) }
    }

    try {
      const [oldState, newState] = await Promise.all([exercise(oldPage, oldUrl), exercise(newPage, newUrl)])
      const success = action === 'restore' ? '所选项目已恢复' : action === 'purge' ? '已永久删除所选项目' : '回收站已清空'
      expect(oldState.before.toast).toBeNull()
      expect(newState, `Rust ${action} 成功反馈的刷新时序与 reference 不一致`).toEqual(oldState)
      expect(oldState.selectionToolbarDuringRefresh).toBe(action === 'empty' ? 0 : 1)
      expect(oldState.after).toEqual({ toast: success, visible: 0 })
    } finally {
      await Promise.all([oldContext.close(), newContext.close()])
    }
  })
}
