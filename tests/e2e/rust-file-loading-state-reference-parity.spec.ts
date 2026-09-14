import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const FOLDER = 'loading-state-folder'
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

const folder = {
  id: FOLDER,
  parent_id: ROOT,
  name: '加载状态目录',
  kind: 'directory',
  size: 0,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: '',
}

const note = {
  id: 'loading-state-note',
  parent_id: ROOT,
  name: '旧内容.txt',
  kind: 'file',
  size: 1024,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'text/plain',
  etag: 'loading-state-note-etag',
}

type Scenario = 'normal' | 'empty' | 'folder-loading' | 'folder-error' | 'trash-error'

async function mockShell(page: Page, scenario: Scenario) {
  let releaseFolder = () => {}
  const folderGate = scenario === 'folder-loading'
    ? new Promise<void>(resolve => { releaseFolder = resolve })
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
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 1 } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 1 })
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) {
      return json({
        items: scenario === 'empty' ? [] : [folder, note],
        total_bytes: scenario === 'empty' ? 0 : note.size,
        file_count: scenario === 'empty' ? 0 : 1,
      })
    }
    if (path === `/api/files/${FOLDER}`) return json({ file: folder, breadcrumbs: [root] })
    if (path === `/api/files/${FOLDER}/children`) {
      if (scenario === 'folder-loading' && folderGate) await folderGate
      if (scenario === 'folder-error') {
        return route.fulfill({ status: 500, json: { error: { status: 500, message: '目录读取失败' } } })
      }
      return json({ items: [], total_bytes: 0, file_count: 0 })
    }
    if (path === '/api/trash') {
      if (scenario === 'trash-error') {
        return route.fulfill({ status: 500, json: { error: { status: 500, message: '回收站读取失败' } } })
      }
      return json({ items: [], total_bytes: 0, file_count: 0 })
    }
    return json({ items: [] })
  })
  return () => releaseFolder()
}

async function openRoot(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?loading-state-reference=${crypto.randomUUID()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
}

async function snapshot(page: Page) {
  return page.evaluate(() => ({
    heading: document.querySelector('.content-head h1')?.textContent?.trim() ?? null,
    state: document.querySelector('.content .state')?.className ?? null,
    stateText: document.querySelector('.content .state')?.textContent?.replace(/\s+/g, ' ').trim() ?? null,
    stateButtons: Array.from(document.querySelectorAll('.content .state button')).map(button => button.textContent?.trim()),
    cards: Array.from(document.querySelectorAll('.content .file-card')).map(card => card.textContent?.replace(/\s+/g, ' ').trim()),
    rows: Array.from(document.querySelectorAll('.content .file-row')).map(row => row.textContent?.replace(/\s+/g, ' ').trim()),
    toast: Array.from(document.querySelectorAll('.toast')).map(element => ({
      text: element.textContent?.replace(/\s+/g, ' ').trim(),
      className: element.className,
    })),
  }))
}

async function comparePages(oldPage: Page, newPage: Page, message: string) {
  expect(await snapshot(newPage), message).toEqual(await snapshot(oldPage))
}

test('文件浏览 loading、空态和普通失败反馈保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'

  for (const scenario of ['normal', 'empty', 'folder-loading', 'folder-error'] as const) {
    const oldContext = await browser.newContext({ viewport: { width: 390, height: 844 } })
    const newContext = await browser.newContext({ viewport: { width: 390, height: 844 } })
    const oldPage = await oldContext.newPage()
    const newPage = await newContext.newPage()
    try {
      const [releaseOld, releaseNew] = await Promise.all([
        mockShell(oldPage, scenario),
        mockShell(newPage, scenario),
      ])
      await Promise.all([openRoot(oldPage, oldUrl), openRoot(newPage, newUrl)])

      if (scenario === 'empty') {
        await Promise.all([
          expect(oldPage.locator('.state.empty')).toBeVisible(),
          expect(newPage.locator('.state.empty')).toBeVisible(),
        ])
        await comparePages(oldPage, newPage, 'Rust 空根状态与 reference 不一致')
        continue
      }

      await expect(oldPage.locator('.file-card')).toHaveCount(2)
      await expect(newPage.locator('.file-card')).toHaveCount(2)
      if (scenario === 'normal') {
        await comparePages(oldPage, newPage, 'Rust 普通根目录完成态与 reference 不一致')
        continue
      }

      await Promise.all([
        oldPage.locator('.file-card').filter({ hasText: folder.name }).click(),
        newPage.locator('.file-card').filter({ hasText: folder.name }).click(),
      ])
      if (scenario === 'folder-loading') {
        await Promise.all([
          expect(oldPage.locator('.content .state')).toContainText('正在读取文件…'),
          expect(newPage.locator('.content .state')).toContainText('正在读取文件…'),
        ])
        await comparePages(oldPage, newPage, 'Rust 目录切换 loading 态与 reference 不一致')
        releaseOld()
        releaseNew()
        await Promise.all([
          expect(oldPage.locator('.state.empty')).toBeVisible(),
          expect(newPage.locator('.state.empty')).toBeVisible(),
        ])
        await comparePages(oldPage, newPage, 'Rust 目录切换完成态与 reference 不一致')
      } else {
        await Promise.all([
          expect(oldPage.locator('.toast')).toContainText('目录读取失败'),
          expect(newPage.locator('.toast')).toContainText('目录读取失败'),
        ])
        await comparePages(oldPage, newPage, 'Rust 目录失败后的旧内容/Toast 与 reference 不一致')
        expect(await oldPage.locator('.content .state button').count()).toBe(0)
        expect(await newPage.locator('.content .state button').count()).toBe(0)
      }
    } finally {
      await Promise.all([oldContext.close(), newContext.close()])
    }
  }
})

test('回收站读取失败保留当前目录并只显示 reference Toast', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 390, height: 844 } })
  const newContext = await browser.newContext({ viewport: { width: 390, height: 844 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  try {
    await Promise.all([mockShell(oldPage, 'trash-error'), mockShell(newPage, 'trash-error')])
    await Promise.all([openRoot(oldPage, oldUrl), openRoot(newPage, newUrl)])
    await Promise.all([
      oldPage.locator('.sidebar-handle').click(),
      newPage.locator('.sidebar-handle').click(),
    ])
    await Promise.all([
      oldPage.locator('.app-sidebar .trash-entry').click(),
      newPage.locator('.app-sidebar .trash-entry').click(),
    ])
    await Promise.all([
      expect(oldPage.locator('.toast')).toContainText('回收站读取失败'),
      expect(newPage.locator('.toast')).toContainText('回收站读取失败'),
    ])
    await comparePages(oldPage, newPage, 'Rust 回收站失败后的目录内容/Toast 与 reference 不一致')
    expect(await oldPage.locator('.content .state button').count()).toBe(0)
    expect(await newPage.locator('.content .state button').count()).toBe(0)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})
