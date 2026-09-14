import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

const files = [
  { id: 'focus-folder', parent_id: ROOT, name: '焦点目录', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' },
  { id: 'focus-text', parent_id: ROOT, name: '焦点文档.txt', kind: 'file', size: 100, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: 'text/plain' },
]

const status = {
  status: 'ok',
  database: { status: 'ok', bytes: 1024 },
  storage: { status: 'ok', bytes: 2048, trash_bytes: 0, file_count: files.length },
  cache: { status: 'ok', memory_bytes: 1024, disk_bytes: 2048, memory_entries: 1, disk_entries: 1, classes: {} },
}

async function mockShell(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events') return route.fulfill({ contentType: 'text/event-stream', body: '' })
    if (path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: `event: status\ndata: ${JSON.stringify(status)}\n\n` })
    }
    if (path === '/api/tasks') return json({ items: [] })
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
    if (path === `/api/files/${ROOT}/children`) return json({ items: files, total_bytes: 100, file_count: 1 })
    return json({ items: [] })
  })
}

async function openShell(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await expect(page.locator('.file-card')).toHaveCount(files.length)
  await page.waitForTimeout(180)
}

async function activeDescriptor(page: Page) {
  return page.evaluate(() => {
    const element = document.activeElement
    if (!element || element === document.body) return 'BODY'
    const style = getComputedStyle(element)
    const rect = element.getBoundingClientRect()
    if (style.display === 'none' || style.visibility === 'hidden' || rect.width === 0 || rect.height === 0) return 'HIDDEN'
    const text = (element.textContent ?? '').replace(/\s+/g, ' ').trim().slice(0, 60)
    return {
      tag: element.tagName.toLowerCase(),
      title: element.getAttribute('title'),
      text,
      className: Array.from(element.classList).sort(),
    }
  })
}

async function tabSequence(page: Page, count: number) {
  const sequence = []
  for (let index = 0; index < count; index += 1) {
    await page.keyboard.press('Tab')
    sequence.push(await activeDescriptor(page))
  }
  return sequence
}

test('桌面全局 Tab 焦点顺序保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  await oldContext.addInitScript(() => {
    localStorage.removeItem('revaro:sidebar:collapsed')
    localStorage.removeItem('revaro:sidebar:expanded')
    localStorage.removeItem('revaro:view-mode')
  })
  await newContext.addInitScript(() => {
    localStorage.removeItem('revaro:sidebar:collapsed')
    localStorage.removeItem('revaro:sidebar:expanded')
    localStorage.removeItem('revaro:view-mode')
  })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockShell(oldPage), mockShell(newPage)])
    await Promise.all([openShell(oldPage, oldUrl), openShell(newPage, newUrl)])
    await Promise.all([oldPage.evaluate(() => (document.activeElement as HTMLElement | null)?.blur()), newPage.evaluate(() => (document.activeElement as HTMLElement | null)?.blur())])

    const oldSequence = await tabSequence(oldPage, 24)
    const newSequence = await tabSequence(newPage, 24)
    expect(newSequence, 'Rust 桌面 Tab 焦点顺序与 reference 不一致').toEqual(oldSequence)
    expect(oldSequence).not.toContain('HIDDEN')
    expect(oldSequence).not.toContain('BODY')
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})
