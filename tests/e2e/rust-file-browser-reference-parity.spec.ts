import { expect, test, type BrowserContext, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

const file = (value: Record<string, unknown>) => ({
  parent_id: ROOT,
  kind: 'file',
  size: 1024,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'text/plain',
  etag: `etag-${String(value.id ?? 'item')}`,
  ...value,
})

const root = file({ id: ROOT, name: '我的文件', kind: 'directory', parent_id: null, size: 0, mime_type: '' })
const rootItems = [
  file({ id: 'folder', name: '资料', kind: 'directory', size: 0, mime_type: '' }),
  file({ id: 'note', name: '说明.txt', mime_type: 'text/plain', size: 2048 }),
]
const trashItems = [
  file({ id: 'deleted-note', name: '已删除.txt', deleted_at: STAMP, size: 512 }),
]

async function mockBrowser(page: Page, mode: 'normal' | 'empty' | 'error' | 'sparse-missing' | 'sparse-null' | 'nullable-breadcrumbs' | 'nullable-trash-items', childrenDelayMs = 0) {
  let releaseChildren = () => {}
  const childrenGate = childrenDelayMs
    ? new Promise<void>(resolve => { releaseChildren = resolve })
    : null
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: rootItems.length } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: rootItems.length })
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: mode === 'nullable-breadcrumbs' ? null : [] })
    if (path === `/api/files/${ROOT}/children`) {
      if (mode === 'error') {
        return route.fulfill({ status: 500, json: { error: { status: 500, message: '模拟读取失败' } } })
      }
      const items = mode === 'empty' ? [] : rootItems
      if (mode === 'sparse-missing') return json({ items })
      if (mode === 'sparse-null') return json({ items, total_bytes: null, file_count: null })
      return json({ items, total_bytes: mode === 'empty' ? 0 : 3072, file_count: mode === 'empty' ? 0 : 1 })
    }
    if (path === '/api/files/folder') return json({ file: rootItems[0], breadcrumbs: [root] })
    if (path === '/api/files/folder/children') {
      if (childrenGate) await childrenGate
      return json({ items: [], total_bytes: 0, file_count: 0 })
    }
    if (path === '/api/trash') {
      if (mode === 'nullable-trash-items') return json({ items: null, total_bytes: 0, file_count: 0 })
      if (mode === 'sparse-missing') return json({ items: trashItems })
      if (mode === 'sparse-null') return json({ items: trashItems, total_bytes: null, file_count: null })
      return json({ items: mode === 'empty' ? [] : trashItems, total_bytes: mode === 'empty' ? 0 : 512, file_count: mode === 'empty' ? 0 : 1 })
    }
    return json({ items: [] })
  })
  return childrenDelayMs ? releaseChildren : undefined
}

test('目录切换期间的 loading 文案和完成后的内容保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    const [releaseOld, releaseNew] = await Promise.all([
      mockBrowser(oldPage, 'normal', 2_000),
      mockBrowser(newPage, 'normal', 2_000),
    ])
    await Promise.all([openRoot(oldPage, oldUrl), openRoot(newPage, newUrl)])
    const oldChildrenRequest = oldPage.waitForRequest(request => request.url().includes('/api/files/folder/children'))
    const newChildrenRequest = newPage.waitForRequest(request => request.url().includes('/api/files/folder/children'))
    await Promise.all([
      oldPage.locator('.file-card').filter({ hasText: '资料' }).click(),
      newPage.locator('.file-card').filter({ hasText: '资料' }).click(),
    ])
    await Promise.all([oldChildrenRequest, newChildrenRequest])
    await Promise.all([
      expect(oldPage.locator('.content .state')).toContainText('正在读取文件…'),
      expect(newPage.locator('.content .state')).toContainText('正在读取文件…'),
    ])
    expect(await contentSnapshot(newPage), 'Rust 首次目录 loading 与 reference 不一致')
      .toEqual(await contentSnapshot(oldPage))

    releaseOld?.()
    releaseNew?.()
    await Promise.all([
      expect(oldPage.locator('.state.empty')).toBeVisible(),
      expect(newPage.locator('.state.empty')).toBeVisible(),
    ])
    expect(await contentSnapshot(newPage), 'Rust 首次目录完成态与 reference 不一致')
      .toEqual(await contentSnapshot(oldPage))
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

async function clearPreferences(context: BrowserContext) {
  await context.addInitScript(() => localStorage.removeItem('revaro:library:media:file'))
}

async function openRoot(page: Page, url: string) {
  await page.goto(`${url}/?compat-file-browser=${crypto.randomUUID()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
}

async function headerSnapshot(page: Page) {
  return page.locator('.content-head').evaluate(element => ({
    title: element.querySelector('h1')?.textContent?.trim(),
    titleClass: element.querySelector('h1')?.className,
    breadcrumb: element.querySelector('nav.breadcrumbs')?.textContent?.replace(/\s+/g, ' ').trim() ?? null,
    meta: element.querySelector('.folder-meta')?.textContent?.replace(/\s+/g, ' ').trim(),
    actions: Array.from(element.querySelectorAll(':scope > .actions button, .file-view-switch button')).map(button => ({
      text: button.textContent?.replace(/\s+/g, ' ').trim(),
      className: button.className,
      title: button.getAttribute('title'),
      pressed: button.getAttribute('aria-pressed'),
      disabled: (button as HTMLButtonElement).disabled,
    })),
  }))
}

async function contentSnapshot(page: Page) {
  return page.locator('.content').evaluate(element => ({
    state: element.querySelector('.state')?.className ?? null,
    stateText: element.querySelector('.state')?.textContent?.replace(/\s+/g, ' ').trim() ?? null,
    cards: Array.from(element.querySelectorAll('.file-card')).map(card => ({
      name: card.querySelector('.card-info strong')?.textContent?.trim(),
      className: Array.from(card.classList).sort().join(' '),
      meta: card.querySelector('.card-info small')?.textContent?.replace(/\s+/g, ' ').trim(),
    })),
    rows: Array.from(element.querySelectorAll('.file-row')).map(row => ({
      name: row.querySelector('.row-info strong')?.textContent?.trim(),
      className: Array.from(row.classList).sort().join(' '),
      meta: row.querySelector('.row-info small')?.textContent?.replace(/\s+/g, ' ').trim(),
    })),
  }))
}

async function toastSnapshot(page: Page) {
  return page.locator('.toast').evaluateAll(elements => elements.map(element => ({
    text: element.textContent?.replace(/\s+/g, ' ').trim(),
    className: element.className,
  })))
}

test('old/new 文件详情 breadcrumbs 为 null 的旧版缺陷与 Rust 容错保持可见', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([
      mockBrowser(oldPage, 'nullable-breadcrumbs'),
      mockBrowser(newPage, 'nullable-breadcrumbs'),
    ])
    await Promise.all([
      oldPage.goto(`${oldUrl}/?compat-file-browser=${crypto.randomUUID()}`),
      newPage.goto(`${newUrl}/?compat-file-browser=${crypto.randomUUID()}`),
    ])
    await Promise.all([
      expect(oldPage.locator('.file-card')).toHaveCount(rootItems.length),
      expect(newPage.locator('.file-card')).toHaveCount(rootItems.length),
      expect(newPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible(),
    ])
    expect(await oldPage.getByRole('heading', { name: '我的文件', exact: true }).count())
      .toBe(0)
    expect(await newPage.getByRole('heading', { name: '我的文件', exact: true }).count())
      .toBe(1)
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('old/new 回收站 items 为 null 的旧版渲染缺陷保持可见', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([
      mockBrowser(oldPage, 'nullable-trash-items'),
      mockBrowser(newPage, 'nullable-trash-items'),
    ])
    await Promise.all([
      oldPage.goto(`${oldUrl}/?compat-file-browser=${crypto.randomUUID()}`),
      newPage.goto(`${newUrl}/?compat-file-browser=${crypto.randomUUID()}`),
    ])
    await Promise.all([
      oldPage.getByTitle('回收站').first().click(),
      newPage.getByTitle('回收站').first().click(),
    ])
    await expect(newPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    expect(await oldPage.getByRole('heading', { name: '回收站', exact: true }).count()).toBe(0)
    expect(await oldPage.locator('body').innerText()).toBe('')
    expect(await newPage.getByRole('heading', { name: '回收站', exact: true }).count()).toBe(0)
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('根目录、列表/方块和回收站内容头保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  await clearPreferences(oldContext)
  await clearPreferences(newContext)
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockBrowser(oldPage, 'normal'), mockBrowser(newPage, 'normal')])
    await Promise.all([openRoot(oldPage, oldUrl), openRoot(newPage, newUrl)])
    await expect(oldPage.locator('.file-card')).toHaveCount(rootItems.length)
    await expect(newPage.locator('.file-card')).toHaveCount(rootItems.length)
    expect(await headerSnapshot(newPage), 'Rust 根目录内容头与 reference 不一致').toEqual(await headerSnapshot(oldPage))
    expect(await contentSnapshot(newPage), 'Rust 根目录方块内容与 reference 不一致').toEqual(await contentSnapshot(oldPage))

    await oldPage.getByTitle('列表视图').click()
    await newPage.getByTitle('列表视图').click()
    expect(await headerSnapshot(newPage), 'Rust 列表模式内容头与 reference 不一致').toEqual(await headerSnapshot(oldPage))
    expect(await contentSnapshot(newPage), 'Rust 列表内容与 reference 不一致').toEqual(await contentSnapshot(oldPage))

    await oldPage.getByTitle('回收站').first().click()
    await newPage.getByTitle('回收站').first().click()
    await expect(oldPage.getByRole('heading', { name: '回收站', exact: true })).toBeVisible()
    await expect(newPage.getByRole('heading', { name: '回收站', exact: true })).toBeVisible()
    expect(await headerSnapshot(newPage), 'Rust 回收站内容头与 reference 不一致').toEqual(await headerSnapshot(oldPage))
    expect(await contentSnapshot(newPage), 'Rust 回收站列表内容与 reference 不一致').toEqual(await contentSnapshot(oldPage))

    await oldPage.getByRole('button', { name: '返回我的文件', exact: true }).click()
    await newPage.getByRole('button', { name: '返回我的文件', exact: true }).click()
    await expect(oldPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await expect(newPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    expect(await headerSnapshot(newPage), 'Rust 返回根目录后的内容头与 reference 不一致').toEqual(await headerSnapshot(oldPage))
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('空目录与目录读取失败保留 reference 的页面结构和反馈', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const cases: Array<'empty' | 'error'> = ['empty', 'error']

  for (const mode of cases) {
    const oldContext = await browser.newContext({ viewport: { width: 390, height: 844 } })
    const newContext = await browser.newContext({ viewport: { width: 390, height: 844 } })
    await clearPreferences(oldContext)
    await clearPreferences(newContext)
    const oldPage = await oldContext.newPage()
    const newPage = await newContext.newPage()
    try {
      await Promise.all([mockBrowser(oldPage, mode), mockBrowser(newPage, mode)])
      await Promise.all([openRoot(oldPage, oldUrl), openRoot(newPage, newUrl)])
      await expect(oldPage.locator('.state.empty')).toBeVisible()
      await expect(newPage.locator('.state.empty')).toBeVisible()
      expect(await headerSnapshot(newPage), `${mode} Rust 内容头与 reference 不一致`).toEqual(await headerSnapshot(oldPage))
      expect(await contentSnapshot(newPage), `${mode} Rust 空/错误内容态与 reference 不一致`).toEqual(await contentSnapshot(oldPage))
      if (mode === 'error') {
        await expect(oldPage.locator('.toast')).toContainText('模拟读取失败')
        await expect(newPage.locator('.toast')).toContainText('模拟读取失败')
        expect(await toastSnapshot(newPage), 'Rust 目录失败反馈与 reference 不一致').toEqual(await toastSnapshot(oldPage))
      }
    } finally {
      await oldContext.close()
      await newContext.close()
    }
  }
})

test('old/new 文件列表在 children 统计缺失或为 null 时仍保留条目', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18180'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18184'
  for (const mode of ['sparse-missing', 'sparse-null'] as const) {
    const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
    const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
    const oldPage = await oldContext.newPage()
    const newPage = await newContext.newPage()
    try {
      await Promise.all([mockBrowser(oldPage, mode), mockBrowser(newPage, mode)])
      await Promise.all([openRoot(oldPage, oldUrl), openRoot(newPage, newUrl)])
      await expect(oldPage.locator('.file-card')).toHaveCount(rootItems.length)
      await expect(newPage.locator('.file-card'), `Rust ${mode} 不应因未使用统计字段丢失整个列表`).toHaveCount(rootItems.length)
      expect(await contentSnapshot(newPage), `Rust ${mode} 条目与 reference 不一致`).toEqual(await contentSnapshot(oldPage))

      await oldPage.getByTitle('回收站').first().click()
      await newPage.getByTitle('回收站').first().click()
      await expect(oldPage.locator('.file-card')).toHaveCount(trashItems.length)
      await expect(newPage.locator('.file-card'), `Rust ${mode} 不应因回收站统计字段丢失条目`).toHaveCount(trashItems.length)
      expect(await contentSnapshot(newPage), `Rust ${mode} 回收站条目与 reference 不一致`).toEqual(await contentSnapshot(oldPage))
    } finally {
      await oldContext.close()
      await newContext.close()
    }
  }
})
