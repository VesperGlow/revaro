import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const IMAGE_ID = 'copy-reference-image'
const TARGET_ID = 'copy-reference-target'
const STAMP = '2026-01-01T00:00:00Z'

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
const image = {
  id: IMAGE_ID,
  parent_id: ROOT,
  name: '复制源.png',
  kind: 'file',
  size: 4096,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'image/png',
  etag: 'copy-source-etag',
}
const target = {
  id: TARGET_ID,
  parent_id: ROOT,
  name: '复制目标',
  kind: 'directory',
  size: 0,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: '',
}

const picture = '<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="800"><rect width="1200" height="800" fill="#789"/><circle cx="840" cy="240" r="120" fill="#eddfb8"/></svg>'

async function mockCopy(
  page: Page,
  outcome: 'success' | 'conflict' | 'failure' = 'success',
  delayRefresh = false,
  emptyResponse = false,
) {
  let copied: Record<string, unknown> | null = null
  let releaseRefresh!: () => void
  const refreshGate = new Promise<void>(resolve => { releaseRefresh = resolve })
  const copyBodies: Array<{ parent_id?: string }> = []
  await page.route('**/api/**', async route => {
    const request = route.request()
    const pathname = new URL(request.url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (pathname === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (pathname === '/api/events' || pathname === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (pathname === '/api/tasks') return json({ items: [] })
    if (pathname === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: copied ? 2 : 1 } })
    }
    if (pathname === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: copied ? 2 : 1 })
    if (pathname === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (pathname === `/api/files/${ROOT}/children`) {
      if (copied && delayRefresh) await refreshGate
      return json({
        items: copied ? [image, target, copied] : [image, target],
        total_bytes: image.size + (copied ? image.size : 0),
        file_count: copied ? 2 : 1,
      })
    }
    if (pathname === `/api/files/${TARGET_ID}`) return json({ file: target, breadcrumbs: [root] })
    if (pathname === `/api/files/${TARGET_ID}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    if (pathname.endsWith('/thumbnail') || pathname.endsWith('/preview')) {
      return route.fulfill({ contentType: 'image/svg+xml', body: picture })
    }
    if (pathname === `/api/files/${IMAGE_ID}/copy` && request.method() === 'POST') {
      const body = request.postDataJSON() as { parent_id?: string }
      copyBodies.push(body)
      if (outcome !== 'success') {
        return route.fulfill({
          status: outcome === 'conflict' ? 409 : 500,
          contentType: 'application/json',
          body: JSON.stringify({
            error: {
              status: outcome === 'conflict' ? 409 : 500,
              message: outcome === 'conflict' ? 'an item with that name already exists' : 'copy failed',
            },
          }),
        })
      }
      copied = { ...image, id: 'copy-reference-result', parent_id: body.parent_id, name: '复制源 (副本).png' }
      return route.fulfill({ status: 201, json: emptyResponse ? {} : copied })
    }
    return json({ items: [] })
  })
  return { copyBodies, copied: () => copied, releaseRefresh: () => releaseRefresh() }
}

async function openCopyDialog(
  page: Page,
  baseUrl: string,
  outcome: 'success' | 'conflict' | 'failure',
  delayRefresh = false,
  submit = true,
  emptyResponse = false,
) {
  const mock = await mockCopy(page, outcome, delayRefresh, emptyResponse)
  await page.goto(`${baseUrl}/?copy-reference=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.locator('.file-card').filter({ hasText: image.name }).click()
  await expect(page.locator('.preview-modal')).toBeVisible()
  const menu = page.locator('.preview-commandbar .preview-menu')
  await menu.locator('summary').click()
  await menu.getByRole('button', { name: '复制', exact: true }).click()
  await expect(page.locator('.preview-modal')).toHaveCount(0)
  await expect(page.locator('.move-copy-dialog')).toBeVisible()

  await page.locator('.directory-trigger').click()
  const picker = page.getByRole('region', { name: '选择目标目录' })
  await picker.getByRole('button', { name: target.name, exact: true }).click()
  await expect(page.locator('.directory-trigger')).toHaveAttribute('title', new RegExp(target.name))
  // The reference keeps the directory flyout open after changing its target;
  // close that nested disclosure through its Escape path before submitting.
  await page.keyboard.press('Escape')
  await expect(picker).toHaveCount(0)
  if (submit) await page.locator('.move-copy-dialog').getByRole('button', { name: '复制', exact: true }).click()
  return mock
}

async function exercise(page: Page, baseUrl: string) {
  const mock = await openCopyDialog(page, baseUrl, 'success')
  await expect(page.locator('.move-copy-dialog')).toHaveCount(0)
  await expect(page.locator('.toast')).toHaveText('已复制 1 项')
  await expect(page.locator('.file-card').filter({ hasText: '复制源 (副本).png' })).toBeVisible()

  return {
    copyBodies: mock.copyBodies,
    copiedParent: mock.copied()?.parent_id,
    previewCount: await page.locator('.preview-modal').count(),
    transferCount: await page.locator('.move-copy-dialog').count(),
    toast: await page.locator('.toast').textContent(),
  }
}

test('old/new 媒体更多菜单的复制入口完成目标目录复制并刷新列表', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({
      copyBodies: [{ parent_id: TARGET_ID }],
      copiedParent: TARGET_ID,
      previewCount: 0,
      transferCount: 0,
      toast: '已复制 1 项',
    })
    expect(newResult, 'Rust 媒体复制入口、目标选择或完成反馈与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 复制成功响应缺少文件字段时仍保留成功反馈', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    const mock = await openCopyDialog(page, baseUrl, 'success', false, true, true)
    await expect(page.locator('.move-copy-dialog')).toHaveCount(0)
    await expect(page.locator('.toast')).toHaveText('已复制 1 项')
    await expect(page.locator('.file-card').filter({ hasText: '复制源 (副本).png' })).toBeVisible()
    return {
      copyBodies: mock.copyBodies,
      copiedParent: mock.copied()?.parent_id,
      toast: await page.locator('.toast').textContent(),
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({
      copyBodies: [{ parent_id: TARGET_ID }],
      copiedParent: TARGET_ID,
      toast: '已复制 1 项',
    })
    expect(newResult, 'Rust 复制成功响应缺字段时未保留 reference 反馈').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('媒体复制成功反馈等待 reference 的目录刷新完成', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    const [oldMock, newMock] = await Promise.all([
      openCopyDialog(oldPage, oldUrl, 'success', true, false),
      openCopyDialog(newPage, newUrl, 'success', true, false),
    ])
    await Promise.all([
      oldPage.locator('.move-copy-dialog').getByRole('button', { name: '复制', exact: true }).click(),
      newPage.locator('.move-copy-dialog').getByRole('button', { name: '复制', exact: true }).click(),
    ])
    await Promise.all([
      expect(oldPage.locator('.move-copy-dialog')).toHaveCount(0),
      expect(newPage.locator('.move-copy-dialog')).toHaveCount(0),
    ])
    await Promise.all([oldPage.waitForTimeout(120), newPage.waitForTimeout(120)])
    expect(await oldPage.locator('.toast').count()).toBe(0)
    expect(await newPage.locator('.toast').count()).toBe(0)
    expect(await oldPage.locator('.file-card').filter({ hasText: '复制源 (副本).png' }).count()).toBe(0)
    expect(await newPage.locator('.file-card').filter({ hasText: '复制源 (副本).png' }).count()).toBe(0)

    oldMock.releaseRefresh()
    newMock.releaseRefresh()
    await Promise.all([
      expect(oldPage.locator('.toast')).toHaveText('已复制 1 项'),
      expect(newPage.locator('.toast')).toHaveText('已复制 1 项'),
      expect(oldPage.locator('.file-card').filter({ hasText: '复制源 (副本).png' })).toBeVisible(),
      expect(newPage.locator('.file-card').filter({ hasText: '复制源 (副本).png' })).toBeVisible(),
    ])
    const oldResult = {
      copyBodies: oldMock.copyBodies,
      copiedParent: oldMock.copied()?.parent_id,
      toast: await oldPage.locator('.toast').textContent(),
    }
    const newResult = {
      copyBodies: newMock.copyBodies,
      copiedParent: newMock.copied()?.parent_id,
      toast: await newPage.locator('.toast').textContent(),
    }
    expect(oldResult).toEqual({ copyBodies: [{ parent_id: TARGET_ID }], copiedParent: TARGET_ID, toast: '已复制 1 项' })
    expect(newResult, 'Rust 复制成功反馈不应早于 reference 的目录刷新').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

for (const outcome of ['conflict', 'failure'] as const) {
  test(`媒体复制 ${outcome} 时保持 reference 的失败反馈和清理行为`, async ({ browser }) => {
    const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
    const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
    const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
    const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
    const oldPage = await oldContext.newPage()
    const newPage = await newContext.newPage()

    async function run(page: Page, baseUrl: string) {
      const mock = await openCopyDialog(page, baseUrl, outcome)
      await expect(page.locator('.move-copy-dialog')).toHaveCount(0)
      await expect(page.locator('.toast')).toHaveText(
        outcome === 'conflict'
          ? '已复制 0 项，1 项失败：复制源.png：an item with that name already exists'
          : '已复制 0 项，1 项失败：复制源.png：copy failed',
      )
      return {
        copyBodies: mock.copyBodies,
        toast: await page.locator('.toast').textContent(),
        toastClass: await page.locator('.toast').getAttribute('class'),
        sourceVisible: await page.locator('.file-card').filter({ hasText: image.name }).count(),
        resultVisible: await page.locator('.file-card').filter({ hasText: '复制源 (副本).png' }).count(),
      }
    }

    try {
      const [oldResult, newResult] = await Promise.all([
        run(oldPage, oldUrl),
        run(newPage, newUrl),
      ])
      expect(oldResult).toEqual({
        copyBodies: [{ parent_id: TARGET_ID }],
        toast: outcome === 'conflict'
          ? '已复制 0 项，1 项失败：复制源.png：an item with that name already exists'
          : '已复制 0 项，1 项失败：复制源.png：copy failed',
        toastClass: 'toast error',
        sourceVisible: 1,
        resultVisible: 0,
      })
      expect(newResult, `Rust 复制 ${outcome} 的反馈或清理与 reference 不一致`).toEqual(oldResult)
    } finally {
      await Promise.all([oldContext.close(), newContext.close()])
    }
  })
}
