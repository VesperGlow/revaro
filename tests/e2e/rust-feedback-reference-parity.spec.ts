import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

const files = ['toast-one.txt', 'toast-two.txt'].map((name, index) => ({
  id: `toast-file-${index + 1}`,
  parent_id: ROOT,
  name,
  kind: 'file',
  size: 12,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'text/plain',
  etag: `etag-${index + 1}`,
}))

async function mockFeedback(page: Page, options: { directoryError?: boolean; directoryForbidden?: boolean; directoryNetworkError?: boolean; directorySequenceError?: boolean } = {}) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })

    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/directories' && options.directoryNetworkError) return route.abort('failed')
    if (path === '/api/directories' && options.directoryForbidden) {
      return route.fulfill({
        status: 403,
        contentType: 'application/json',
        body: JSON.stringify({ error: { status: 403, message: 'permission denied' } }),
      })
    }
    if (path === '/api/directories' && options.directorySequenceError) {
      const body = route.request().postDataJSON() as { name?: string }
      const message = body.name?.includes('second') ? 'second failure' : 'first failure'
      return route.fulfill({
        status: 409,
        contentType: 'application/json',
        body: JSON.stringify({ error: { status: 409, message } }),
      })
    }
    if (path === '/api/directories' && options.directoryError) {
      return route.fulfill({
        status: 409,
        contentType: 'application/json',
        body: JSON.stringify({ error: { status: 409, message: 'folder already exists' } }),
      })
    }
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: files.length } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: files.length })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: files, total_bytes: 24, file_count: files.length })
    if (path === '/api/files/batch-download/prepare') {
      await new Promise(resolve => setTimeout(resolve, 600))
      return json({ token: 'feedback-parity-token' })
    }
    return json({ items: [] })
  })
}

async function openFeedbackFixture(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '列表', exact: true }).click()
  for (const file of files) {
    const row = page.locator('.file-row').filter({ hasText: file.name })
    await expect(row).toBeVisible()
    await row.getByRole('button', { name: '选择项目' }).click()
  }
  await expect(page.getByRole('toolbar', { name: '所选项目操作' })).toBeVisible()
}

async function toastMetrics(page: Page) {
  return page.locator('.toast').evaluate(element => {
    const style = getComputedStyle(element)
    return {
      pointerEvents: style.pointerEvents,
      position: style.position,
      bottom: style.bottom,
      transform: style.transform,
      background: style.backgroundColor,
      color: style.color,
      padding: style.padding,
      borderRadius: style.borderRadius,
      fontSize: style.fontSize,
      className: element.getAttribute('class'),
      role: element.getAttribute('role'),
    }
  })
}

async function clickToast(page: Page) {
  const box = await page.locator('.toast').boundingBox()
  if (!box) throw new Error('toast did not have a box')
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2)
}

test('全局 toast 的命中区域和最新通知交互保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockFeedback(oldPage), mockFeedback(newPage)])
    await Promise.all([
      openFeedbackFixture(oldPage, oldUrl),
      openFeedbackFixture(newPage, newUrl),
    ])
    await Promise.all([
      oldPage.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '下载 (2)', exact: true }).click(),
      newPage.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '下载 (2)', exact: true }).click(),
    ])
    await expect(oldPage.locator('.toast')).toHaveText('正在准备 2 个文件…')
    await expect(newPage.locator('.toast')).toHaveText('正在准备 2 个文件…')
    expect(await toastMetrics(newPage), 'Rust toast 的 CSS 命中区域与 reference 不一致')
      .toEqual(await toastMetrics(oldPage))

    await Promise.all([clickToast(oldPage), clickToast(newPage)])
    await expect(oldPage.getByRole('toolbar', { name: '所选项目操作' })).toBeVisible()
    await expect(newPage.getByRole('toolbar', { name: '所选项目操作' })).toBeVisible()
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('错误 toast 的 class、文案和关闭时限保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function trigger(page: Page, baseUrl: string) {
    await page.goto(`${baseUrl}/?toast-error-parity=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.getByRole('button', { name: '新建文件夹', exact: true }).first().click()
    const dialog = page.locator('.app-dialog')
    await dialog.locator('input').fill(`toast-error-${Date.now()}`)
    await dialog.getByRole('button', { name: '创建', exact: true }).click()
    await expect(dialog).toHaveCount(0)
    const toast = page.locator('.toast')
    await expect(toast).toHaveText('folder already exists')
    return toastMetrics(page)
  }

  try {
    await Promise.all([
      mockFeedback(oldPage, { directoryError: true }),
      mockFeedback(newPage, { directoryError: true }),
    ])
    const [oldToast, newToast] = await Promise.all([
      trigger(oldPage, oldUrl),
      trigger(newPage, newUrl),
    ])
    expect(newToast, 'Rust error toast 的 DOM/CSS 语义与 reference 不一致').toEqual(oldToast)
    await Promise.all([
      expect(oldPage.locator('.toast')).toHaveCount(0, { timeout: 5_000 }),
      expect(newPage.locator('.toast')).toHaveCount(0, { timeout: 5_000 }),
    ])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('网络失败 toast 的 transport 文案和视觉语义保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function trigger(page: Page, baseUrl: string) {
    await page.goto(`${baseUrl}/?toast-network-parity=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.getByRole('button', { name: '新建文件夹', exact: true }).first().click()
    const dialog = page.locator('.app-dialog')
    await dialog.locator('input').fill(`toast-network-${Date.now()}`)
    await dialog.getByRole('button', { name: '创建', exact: true }).click()
    await expect(dialog).toHaveCount(0)
    const toast = page.locator('.toast')
    await expect(toast).toBeVisible()
    return { text: await toast.innerText(), metrics: await toastMetrics(page) }
  }

  try {
    await Promise.all([
      mockFeedback(oldPage, { directoryNetworkError: true }),
      mockFeedback(newPage, { directoryNetworkError: true }),
    ])
    const [oldToast, newToast] = await Promise.all([
      trigger(oldPage, oldUrl),
      trigger(newPage, newUrl),
    ])
    expect(newToast, 'Rust network error toast 与 reference 不一致').toEqual(oldToast)
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('403 权限错误 toast 的文案、class 和时限保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function trigger(page: Page, baseUrl: string) {
    await page.goto(`${baseUrl}/?toast-forbidden-parity=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.getByRole('button', { name: '新建文件夹', exact: true }).first().click()
    const dialog = page.locator('.app-dialog')
    await dialog.locator('input').fill(`toast-forbidden-${Date.now()}`)
    await dialog.getByRole('button', { name: '创建', exact: true }).click()
    await expect(dialog).toHaveCount(0)
    const toast = page.locator('.toast')
    await expect(toast).toHaveText('permission denied')
    return toastMetrics(page)
  }

  try {
    await Promise.all([
      mockFeedback(oldPage, { directoryForbidden: true }),
      mockFeedback(newPage, { directoryForbidden: true }),
    ])
    const [oldToast, newToast] = await Promise.all([
      trigger(oldPage, oldUrl),
      trigger(newPage, newUrl),
    ])
    expect(newToast, 'Rust 403 error toast 与 reference 不一致').toEqual(oldToast)
    await Promise.all([
      expect(oldPage.locator('.toast')).toHaveCount(0, { timeout: 5_000 }),
      expect(newPage.locator('.toast')).toHaveCount(0, { timeout: 5_000 }),
    ])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('连续通知只保留最新内容并从替换时刻重新计时', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function trigger(page: Page, baseUrl: string) {
    await page.goto(`${baseUrl}/?toast-sequence-parity=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.getByRole('button', { name: '新建文件夹', exact: true }).first().click()
    const dialog = page.locator('.app-dialog')
    await dialog.locator('input').fill('toast-first')
    await dialog.getByRole('button', { name: '创建', exact: true }).click()
    await expect(page.locator('.toast')).toHaveText('first failure')
    await page.waitForTimeout(2_500)
    await page.getByRole('button', { name: '新建文件夹', exact: true }).first().click()
    await dialog.locator('input').fill('toast-second')
    await dialog.getByRole('button', { name: '创建', exact: true }).click()
    await expect(page.locator('.toast')).toHaveText('second failure')
  }

  try {
    await Promise.all([
      mockFeedback(oldPage, { directorySequenceError: true }),
      mockFeedback(newPage, { directorySequenceError: true }),
      trigger(oldPage, oldUrl),
      trigger(newPage, newUrl),
    ])
    await Promise.all([
      oldPage.waitForTimeout(1_500),
      newPage.waitForTimeout(1_500),
    ])
    await expect(oldPage.locator('.toast')).toHaveText('second failure')
    await expect(newPage.locator('.toast')).toHaveText('second failure')
    expect(await newPage.locator('.toast').count(), 'Rust 连续通知不应堆叠多个 Toast').toBe(await oldPage.locator('.toast').count())
    await Promise.all([
      expect(oldPage.locator('.toast')).toHaveCount(0, { timeout: 3_000 }),
      expect(newPage.locator('.toast')).toHaveCount(0, { timeout: 3_000 }),
    ])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('401 会话过期保留 reference 结果并明确记录 Rust 安全强化', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const folder = {
    id: 'feedback-expired-folder',
    parent_id: ROOT,
    name: '会话过期目录',
    kind: 'directory',
    size: 0,
    status: 'ready',
    created_at: STAMP,
    updated_at: STAMP,
    mime_type: '',
  }

  async function mockExpired(page: Page) {
    await page.route('**/api/**', async route => {
      const path = new URL(route.request().url()).pathname
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
      if (path === `/api/files/${ROOT}/children`) return json({ items: [folder], total_bytes: 0, file_count: 1 })
      if (path === `/api/files/${folder.id}` || path === `/api/files/${folder.id}/children`) {
        return route.fulfill({
          status: 401,
          contentType: 'application/json',
          body: JSON.stringify({ error: { status: 401, message: 'session expired' } }),
        })
      }
      return json({ items: [] })
    })
  }

  async function exercise(page: Page, baseUrl: string) {
    await mockExpired(page)
    await page.goto(`${baseUrl}/?toast-session-expired=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.locator('.file-card, .file-row').filter({ hasText: folder.name }).click()
  }

  try {
    await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    await expect(oldPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await expect(oldPage.locator('.toast')).toHaveText('session expired')
    await expect(newPage.getByLabel('用户名')).toBeVisible()
    await expect(newPage.getByRole('heading', { name: '我的文件', exact: true })).toHaveCount(0)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})
