import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const FIRST = 'history-sequence-first'
const SECOND = 'history-sequence-second'
const STAMP = '2026-01-01T00:00:00Z'

const root = { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' }
const first = { id: FIRST, parent_id: ROOT, name: '一级目录', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' }
const second = { id: SECOND, parent_id: FIRST, name: '二级目录', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' }

type RequestMark = { path: string; at: number }

async function mockHistorySequence(page: Page, marks: RequestMark[]) {
  let phase: 'initial' | 'back' = 'initial'
  await page.exposeFunction('__beginHistorySequenceBack', () => {
    phase = 'back'
  })
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    marks.push({ path, at: Date.now() })
    const json = (value: unknown) => route.fulfill({ json: value })
    const delay = async (milliseconds: number) => {
      if (milliseconds > 0) await new Promise(resolve => setTimeout(resolve, milliseconds))
    }
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
      if (phase === 'back') await delay(0)
      return json({ file: root, breadcrumbs: [] })
    }
    if (path === `/api/files/${ROOT}/children`) {
      if (phase === 'back') await delay(0)
      return json({ items: [first], total_bytes: 0, file_count: 1 })
    }
    if (path === `/api/files/${FIRST}`) {
      if (phase === 'back') await delay(350)
      return json({ file: first, breadcrumbs: [root, first] })
    }
    if (path === `/api/files/${FIRST}/children`) {
      if (phase === 'back') await delay(350)
      return json({ items: [second], total_bytes: 0, file_count: 1 })
    }
    if (path === `/api/files/${SECOND}`) return json({ file: second, breadcrumbs: [root, first, second] })
    if (path === `/api/files/${SECOND}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    return json({ items: [] })
  })
}

async function openNestedPath(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?history-sequence-reference=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.locator('.file-card').filter({ hasText: first.name }).click()
  await expect(page.getByRole('heading', { name: first.name, exact: true })).toBeVisible()
  await page.locator('.file-card').filter({ hasText: second.name }).click()
  await expect(page.getByRole('heading', { name: second.name, exact: true })).toBeVisible()
}

async function snapshot(page: Page) {
  return page.evaluate(() => ({
    pathname: window.location.pathname,
    heading: document.querySelector('.content-head h1')?.textContent?.trim() ?? null,
    loading: document.querySelector('.state:not(.empty)')?.textContent?.replace(/\s+/g, ' ').trim() ?? null,
  }))
}

async function exercise(page: Page, baseUrl: string, marks: RequestMark[]) {
  await mockHistorySequence(page, marks)
  await openNestedPath(page, baseUrl)
  const before = await snapshot(page)
  const backStartedAt = Date.now()
  await page.evaluate(() => {
    void (window as unknown as { __beginHistorySequenceBack: () => void }).__beginHistorySequenceBack()
    // Two browser back events are issued in one turn, matching a user
    // rapidly pressing the browser back button twice.
    history.back()
    history.back()
  })
  await page.waitForTimeout(100)
  const intermediate = await snapshot(page)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible({ timeout: 5_000 })
  const settled = await snapshot(page)
  return {
    before,
    intermediate,
    settled,
    requestsAfterBack: marks
      .filter(mark => mark.at >= backStartedAt && mark.path.startsWith('/api/files/'))
      .map(mark => mark.path),
  }
}

test('快速连续浏览器后退按 reference 顺序逐级完成目录恢复', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const oldMarks: RequestMark[] = []
  const newMarks: RequestMark[] = []

  try {
    const [oldState, newState] = await Promise.all([
      exercise(oldPage, oldUrl, oldMarks),
      exercise(newPage, newUrl, newMarks),
    ])
    expect(oldState.before).toEqual({ pathname: `/f/${SECOND}`, heading: second.name, loading: null })
    expect(oldState.intermediate).toEqual({ pathname: '/', heading: second.name, loading: '正在读取文件…' })
    expect(oldState.settled).toEqual({ pathname: '/', heading: root.name, loading: null })
    expect(newState, 'Rust 快速连续后退的中间/最终目录状态或请求顺序与 reference 不一致').toEqual(oldState)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})
