import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

const task = {
  id: 'global-ui-active-task',
  type: 'upload',
  status: 'running',
  phase: '上传中',
  progress: 42,
  speed: 0,
  retry_count: 0,
  max_retries: 3,
  error: '',
  source_type: '',
  source_id: '',
  cancel_requested: false,
  created_at: STAMP,
  updated_at: STAMP,
  finished_at: STAMP,
  name: '活动任务.bin',
}

const status = {
  status: 'degraded',
  database: { status: 'ok', bytes: 1024 },
  storage: { status: 'degraded', bytes: 2048, trash_bytes: 512, file_count: 1 },
  cache: {
    status: 'critical',
    memory_bytes: 1024,
    disk_bytes: 2048,
    memory_entries: 1,
    disk_entries: 2,
    classes: { default: { hits: 2, misses: 1, loads: 1, load_errors: 0, evictions: 0 } },
  },
}

async function mockShell(page: Page, statusValue = status) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events') return route.fulfill({ contentType: 'text/event-stream', body: '' })
    if (path === '/api/system/status/stream') {
      return route.fulfill({
        contentType: 'text/event-stream',
        body: `event: status\ndata: ${JSON.stringify(statusValue)}\n\n`,
      })
    }
    if (path === '/api/tasks') return json({ items: [task] })
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
    return json({ items: [] })
  })
}

async function mockShellWithReconnect(page: Page) {
  let streamRequests = 0
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events') return route.fulfill({ contentType: 'text/event-stream', body: '' })
    if (path === '/api/system/status/stream') {
      streamRequests += 1
      const statusValue = streamRequests === 1 ? status : { ...status, status: 'ok' }
      return route.fulfill({
        contentType: 'text/event-stream',
        body: `event: status\ndata: ${JSON.stringify(statusValue)}\n\n`,
      })
    }
    if (path === '/api/tasks') return json({ items: [task] })
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
    return json({ items: [] })
  })
  return () => streamRequests
}

async function openShell(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
}

async function badgeMetrics(page: Page, selector: string) {
  return page.locator(selector).evaluateAll(elements => elements.map(element => {
    const style = getComputedStyle(element)
    const rect = element.getBoundingClientRect()
    return {
      className: element.className,
      width: rect.width,
      height: rect.height,
      fontSize: style.fontSize,
      padding: style.padding,
      minHeight: style.minHeight,
    }
  }))
}

test('任务中心和系统状态 badge 保持 reference 尺寸与视觉层级', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockShell(oldPage), mockShell(newPage)])
    await Promise.all([openShell(oldPage, oldUrl), openShell(newPage, newUrl)])

    await oldPage.getByTitle('任务中心').click()
    await newPage.getByTitle('任务中心').click()
    const taskSelector = '.task-panel > header .status-badge'
    await expect(oldPage.locator(taskSelector)).toBeVisible()
    await expect(newPage.locator(taskSelector)).toBeVisible()
    expect(await badgeMetrics(newPage, taskSelector)).toEqual(await badgeMetrics(oldPage, taskSelector))

    await oldPage.getByTitle('任务中心').click()
    await newPage.getByTitle('任务中心').click()
    await oldPage.locator('.system-status > summary').click()
    await newPage.locator('.system-status > summary').click()
    const serviceSelector = '.status-grid .service-card .status-badge'
    await expect(oldPage.locator(serviceSelector)).toHaveCount(3)
    await expect(newPage.locator(serviceSelector)).toHaveCount(3)
    expect(await badgeMetrics(newPage, serviceSelector)).toEqual(await badgeMetrics(oldPage, serviceSelector))
    expect(await newPage.locator('.status-grid .service-card').nth(2).innerText())
      .toBe(await oldPage.locator('.status-grid .service-card').nth(2).innerText())
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('系统状态保留 reference 的未知/critical 顶层状态 class', async ({ page }) => {
  await mockShell(page, { ...status, status: 'critical' })
  await openShell(page, process.env.E2E_BASE_URL || 'http://127.0.0.1:18080')
  await expect(page.locator('.system-status')).toHaveClass(/system-status critical/)
})

test('系统状态 Escape 保留 reference 的默认事件与 summary 焦点行为', async ({ page }) => {
  await mockShell(page)
  await openShell(page, process.env.E2E_BASE_URL || 'http://127.0.0.1:18080')
  await page.locator('.system-status > summary').click()
  await page.evaluate(() => {
    ;(window as Window & { __escapeDefaultPrevented?: boolean }).__escapeDefaultPrevented = undefined
    window.addEventListener('keydown', event => {
      if (event.key === 'Escape') {
        ;(window as Window & { __escapeDefaultPrevented?: boolean }).__escapeDefaultPrevented = event.defaultPrevented
      }
    }, { once: true })
  })
  await page.keyboard.press('Escape')
  await expect(page.locator('.system-status')).not.toHaveAttribute('open')
  expect(await page.evaluate(() => (window as Window & { __escapeDefaultPrevented?: boolean }).__escapeDefaultPrevented)).toBe(false)
  expect(await page.locator('.system-status > summary').evaluate(element => document.activeElement === element)).toBe(true)
})

test('系统状态 SSE 断线按 reference 重连并更新顶层状态', async ({ page }) => {
  const streamRequests = await mockShellWithReconnect(page)
  await openShell(page, process.env.E2E_BASE_URL || 'http://127.0.0.1:18080')
  await expect(page.locator('.system-status')).toHaveClass(/system-status degraded/)
  await expect.poll(streamRequests, { timeout: 5_000 }).toBeGreaterThan(1)
  await expect(page.locator('.system-status')).toHaveClass(/system-status ok/)
  await expect(page.locator('.system-status')).toContainText('所有服务正常')
})

test('系统状态在退出登录时清理 EventSource 与重连定时器', async ({ page }) => {
  const streamRequests = await mockShellWithReconnect(page)
  await openShell(page, process.env.E2E_BASE_URL || 'http://127.0.0.1:18080')
  await expect(page.locator('.system-status')).toHaveClass(/system-status degraded/)
  await page.locator('button[title="打开账户设置"]').click()
  const account = page.locator('.account-modal')
  await expect(account).toBeVisible()
  await account.getByRole('button', { name: '退出登录', exact: true }).click()
  await expect(page.getByRole('heading', { name: '登录私人空间' })).toBeVisible()
  const requestsAfterLogout = streamRequests()
  await page.waitForTimeout(1_300)
  expect(streamRequests()).toBe(requestsAfterLogout)
})
